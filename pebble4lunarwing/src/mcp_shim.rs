//! `mcp_shim.rs` — thin stdio MCP server exposing the tenant's allowlisted
//! hub tools (WI#324 design item 2) to `pebble` (pebble lane, ported
//! from `kimicode4lunarwing/scripts/lunarwing_mcp_server.ts`).
//!
//! Pebble spawns this process per task session via the `mcpServers.lunarwing`
//! entry in the baked `$PEBBLE_CONFIG_HOME/settings.json` and speaks MCP over
//! stdio (newline-delimited JSON-RPC 2.0). Each request is relayed over the
//! per-task unix socket the bridge created (`LUNARWING_TOOL_PROXY_SOCKET`),
//! tagged with the job id (`LUNARWING_TASK_ID`); the bridge's tool-proxy relay
//! forwards it as a `tool_list` / `tool_call` frame on the hub WebSocket and
//! routes the reply back.
//!
//! Without the proxy env (cli mode, non-task runs) the server stays
//! well-behaved and advertises zero tools; the hub stays deny-by-default
//! without a `tool_allowlist` grant either way.
//!
//! Tool surface: whatever the hub grants the worker (deny-by-default).
//! MCP clients see them as `mcp__lunarwing__<name>`.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::protocol;
use crate::tool_proxy;

const SERVER_NAME: &str = "lunarwing-tool-proxy";
const SERVER_VERSION: &str = "1.0.0";
/// Echoed back when the client proposes a version; we speak the tools-only
/// subset common to all MCP revisions.
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";
/// Inbound line cap. The kimi shim counts UTF-16 code units; a UTF-8 byte
/// cap is strictly tighter for non-ASCII and bounds memory the same way
/// (MR !1549 EF-9).
const MAX_STDIN_LINE_BYTES: usize = 1024 * 1024;
/// MR !1561 EV-3: an over-long request line must still be answered (exactly
/// once, with its id) — otherwise the client waits out its own MCP timeout.
/// The middle of the line is discarded (the 1 MiB bound stands); the head
/// and a rolling tail are kept so the id can be recovered from the line's
/// top-level edges: id-first (`{"jsonrpc":"2.0","id":N,`) or SDK order
/// (`…,"jsonrpc":"2.0","id":N}`). A nested "id" is never used.
const EDGE_KEEP_BYTES: usize = 256;

const DEFAULT_PROXY_TIMEOUT_MS: u64 = 300_000;
const MAX_PROXY_TIMEOUT_MS: u64 = 900_000;
const MIN_PROXY_TIMEOUT_MS: u64 = 1_000;

fn request_timeout_from_env() -> Duration {
    let ms = std::env::var("LUNARWING_TOOL_PROXY_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(DEFAULT_PROXY_TIMEOUT_MS, |v| {
            v.clamp(MIN_PROXY_TIMEOUT_MS, MAX_PROXY_TIMEOUT_MS)
        });
    Duration::from_millis(ms)
}

/// Where the per-task relay lives, from the env the bridge's executor
/// injects into the pebble process (inherited by this MCP child). The task
/// id is required to be present (the relay socket is bound to it) but is
/// not read back: the per-task socket is the binding.
pub struct ShimConfig {
    pub socket_path: String,
    pub timeout: Duration,
}

impl ShimConfig {
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let socket_path = std::env::var(tool_proxy::PROXY_SOCKET_ENV).ok()?;
        let task_id = std::env::var(tool_proxy::TASK_ID_ENV).ok()?;
        if socket_path.is_empty() || task_id.is_empty() {
            return None;
        }
        Some(Self {
            socket_path,
            timeout: request_timeout_from_env(),
        })
    }
}

/// Parsed shape of one relay IPC reply.
#[derive(Debug, Default, Clone)]
pub struct RelayReply {
    pub ok: bool,
    pub tools: Option<Value>,
    pub status: Option<String>,
    pub output: Option<String>,
    pub error: Option<String>,
}

impl RelayReply {
    fn failed(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(message.into()),
            ..Self::default()
        }
    }

    fn parse(value: &Value) -> Self {
        Self {
            ok: value.get("ok").and_then(Value::as_bool).unwrap_or(false),
            tools: value.get("tools").cloned(),
            status: value
                .get("status")
                .and_then(Value::as_str)
                .map(String::from),
            output: value
                .get("output")
                .and_then(Value::as_str)
                .map(String::from),
            error: value.get("error").and_then(Value::as_str).map(String::from),
        }
    }
}

/// The relay the shim forwards through; abstracted so tests can drive
/// `handle_request` with a fake or with the real `tool_proxy` relay.
pub(crate) trait Relay: Send + Sync {
    fn request(&self, req: Value) -> Pin<Box<dyn Future<Output = RelayReply> + Send + '_>>;
}

/// One relay request over a fresh IPC connection (NDJSON, a single line
/// each way), bounded by the configured timeout.
struct UnixRelay<'a> {
    config: &'a ShimConfig,
}

impl Relay for UnixRelay<'_> {
    fn request(&self, req: Value) -> Pin<Box<dyn Future<Output = RelayReply> + Send + '_>> {
        Box::pin(relay_request(self.config, req))
    }
}

async fn relay_request(config: &ShimConfig, request: Value) -> RelayReply {
    let timeout_ms = config.timeout.as_millis();
    let exchange = async {
        let mut conn = UnixStream::connect(&config.socket_path)
            .await
            .map_err(|e| format!("tool proxy socket error: {e}"))?;
        let mut line = serde_json::to_string(&request).unwrap_or_default();
        line.push('\n');
        conn.write_all(line.as_bytes())
            .await
            .map_err(|e| format!("tool proxy socket error: {e}"))?;
        let mut buf = Vec::new();
        let mut chunk = vec![0u8; 65_536].into_boxed_slice();
        loop {
            if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let text = String::from_utf8_lossy(&buf[..pos]).trim().to_string();
                return serde_json::from_str::<Value>(&text)
                    .map_err(|_| "tool proxy returned invalid JSON".to_string());
            }
            match conn.read(&mut chunk).await {
                Ok(0) => return Err("tool proxy socket closed before a reply".to_string()),
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) => return Err(format!("tool proxy socket error: {e}")),
            }
        }
    };
    match tokio::time::timeout(config.timeout, exchange).await {
        Ok(Ok(reply)) => RelayReply::parse(&reply),
        Ok(Err(message)) => RelayReply::failed(message),
        Err(_) => RelayReply::failed(format!("tool proxy request timed out after {timeout_ms}ms")),
    }
}

fn result_response(id: &Value, result: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: &Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

fn new_call_id() -> String {
    protocol::envelope_id()
}

/// Handle one parsed JSON-RPC request. Returns None for notifications,
/// which never get a response.
pub async fn handle_request(request: &Value, relay: Option<&dyn Relay>) -> Option<Value> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let is_notification = request.get("id").is_none() || id.is_null();
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        if is_notification {
            return None;
        }
        return Some(error_response(&id, -32600, "request missing method"));
    };
    match method {
        "initialize" => {
            let requested = request
                .get("params")
                .and_then(|p| p.get("protocolVersion"))
                .and_then(Value::as_str);
            let version = match requested {
                Some(v) if !v.is_empty() && v.len() <= 64 => v,
                _ => DEFAULT_PROTOCOL_VERSION,
            };
            Some(result_response(
                &id,
                &json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
                }),
            ))
        }
        "notifications/initialized" | "notifications/cancelled" => None,
        "ping" => {
            if is_notification {
                None
            } else {
                Some(result_response(&id, &json!({})))
            }
        }
        "tools/list" => Some(handle_tools_list(&id, relay).await),
        "tools/call" => Some(handle_tools_call(&id, request, relay).await),
        other => {
            if is_notification {
                None
            } else {
                let shown: String = other.chars().take(64).collect();
                Some(error_response(
                    &id,
                    -32601,
                    format!("method not supported: {shown}"),
                ))
            }
        }
    }
}

async fn handle_tools_list(id: &Value, relay: Option<&dyn Relay>) -> Value {
    let reply = match relay {
        Some(relay) => relay.request(json!({"op": "list"})).await,
        None => RelayReply::failed("tool proxy is not configured for this task"),
    };
    if !reply.ok {
        // A proxy that is absent/misconfigured must not wedge the client:
        // an empty catalog is the safe answer (deny-by-default hub-side
        // anyway).
        return result_response(id, &json!({"tools": []}));
    }
    let tools: Vec<Value> = reply
        .tools
        .and_then(|t| t.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|tool| {
            let name = tool.get("name")?.as_str()?;
            let description = tool
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("");
            let parameters = tool.get("parameters").filter(|p| p.is_object()).cloned();
            Some(json!({
                "name": name,
                "description": description,
                "inputSchema": parameters.unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            }))
        })
        .collect();
    result_response(id, &json!({"tools": tools}))
}

async fn handle_tools_call(id: &Value, request: &Value, relay: Option<&dyn Relay>) -> Value {
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    if name.is_empty() {
        return error_response(id, -32602, "tools/call requires a name");
    }
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let reply = match relay {
        Some(relay) => {
            relay
                .request(json!({
                    "op": "call",
                    "call_id": new_call_id(),
                    "name": name,
                    "arguments": arguments,
                }))
                .await
        }
        None => RelayReply::failed("tool proxy is not configured for this task"),
    };
    if !reply.ok {
        let text = reply
            .error
            .unwrap_or_else(|| "tool proxy failure".to_string());
        return tool_error_result(id, &text);
    }
    if reply.status.as_deref() == Some("success") {
        return result_response(
            id,
            &json!({
                "content": [{"type": "text", "text": reply.output.unwrap_or_default()}],
                "isError": false,
            }),
        );
    }
    // "denied" and "error" both surface as MCP tool errors; the hub's reason
    // text is operator-facing and sanitized, so pass it through verbatim.
    let reason = reply
        .error
        .filter(|e| !e.is_empty())
        .or(reply.output.filter(|o| !o.is_empty()))
        .unwrap_or_else(|| format!("tool call {}", reply.status.as_deref().unwrap_or("failed")));
    tool_error_result(id, &reason)
}

fn tool_error_result(id: &Value, text: &str) -> Value {
    result_response(
        id,
        &json!({
            "content": [{"type": "text", "text": text}],
            "isError": true,
        }),
    )
}

/// What the stdin gate yields for processing.
pub enum GateEvent {
    /// A complete, in-budget line to parse as JSON-RPC.
    Line(String),
    /// An over-long line: answer exactly once with -32600 carrying the id
    /// recovered from the line's top-level edges (EV-3).
    Reject { id: Option<Value> },
}

/// Stateful stdin line gate implementing the 1 MiB cap with
/// discard-to-newline (EV-3): while an oversized line is being discarded,
/// everything up to its terminating newline is dropped (keeping a rolling
/// tail for id recovery) instead of letting the tail parse as a line of its
/// own.
pub struct LineGate {
    buffer: Vec<u8>,
    discard_head: Option<Vec<u8>>,
    discard_tail: Vec<u8>,
}

impl LineGate {
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            discard_head: None,
            discard_tail: Vec::new(),
        }
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Vec<GateEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        if self.discard_head.is_some() {
            let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') else {
                self.discard_tail = keep_tail(&self.discard_tail, &self.buffer);
                self.buffer.clear();
                return events;
            };
            self.discard_tail = keep_tail(&self.discard_tail, &self.buffer[..pos]);
            self.buffer.drain(..=pos);
            let head = self.discard_head.take().unwrap_or_default();
            let id = recover_request_id_from_edges(&head, &self.discard_tail);
            self.discard_tail.clear();
            events.push(GateEvent::Reject { id });
        }
        while let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..pos).collect();
            self.buffer.drain(..1.min(self.buffer.len()));
            if line.len() > MAX_STDIN_LINE_BYTES {
                let id = recover_request_id_from_edges(&line, &line);
                events.push(GateEvent::Reject { id });
                continue;
            }
            let text = String::from_utf8_lossy(&line).trim().to_string();
            if !text.is_empty() {
                events.push(GateEvent::Line(text));
            }
        }
        if self.buffer.len() > MAX_STDIN_LINE_BYTES {
            // A line this long without a terminator can never succeed at the
            // relay; stop buffering it (the middle is discarded, the head
            // and a rolling tail are kept) and answer it once, with its
            // recovered id, when the terminating newline arrives.
            eprintln!(
                "[{SERVER_NAME}] dropping oversized unterminated stdin line ({} bytes)",
                self.buffer.len()
            );
            self.discard_head = Some(self.buffer[..EDGE_KEEP_BYTES].to_vec());
            self.discard_tail = self.buffer[self.buffer.len() - EDGE_KEEP_BYTES..].to_vec();
            self.buffer.clear();
        }
        events
    }
}

impl Default for LineGate {
    fn default() -> Self {
        Self::new()
    }
}

fn keep_tail(previous: &[u8], chunk: &[u8]) -> Vec<u8> {
    let mut combined = Vec::with_capacity(previous.len() + chunk.len());
    combined.extend_from_slice(previous);
    combined.extend_from_slice(chunk);
    if combined.len() > EDGE_KEEP_BYTES {
        combined.drain(..combined.len() - EDGE_KEEP_BYTES);
    }
    combined
}

/// Parse a JSON-RPC id token at the start of `s`: a string literal (escapes
/// honored) or an integer. Returns the value and the consumed byte count.
fn parse_id_at(s: &[u8]) -> Option<(Value, usize)> {
    if s.is_empty() {
        return None;
    }
    if s[0] == b'"' {
        let mut i = 1;
        while i < s.len() {
            match s[i] {
                b'\\' => i += 2,
                b'"' => {
                    let parsed: String = serde_json::from_slice(&s[..=i]).ok()?;
                    return Some((Value::String(parsed), i + 1));
                }
                _ => i += 1,
            }
        }
        return None;
    }
    let mut i = usize::from(s[0] == b'-');
    let digits_start = i;
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
    }
    if i == digits_start {
        return None;
    }
    let number: i64 = std::str::from_utf8(&s[..i]).ok()?.parse().ok()?;
    Some((Value::from(number), i))
}

/// Recover the request id from an over-long line's kept edges: id-first
/// (`{"jsonrpc":"2.0","id":N,`) at the head, or SDK order
/// (`…,"jsonrpc":"2.0","id":N}`) at the tail. A nested "id" is never used.
fn recover_request_id_from_edges(head: &[u8], tail: &[u8]) -> Option<Value> {
    const HEAD_MARKER: &[u8] = b"{\"jsonrpc\":\"2.0\",\"id\":";
    const TAIL_MARKER: &[u8] = b",\"jsonrpc\":\"2.0\",\"id\":";
    let head_trim: &[u8] = {
        let start = head
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .unwrap_or(head.len());
        &head[start..]
    };
    if let Some(rest) = head_trim.strip_prefix(HEAD_MARKER) {
        if let Some((id, used)) = parse_id_at(rest) {
            if matches!(rest.get(used), Some(b',' | b'}')) {
                return Some(id);
            }
        }
    }
    let tail_trim: &[u8] = {
        let end = tail
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
            .map_or(0, |p| p + 1);
        &tail[..end]
    };
    if tail_trim.last() == Some(&b'}') {
        let body = &tail_trim[..tail_trim.len() - 1];
        if let Some(at) = body
            .windows(TAIL_MARKER.len())
            .rposition(|w| w == TAIL_MARKER)
        {
            let rest = &body[at + TAIL_MARKER.len()..];
            if let Some((id, used)) = parse_id_at(rest) {
                if used == rest.len() {
                    return Some(id);
                }
            }
        }
    }
    None
}

/// Entry point for the `mcp-shim` subcommand: serve MCP over stdio until
/// the client closes stdin.
pub async fn run() {
    let relay = ShimConfig::from_env();
    if relay.is_none() {
        eprintln!(
            "[{SERVER_NAME}] LUNARWING_TOOL_PROXY_SOCKET/LUNARWING_TASK_ID unset; advertising zero tools"
        );
    }
    let unix_relay;
    let relay_ref: Option<&dyn Relay> = match relay.as_ref() {
        Some(config) => {
            unix_relay = UnixRelay { config };
            Some(&unix_relay)
        }
        None => None,
    };
    let mut gate = LineGate::new();
    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut chunk = vec![0u8; 65_536].into_boxed_slice();
    loop {
        match stdin.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                for event in gate.feed(&chunk[..n]) {
                    let response = match event {
                        GateEvent::Reject { id } => Some(error_response(
                            &id.unwrap_or(Value::Null),
                            -32600,
                            "request exceeds the 1 MiB line budget",
                        )),
                        GateEvent::Line(text) => match serde_json::from_str::<Value>(&text) {
                            Ok(request) => handle_request(&request, relay_ref).await,
                            Err(_) => Some(error_response(&Value::Null, -32700, "parse error")),
                        },
                    };
                    if let Some(response) = response {
                        let mut line = serde_json::to_string(&response).unwrap_or_default();
                        line.push('\n');
                        if stdout.write_all(line.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                }
                if stdout.flush().await.is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use tokio::sync::mpsc;

    struct FakeRelay {
        reply: RelayReply,
        seen: StdMutex<Vec<Value>>,
    }

    impl Relay for FakeRelay {
        fn request(&self, req: Value) -> Pin<Box<dyn Future<Output = RelayReply> + Send + '_>> {
            self.seen.lock().unwrap().push(req);
            let reply = self.reply.clone();
            Box::pin(async move { reply })
        }
    }

    fn fake(reply: RelayReply) -> FakeRelay {
        FakeRelay {
            reply,
            seen: StdMutex::new(Vec::new()),
        }
    }

    #[tokio::test]
    async fn initialize_echoes_protocol_version_and_ignores_notifications() {
        let reply = handle_request(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": {"protocolVersion": "2025-03-26"}}),
            None,
        )
        .await
        .expect("response");
        assert_eq!(reply["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(reply["result"]["serverInfo"]["name"], SERVER_NAME);

        let reply = handle_request(
            &json!({"jsonrpc": "2.0", "id": 2, "method": "initialize",
                    "params": {"protocolVersion": "x".repeat(65)}}),
            None,
        )
        .await
        .expect("response");
        assert_eq!(reply["result"]["protocolVersion"], DEFAULT_PROTOCOL_VERSION);

        // initialize is always answered, even notification-shaped (id null),
        // matching the kimi shim; other notification-shaped methods stay
        // silent.
        let reply = handle_request(&json!({"method": "initialize"}), None)
            .await
            .expect("initialize always answers");
        assert_eq!(reply["id"], Value::Null);
        assert!(
            handle_request(&json!({"method": "notifications/initialized"}), None)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn ping_and_unknown_methods() {
        let reply = handle_request(&json!({"id": 7, "method": "ping"}), None)
            .await
            .expect("response");
        assert_eq!(reply["result"], json!({}));
        assert!(handle_request(&json!({"method": "ping"}), None)
            .await
            .is_none());

        let reply = handle_request(&json!({"id": 8, "method": "resources/list"}), None)
            .await
            .expect("response");
        assert_eq!(reply["error"]["code"], -32601);
        assert!(
            handle_request(&json!({"id": 9}), None)
                .await
                .expect("response")["error"]["code"]
                == -32600
        );
    }

    #[tokio::test]
    async fn tools_list_maps_the_catalog_and_defaults() {
        let relay = fake(RelayReply {
            ok: true,
            tools: Some(json!([
                {"name": "memory_read", "description": "read", "parameters": {"type": "object"}},
                {"name": "memory_write"},
                {"description": "no name, dropped"},
            ])),
            ..RelayReply::default()
        });
        let reply = handle_request(&json!({"id": 3, "method": "tools/list"}), Some(&relay))
            .await
            .expect("response");
        let tools = reply["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], "memory_read");
        assert_eq!(tools[0]["inputSchema"]["type"], "object");
        assert_eq!(tools[1]["description"], "");
        assert_eq!(
            tools[1]["inputSchema"],
            json!({"type": "object", "properties": {}})
        );
    }

    #[tokio::test]
    async fn tools_list_without_a_proxy_or_on_failure_is_empty() {
        let reply = handle_request(&json!({"id": 4, "method": "tools/list"}), None)
            .await
            .expect("response");
        assert_eq!(reply["result"]["tools"], json!([]));

        let relay = fake(RelayReply::failed("socket gone"));
        let reply = handle_request(&json!({"id": 4, "method": "tools/list"}), Some(&relay))
            .await
            .expect("response");
        assert_eq!(reply["result"]["tools"], json!([]));
    }

    #[tokio::test]
    async fn tools_call_success_denied_and_error_mapping() {
        let relay = fake(RelayReply {
            ok: true,
            status: Some("success".to_string()),
            output: Some("the value".to_string()),
            ..RelayReply::default()
        });
        let reply = handle_request(
            &json!({"id": 5, "method": "tools/call",
                    "params": {"name": "memory_read", "arguments": {"path": "x"}}}),
            Some(&relay),
        )
        .await
        .expect("response");
        assert_eq!(reply["result"]["isError"], false);
        assert_eq!(reply["result"]["content"][0]["text"], "the value");
        {
            let seen = relay.seen.lock().unwrap();
            assert_eq!(seen[0]["op"], "call");
            assert_eq!(seen[0]["name"], "memory_read");
            assert_eq!(seen[0]["arguments"]["path"], "x");
            let call_id = seen[0]["call_id"].as_str().unwrap();
            assert!(!call_id.is_empty() && call_id.len() <= 128);
        }

        // denied → isError carrying the hub's reason verbatim
        let relay = fake(RelayReply {
            ok: true,
            status: Some("denied".to_string()),
            error: Some("not in tool_allowlist".to_string()),
            ..RelayReply::default()
        });
        let reply = handle_request(
            &json!({"id": 6, "method": "tools/call", "params": {"name": "shell"}}),
            Some(&relay),
        )
        .await
        .expect("response");
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(
            reply["result"]["content"][0]["text"],
            "not in tool_allowlist"
        );

        // error status with only an output body
        let relay = fake(RelayReply {
            ok: true,
            status: Some("error".to_string()),
            output: Some("boom".to_string()),
            ..RelayReply::default()
        });
        let reply = handle_request(
            &json!({"id": 7, "method": "tools/call", "params": {"name": "memory_read"}}),
            Some(&relay),
        )
        .await
        .expect("response");
        assert_eq!(reply["result"]["content"][0]["text"], "boom");

        // transport failure
        let relay = fake(RelayReply::failed("tool proxy socket error: gone"));
        let reply = handle_request(
            &json!({"id": 8, "method": "tools/call", "params": {"name": "memory_read"}}),
            Some(&relay),
        )
        .await
        .expect("response");
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(
            reply["result"]["content"][0]["text"],
            "tool proxy socket error: gone"
        );
    }

    #[tokio::test]
    async fn tools_call_requires_a_name_and_a_proxy() {
        let relay = fake(RelayReply::default());
        let reply = handle_request(
            &json!({"id": 9, "method": "tools/call", "params": {}}),
            Some(&relay),
        )
        .await
        .expect("response");
        assert_eq!(reply["error"]["code"], -32602);

        let reply = handle_request(
            &json!({"id": 10, "method": "tools/call", "params": {"name": "memory_read"}}),
            None,
        )
        .await
        .expect("response");
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(
            reply["result"]["content"][0]["text"],
            "tool proxy is not configured for this task"
        );
    }

    #[test]
    fn line_gate_passes_normal_lines_and_skips_blank() {
        let mut gate = LineGate::new();
        let events = gate.feed(b"{\"a\":1}\n\n{\"b\":2}\n");
        assert_eq!(events.len(), 2);
        match &events[0] {
            GateEvent::Line(text) => assert_eq!(text, "{\"a\":1}"),
            GateEvent::Reject { .. } => panic!("expected a line"),
        }
    }

    #[test]
    fn line_gate_rejects_oversized_lines_once_with_recovered_id() {
        let mut gate = LineGate::new();
        // Over-long, newline-terminated in one chunk: id from the head.
        let mut big = String::from(
            "{\"jsonrpc\":\"2.0\",\"id\":41,\"method\":\"tools/call\",\"params\":{\"x\":\"",
        );
        big.push_str(&"y".repeat(MAX_STDIN_LINE_BYTES));
        big.push_str("\"}}\n");
        let events = gate.feed(big.as_bytes());
        assert_eq!(events.len(), 1);
        match &events[0] {
            GateEvent::Reject { id } => assert_eq!(*id, Some(Value::from(41))),
            GateEvent::Line(_) => panic!("expected a reject"),
        }
        // The next line parses fresh.
        let events = gate.feed(b"{\"ok\":true}\n");
        assert!(matches!(&events[0], GateEvent::Line(t) if t == "{\"ok\":true}"));
    }

    #[test]
    fn line_gate_discards_streaming_oversized_lines_until_newline() {
        let mut gate = LineGate::new();
        let prefix = b"{\"jsonrpc\":\"2.0\",\"id\":\"abc\",\"method\":\"x\",\"pad\":\"";
        assert!(gate.feed(prefix).is_empty());
        // Stream the body past the cap in chunks; no events until newline.
        let filler = vec![b'z'; 512 * 1024];
        assert!(gate.feed(&filler).is_empty());
        assert!(gate.feed(&filler).is_empty());
        assert!(gate.feed(&filler).is_empty());
        let events = gate.feed(b"\"}\n");
        assert_eq!(events.len(), 1);
        match &events[0] {
            GateEvent::Reject { id } => assert_eq!(*id, Some(Value::from("abc"))),
            GateEvent::Line(_) => panic!("expected a reject"),
        }
    }

    #[test]
    fn edge_recovery_handles_sdk_order_and_escapes() {
        let tail = b",\"params\":{},\"jsonrpc\":\"2.0\",\"id\":\"a\\\"b\"}";
        assert_eq!(
            recover_request_id_from_edges(b"", tail),
            Some(Value::from("a\"b"))
        );
        // A nested id is never used.
        let head = b"{\"params\":{\"id\":5},\"jsonrpc\":\"2.0\",\"id\":9}";
        assert_eq!(
            recover_request_id_from_edges(head, head),
            Some(Value::from(9))
        );
        assert_eq!(recover_request_id_from_edges(b"garbage", b"garbage"), None);
    }

    /// The run-84-gap analog: the real shim handler drives the real relay
    /// over a real unix socket, emitting a hub frame that is answered
    /// through `handle_hub_frame`.
    #[tokio::test]
    async fn real_shim_drives_the_real_relay_to_a_hub() {
        let dir = std::env::temp_dir().join(format!("pebble-shim-fullpath-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let relay = tool_proxy::ToolProxyRelay::with_overrides(dir.clone(), Duration::from_secs(5));
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let handle = relay.start_task("task-full", out_tx).await.expect("task");
        let config = ShimConfig {
            socket_path: handle.socket_path.to_string_lossy().into_owned(),
            timeout: Duration::from_secs(5),
        };
        let unix = UnixRelay { config: &config };
        let request = json!({"id": 11, "method": "tools/call",
                             "params": {"name": "memory_read", "arguments": {"path": "p"}}});
        let driver = handle_request(&request, Some(&unix));
        let hub = async {
            // The hub receives a well-formed tool_call frame bound to the task.
            let msg = tokio::time::timeout(Duration::from_secs(5), out_rx.recv())
                .await
                .expect("frame")
                .expect("open");
            let tokio_tungstenite::tungstenite::protocol::Message::Text(text) = msg else {
                panic!("expected text frame");
            };
            let frame: Value = serde_json::from_str(text.as_str()).unwrap();
            assert_eq!(frame["type"], "tool_call");
            assert_eq!(frame["payload"]["task_id"], "task-full");
            assert_eq!(frame["payload"]["name"], "memory_read");
            let call_id = frame["payload"]["call_id"].as_str().unwrap().to_string();
            assert!(
                relay
                    .handle_hub_frame(&protocol::create_envelope(
                        "tool_result",
                        json!({"call_id": call_id, "status": "success", "output": "v", "error": null}),
                    ))
                    .await
            );
        };
        let (reply, ()) = tokio::join!(driver, hub);
        let reply = reply.expect("response");
        assert_eq!(reply["result"]["isError"], false);
        assert_eq!(reply["result"]["content"][0]["text"], "v");
        relay.close_task("task-full").await;
        std::fs::remove_dir_all(dir).ok();
    }

    const NEWLINE: u8 = 10;

    fn reject_ids(gate: &mut LineGate, chunks: &[&[u8]]) -> Vec<Option<Value>> {
        let mut ids = Vec::new();
        for chunk in chunks {
            for event in gate.feed(chunk) {
                match event {
                    GateEvent::Reject { id } => ids.push(id),
                    GateEvent::Line(text) => panic!("expected only rejects, got {text}"),
                }
            }
        }
        ids
    }

    /// FE-2: the model controls the arguments, keys included. A planted
    /// top-level-looking marker at the start or at the end of the arguments
    /// must never be answered; only a top-level edge id is recovered. Runs
    /// both gate paths: the in-loop path (line and newline in one chunk) and
    /// the discard path (newline in a later chunk).
    #[test]
    fn oversized_lines_never_answer_a_planted_nested_id() {
        let pad = "y".repeat(MAX_STDIN_LINE_BYTES);
        let planted_head = format!(
            r#"{{"method":"tools/call","params":{{"arguments":{{"jsonrpc":"2.0","id":99,"pad":"{pad}"}}}},"jsonrpc":"2.0","id":42}}"#
        );
        let planted_tail = format!(
            r#"{{"id":43,"method":"tools/call","params":{{"arguments":{{"pad":"{pad}","jsonrpc":"2.0","id":98}}}}}}"#
        );
        let both_tail = format!(
            r#"{{"method":"tools/call","params":{{"arguments":{{"pad":"{pad}","jsonrpc":"2.0","id":97}}}},"jsonrpc":"2.0","id":44}}"#
        );
        let newline = [NEWLINE];
        for line in [&planted_head, &planted_tail, &both_tail] {
            let mut joined = line.as_bytes().to_vec();
            joined.push(NEWLINE);
            let in_loop = reject_ids(&mut LineGate::new(), &[&joined]);
            let discard = reject_ids(&mut LineGate::new(), &[line.as_bytes(), &newline]);
            assert_eq!(in_loop, discard, "both gate paths agree");
        }
        let ids: Vec<Option<Value>> = [&planted_head, &planted_tail, &both_tail]
            .iter()
            .flat_map(|line| {
                let mut joined = line.as_bytes().to_vec();
                joined.push(NEWLINE);
                reject_ids(&mut LineGate::new(), &[&joined])
            })
            .collect();
        assert_eq!(
            ids,
            vec![Some(Value::from(42)), None, Some(Value::from(44))]
        );
    }

    /// EF-9: an unterminated line never grows the gate past the cap.
    #[test]
    fn line_gate_never_buffers_past_the_cap() {
        let mut gate = LineGate::new();
        let chunk = vec![b'z'; 64 * 1024];
        for _ in 0..40 {
            assert!(gate.feed(&chunk).is_empty());
            assert!(gate.buffer.len() <= MAX_STDIN_LINE_BYTES + chunk.len());
        }
        assert!(gate.discard_head.is_some(), "the line is being discarded");
    }

    /// KL-3 analog: a non-denied hub error is an MCP tool error.
    #[tokio::test]
    async fn error_status_is_a_tool_error_even_with_an_output_body() {
        let relay = fake(RelayReply {
            ok: true,
            status: Some("error".to_string()),
            output: Some("boom".to_string()),
            ..RelayReply::default()
        });
        let reply = handle_request(
            &json!({"id": 12, "method": "tools/call", "params": {"name": "memory_read"}}),
            Some(&relay),
        )
        .await
        .expect("response");
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(reply["result"]["content"][0]["text"], "boom");
    }
}
