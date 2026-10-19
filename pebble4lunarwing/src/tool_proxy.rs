//! `tool_proxy.rs` — bridge-side relay for the WI#324 worker tool proxy
//! (pebble lane, ported from `kimicode4lunarwing/scripts/lunarwing_tool_proxy.ts`).
//!
//! Owns a per-task unix-domain socket that the in-task stdio MCP shim
//! (`mcp_shim.rs`, spawned by `pebble` via the registered `mcpServers`
//! entry) connects to, and translates the shim's newline-delimited JSON
//! requests into `tool_list` / `tool_call` frames on the task's hub
//! WebSocket. Replies (`tool_catalog` / `tool_result`) arrive on the same
//! hub connection and are routed back to the waiting shim by call id.
//!
//! IPC request shapes (one JSON object per line):
//!   `{"op": "list"}`
//!   `{"op": "call", "call_id": "<ident>", "name": "<tool>", "arguments": {...}}`
//! IPC response shapes (one line, then the relay half-closes):
//!   `{"ok": true, "tools": [...]}`                    (list)
//!   `{"ok": true, "status", "output", "error"}`       (call)
//!   `{"ok": false, "error": "<reason>"}`              (any failure)
//!
//! Bounds mirror the hub (`external_worker.rs`): a serialized `tool_call`
//! frame must fit 256 KiB; name/`call_id` are `[A-Za-z0-9_-]{1,128}`. The
//! relay pre-checks the frame size so an oversized call gets a clean local
//! error instead of a hub-side frame rejection that fails the whole job.
//!
//! One relay exists per hub connection (the pebble bridge serves one daemon
//! connection at a time per accepted socket), so the kimi lane's
//! `pendingBySocket` map collapses to a single pending table scoped by the
//! connection's lifetime; `drop_hub` on connection close is the EV-4/EF-9
//! equivalent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::protocol::{self, Envelope};

/// Keep in sync with `MAX_TOOL_CALL_FRAME_BYTES` in
/// `ic/src/orchestrator/external_worker.rs` and `TOOL_CALL_FRAME_CAP_BYTES`
/// in `kimicode4lunarwing/scripts/lunarwing_tool_proxy.ts`.
pub const TOOL_CALL_FRAME_CAP_BYTES: usize = 256 * 1024;
/// IPC line cap: a request line larger than this can never fit the WS frame
/// cap, so the connection is told off and the line is dropped.
const MAX_IPC_LINE_BYTES: usize = TOOL_CALL_FRAME_CAP_BYTES;
const MAX_IDENT_LEN: usize = 128;
const DEFAULT_PROXY_TIMEOUT_MS: u64 = 300_000;
const MAX_PROXY_TIMEOUT_MS: u64 = 900_000;
const MIN_PROXY_TIMEOUT_MS: u64 = 1_000;

/// Outbound hub frames for one connection, drained by the bridge's writer
/// task. Shared with bridge.rs.
pub(crate) type Outbound = mpsc::UnboundedSender<Message>;

/// Env key pointing the in-task shim at the per-task relay socket.
pub const PROXY_SOCKET_ENV: &str = "LUNARWING_TOOL_PROXY_SOCKET";
/// Env key carrying the hub-bound task id into the task process.
pub const TASK_ID_ENV: &str = "LUNARWING_TASK_ID";

fn proxy_timeout_from_env() -> Duration {
    let raw = std::env::var("LUNARWING_TOOL_PROXY_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    let ms = raw.map_or(DEFAULT_PROXY_TIMEOUT_MS, |v| {
        v.clamp(MIN_PROXY_TIMEOUT_MS, MAX_PROXY_TIMEOUT_MS)
    });
    Duration::from_millis(ms)
}

/// Directory the per-task sockets live in. Overridable for tests and for
/// operators via `LUNARWING_TOOL_PROXY_DIR`; kept short because unix socket
/// paths cap at ~108 bytes.
#[must_use]
pub fn tool_proxy_dir() -> PathBuf {
    std::env::var("LUNARWING_TOOL_PROXY_DIR")
        .ok()
        .filter(|v| !v.is_empty())
        .map_or_else(|| PathBuf::from("/tmp/lunarwing-tool-proxy"), PathBuf::from)
}

fn is_valid_ident(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENT_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// FNV-1a 64-bit: a small dependency-free hash for the socket-path suffix.
/// This is NOT a security boundary (the proxy dir is a same-uid shared /tmp
/// tree — EF-1/EF-2); it only keeps paths unique per task id.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Filesystem-safe per-task socket path under `dir`. Task ids come from the
/// hub but are still treated as untrusted input for path construction
/// (sanitized + hash-suffixed).
#[must_use]
pub fn task_socket_path_in(dir: &Path, task_id: &str) -> PathBuf {
    let safe: String = task_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    dir.join(format!("{safe}-{:016x}.sock", fnv1a_64(task_id.as_bytes())))
}

/// Environment the task's MCP shim needs, for injection into the pebble
/// child AFTER task-supplied env so it cannot be shadowed.
#[must_use]
pub fn proxy_env_for_task(task_id: &str, socket_path: &Path) -> [(String, String); 2] {
    [
        (TASK_ID_ENV.to_string(), task_id.to_string()),
        (
            PROXY_SOCKET_ENV.to_string(),
            socket_path.to_string_lossy().into_owned(),
        ),
    ]
}

struct PendingWaiter {
    task_id: String,
    is_catalog: bool,
    reply: oneshot::Sender<Value>,
}

struct RelayCore {
    pending: Mutex<HashMap<String, PendingWaiter>>,
    /// EV-4: once the hub connection is gone it stays gone — a request
    /// arriving on a still-open task IPC server fails at once instead of
    /// re-creating pending state and waiting out the relay timeout.
    hub_open: AtomicBool,
}

struct TaskProxy {
    socket_path: PathBuf,
    accept_loop: tokio::task::JoinHandle<()>,
}

/// Handle returned to the bridge for one task's relay slot.
pub struct TaskProxyHandle {
    pub socket_path: PathBuf,
}

/// One relay per hub connection. Tracks pending hub replies (the hub's
/// `tool_result`/`tool_catalog` frames carry `call_id` but no `task_id`, so
/// routing is scoped by the connection the reply arrived on) and one IPC
/// server per active task.
pub struct ToolProxyRelay {
    core: Arc<RelayCore>,
    tasks: Mutex<HashMap<String, TaskProxy>>,
    dir: PathBuf,
    timeout: Duration,
}

impl ToolProxyRelay {
    /// Production constructor: directory and timeout from the environment.
    pub fn new() -> Self {
        Self::with_overrides(tool_proxy_dir(), proxy_timeout_from_env())
    }

    pub fn with_overrides(dir: PathBuf, timeout: Duration) -> Self {
        Self {
            core: Arc::new(RelayCore {
                pending: Mutex::new(HashMap::new()),
                hub_open: AtomicBool::new(true),
            }),
            tasks: Mutex::new(HashMap::new()),
            dir,
            timeout,
        }
    }

    /// Start the per-task IPC server. Returns the handle, or None when the
    /// server could not be started (the task then simply runs without
    /// tools). Never fails hard: the filesystem setup runs on a shared,
    /// same-uid /tmp tree that a concurrent task process could sabotage
    /// (MR !1549 EF-2).
    pub async fn start_task(&self, task_id: &str, out_tx: Outbound) -> Option<TaskProxyHandle> {
        let socket_path = task_socket_path_in(&self.dir, task_id);
        let mut tasks = self.tasks.lock().await;
        if tasks.contains_key(task_id) {
            return None;
        }
        if let Err(e) = prepare_socket_path(&socket_path) {
            eprintln!(
                "[tool-proxy] task {task_id}: proxy dir setup failed (task runs without tools): {e}"
            );
            return None;
        }
        let listener = match UnixListener::bind(&socket_path) {
            Ok(listener) => listener,
            Err(e) => {
                eprintln!("[tool-proxy] task {task_id}: listen failed: {e}");
                return None;
            }
        };
        let accept_loop = tokio::spawn(accept_loop(
            listener,
            Arc::clone(&self.core),
            out_tx,
            task_id.to_string(),
            self.timeout,
        ));
        tasks.insert(
            task_id.to_string(),
            TaskProxy {
                socket_path: socket_path.clone(),
                accept_loop,
            },
        );
        drop(tasks);
        Some(TaskProxyHandle { socket_path })
    }

    /// Route an inbound hub frame (`tool_result` / `tool_catalog`) to its
    /// waiter. Returns true when the frame was consumed.
    pub async fn handle_hub_frame(&self, envelope: &Envelope) -> bool {
        if !envelope.payload.is_object() {
            return false;
        }
        match envelope.msg_type.as_str() {
            "tool_result" => {
                let call_id = envelope
                    .payload
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let waiter = {
                    let mut pending = self.core.pending.lock().await;
                    let routable = matches!(pending.get(call_id), Some(w) if !w.is_catalog);
                    if routable {
                        pending.remove(call_id)
                    } else {
                        None
                    }
                };
                let Some(waiter) = waiter else { return false };
                let _ = waiter.reply.send(envelope.payload.clone());
                true
            }
            "tool_catalog" => {
                // tool_list carries no call_id; at most one catalog request
                // is in flight per task, keyed synthetically.
                let waiter = {
                    let mut pending = self.core.pending.lock().await;
                    let key = pending
                        .iter()
                        .find(|(_, waiter)| waiter.is_catalog)
                        .map(|(key, _)| key.clone());
                    key.and_then(|key| pending.remove(&key))
                };
                let Some(waiter) = waiter else { return false };
                let _ = waiter.reply.send(envelope.payload.clone());
                true
            }
            _ => false,
        }
    }

    /// Close the task's IPC server and fail every call it still has in
    /// flight. Idempotent and never-failing (EF-2): teardown runs from task
    /// cleanup paths that must still release their other slots.
    pub async fn close_task(&self, task_id: &str) {
        let task = self.tasks.lock().await.remove(task_id);
        let Some(task) = task else { return };
        // Stop accepting new connections; live connections end themselves
        // once their (final) response is written — one reply per connection,
        // then half-close — so pending waiters are resolved below and their
        // error responses still reach the shim. A shim's next connect gets
        // ECONNREFUSED from the closed server, surfaced as a tool error.
        task.accept_loop.abort();
        if let Err(e) = std::fs::remove_file(&task.socket_path) {
            // Best effort (EF-2): a sabotaged proxy dir must not fail task
            // teardown — the caller still has to release the workspace claim.
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!("[tool-proxy] task {task_id}: socket cleanup failed: {e}");
            }
        }
        let keys: Vec<String> = {
            let pending = self.core.pending.lock().await;
            pending
                .iter()
                .filter(|(_, waiter)| waiter.task_id == task_id)
                .map(|(key, _)| key.clone())
                .collect()
        };
        let mut pending = self.core.pending.lock().await;
        for key in keys {
            if let Some(waiter) = pending.remove(&key) {
                let _ = waiter
                    .reply
                    .send(json!({"error": "task ended before the tool reply arrived"}));
            }
        }
    }

    /// Hub connection closed: fail every waiter parked on it and refuse new
    /// ones (MR !1549 EF-9, MR !1561 EV-4). Task IPC servers stay up; their
    /// next request fails at once with the same error.
    pub async fn drop_hub(&self) {
        self.core.hub_open.store(false, Ordering::SeqCst);
        let drained: Vec<PendingWaiter> = {
            let mut pending = self.core.pending.lock().await;
            pending.drain().map(|(_, waiter)| waiter).collect()
        };
        for waiter in drained {
            let _ = waiter
                .reply
                .send(json!({"error": "hub connection closed before the tool reply arrived"}));
        }
    }

    /// Number of tasks with a live IPC server (test introspection).
    #[cfg(test)]
    pub async fn active_task_count(&self) -> usize {
        self.tasks.lock().await.len()
    }
}

impl Default for ToolProxyRelay {
    fn default() -> Self {
        Self::new()
    }
}

/// mkdir -p the parent dir (0o700) and remove a stale socket file from a
/// crashed earlier task (the path is hash-bound to this task, so a leftover
/// would make `bind()` fail with `EADDRINUSE`).
fn prepare_socket_path(socket_path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    match std::fs::remove_file(socket_path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

async fn accept_loop(
    listener: UnixListener,
    core: Arc<RelayCore>,
    out_tx: Outbound,
    task_id: String,
    timeout: Duration,
) {
    loop {
        let Ok((mut conn, _)) = listener.accept().await else {
            break;
        };
        let conn_core = Arc::clone(&core);
        let conn_out = out_tx.clone();
        let conn_task = task_id.clone();
        tokio::spawn(async move {
            handle_ipc_conn(&mut conn, conn_core, conn_out, conn_task, timeout).await;
        });
    }
}

/// One request per connection, one response, then half-close (the kimi
/// lane's end-after-flush): reply bytes leave the kernel buffer even when
/// the task is closing, and every response path — including the over-cap
/// told-off — ends the connection the same way. An over-cap line is
/// discarded up to its terminating newline before the reply is written, and
/// every reply is followed by a drain to EOF: closing with unread data in
/// the socket receive queue would RST the just-written reply away (the shim
/// never pipelines, but a same-uid task process can).
async fn handle_ipc_conn(
    conn: &mut UnixStream,
    core: Arc<RelayCore>,
    out_tx: Outbound,
    task_id: String,
    timeout: Duration,
) {
    const OVER_CAP: &str = "request line exceeds the 256 KiB frame budget";
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 65_536].into_boxed_slice();
    let mut discarding = false;
    loop {
        if let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
            if discarding || pos > MAX_IPC_LINE_BYTES {
                buffer.drain(..=pos);
                respond_and_drain(conn, json!({"ok": false, "error": OVER_CAP})).await;
                return;
            }
            let mut line: Vec<u8> = buffer.drain(..=pos).collect();
            line.pop(); // the newline
            let text = String::from_utf8_lossy(&line).trim().to_string();
            if text.is_empty() {
                continue;
            }
            let response = handle_ipc_line(&core, &out_tx, &task_id, &text, timeout).await;
            respond_and_drain(conn, response).await;
            return;
        }
        if buffer.len() > MAX_IPC_LINE_BYTES {
            // A line this long without a terminator can never succeed at the
            // hub; keep discarding until its newline, then answer.
            discarding = true;
            buffer.clear();
        }
        match conn.read(&mut chunk).await {
            Ok(0) | Err(_) => return, // peer went away with nothing pending
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
        }
    }
}

async fn respond_and_drain(conn: &mut UnixStream, response: Value) {
    if let Ok(mut text) = serde_json::to_string(&response) {
        text.push('\n');
        if conn.write_all(text.as_bytes()).await.is_ok() {
            let _ = conn.shutdown().await;
        }
    }
    let mut sink = vec![0u8; 65_536].into_boxed_slice();
    loop {
        match conn.read(&mut sink).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

async fn handle_ipc_line(
    core: &Arc<RelayCore>,
    out_tx: &Outbound,
    task_id: &str,
    line: &str,
    timeout: Duration,
) -> Value {
    let request: Value = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(_) => return json!({"ok": false, "error": "request is not valid JSON"}),
    };
    let Some(op) = request.get("op").and_then(Value::as_str) else {
        return json!({"ok": false, "error": "request must be an object with an op field"});
    };
    if !core.hub_open.load(Ordering::SeqCst) {
        return json!({"ok": false, "error": "hub connection closed before the tool reply arrived"});
    }
    match op {
        "list" => handle_list(core, out_tx, task_id, timeout).await,
        "call" => handle_call(core, out_tx, task_id, &request, timeout).await,
        other => {
            let shown: String = other.chars().take(64).collect();
            json!({"ok": false, "error": format!("unknown op {}", shown)})
        }
    }
}

async fn handle_list(
    core: &Arc<RelayCore>,
    out_tx: &Outbound,
    task_id: &str,
    timeout: Duration,
) -> Value {
    let frame = protocol::create_envelope("tool_list", json!({ "task_id": task_id }));
    let Ok(serialized) = serde_json::to_string(&frame) else {
        return json!({"ok": false, "error": "tool_catalog request failed or timed out"});
    };
    // One catalog request in flight per task, keyed synthetically (the hub's
    // tool_catalog carries no call_id to correlate by).
    let key = format!("catalog:{task_id}");
    match send_and_wait(core, out_tx, key, task_id, true, serialized, timeout).await {
        Some(payload) => {
            let tools = payload.get("tools").cloned().unwrap_or_else(|| json!([]));
            let error = payload.get("error").cloned().unwrap_or(Value::Null);
            json!({"ok": true, "tools": tools, "error": error})
        }
        None => json!({"ok": false, "error": "tool_catalog request failed or timed out"}),
    }
}

async fn handle_call(
    core: &Arc<RelayCore>,
    out_tx: &Outbound,
    task_id: &str,
    request: &Value,
    timeout: Duration,
) -> Value {
    let Some(call_id) = request.get("call_id").and_then(Value::as_str) else {
        return json!({"ok": false, "error": "call_id must be [A-Za-z0-9_-]{1,128}"});
    };
    if !is_valid_ident(call_id) {
        return json!({"ok": false, "error": "call_id must be [A-Za-z0-9_-]{1,128}"});
    }
    let Some(name) = request.get("name").and_then(Value::as_str) else {
        return json!({"ok": false, "error": "name must be [A-Za-z0-9_-]{1,128}"});
    };
    if !is_valid_ident(name) {
        return json!({"ok": false, "error": "name must be [A-Za-z0-9_-]{1,128}"});
    }
    let arguments = request.get("arguments").cloned().unwrap_or(Value::Null);
    let frame = protocol::create_envelope(
        "tool_call",
        json!({
            "task_id": task_id,
            "call_id": call_id,
            "name": name,
            "arguments": arguments,
        }),
    );
    let serialized = serde_json::to_string(&frame).unwrap_or_default();
    if serialized.len() > TOOL_CALL_FRAME_CAP_BYTES {
        // KL-8/JU-4: pre-check locally so the caller gets a clean budget
        // error instead of a hub-side frame rejection or a bare EPIPE.
        return json!({"ok": false, "error": format!(
            "tool_call frame exceeds the {TOOL_CALL_FRAME_CAP_BYTES}-byte hub budget"
        )});
    }
    match send_and_wait(
        core,
        out_tx,
        call_id.to_string(),
        task_id,
        false,
        serialized,
        timeout,
    )
    .await
    {
        Some(payload) => {
            let status = payload
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("error");
            let output = payload.get("output").and_then(Value::as_str).unwrap_or("");
            let error = payload.get("error").cloned().unwrap_or(Value::Null);
            json!({"ok": true, "status": status, "output": output, "error": error})
        }
        None => json!({"ok": false, "error": "tool_result wait failed or timed out"}),
    }
}

/// Park a waiter and send the frame. Resolves `Some(payload)` with the
/// hub's reply payload (including the error payloads synthesized by
/// `close_task` / `drop_hub`), or `None` on duplicate key, send failure, or
/// timeout.
async fn send_and_wait(
    core: &Arc<RelayCore>,
    out_tx: &Outbound,
    key: String,
    task_id: &str,
    is_catalog: bool,
    serialized: String,
    timeout: Duration,
) -> Option<Value> {
    if !core.hub_open.load(Ordering::SeqCst) {
        return Some(json!({"error": "hub connection closed before the tool reply arrived"}));
    }
    let (tx, rx) = oneshot::channel();
    {
        let mut pending = core.pending.lock().await;
        if pending.contains_key(&key) {
            // Duplicate call_id in flight; the shim must not reuse ids.
            return None;
        }
        pending.insert(
            key.clone(),
            PendingWaiter {
                task_id: task_id.to_string(),
                is_catalog,
                reply: tx,
            },
        );
    }
    if out_tx.send(Message::Text(serialized.into())).is_err() {
        core.pending.lock().await.remove(&key);
        eprintln!("[tool-proxy] hub send failed");
        return None;
    }
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(payload)) => Some(payload),
        Ok(Err(_)) => None, // sender dropped without a resolve
        Err(_) => {
            core.pending.lock().await.remove(&key);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_dir() -> PathBuf {
        let unique = TEMP_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("pebble-tool-proxy-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create proxy test dir");
        dir
    }

    fn test_relay() -> (ToolProxyRelay, PathBuf) {
        let dir = test_dir();
        (
            ToolProxyRelay::with_overrides(dir.clone(), Duration::from_secs(5)),
            dir,
        )
    }

    fn fake_hub() -> (Outbound, mpsc::UnboundedReceiver<Message>) {
        mpsc::unbounded_channel()
    }

    async fn next_hub_frame(rx: &mut mpsc::UnboundedReceiver<Message>) -> Envelope {
        let msg = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("hub frame within timeout")
            .expect("hub channel open");
        let Message::Text(text) = msg else {
            panic!("expected text frame, got {msg:?}");
        };
        serde_json::from_str(text.as_str()).expect("valid envelope JSON")
    }

    async fn ipc_request(socket_path: &Path, request: &Value) -> Value {
        let mut conn = UnixStream::connect(socket_path)
            .await
            .expect("connect to task socket");
        conn.write_all(serde_json::to_string(request).unwrap().as_bytes())
            .await
            .expect("write request");
        conn.write_all(b"\n").await.expect("write newline");
        let mut reply = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), conn.read_to_end(&mut reply))
            .await
            .expect("reply within timeout")
            .expect("read reply");
        let text = String::from_utf8(reply).expect("utf-8 reply");
        serde_json::from_str(text.trim()).expect("reply is JSON")
    }

    #[test]
    fn socket_path_is_sanitized_bounded_and_deterministic() {
        let dir = Path::new("/tmp/x");
        let path = task_socket_path_in(dir, "task/with..bad chars");
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(!name.contains('/'));
        assert_eq!(
            Path::new(name).extension().and_then(|e| e.to_str()),
            Some("sock")
        );
        assert!(path.to_str().unwrap().len() <= 108);
        assert_eq!(path, task_socket_path_in(dir, "task/with..bad chars"));
        assert_ne!(path, task_socket_path_in(dir, "task/with..bad chars2"));
        let long = task_socket_path_in(dir, &"a".repeat(500));
        assert!(long.to_str().unwrap().len() <= 108);
    }

    #[test]
    fn ident_bounds_match_the_hub() {
        assert!(is_valid_ident("memory_read"));
        assert!(is_valid_ident("gitlab-tool"));
        assert!(!is_valid_ident(""));
        assert!(!is_valid_ident(&"a".repeat(129)));
        assert!(!is_valid_ident("bad name"));
        assert!(!is_valid_ident("bad/name"));
        assert!(is_valid_ident(&"a".repeat(128)));
    }

    #[tokio::test]
    async fn list_round_trips_through_the_hub() {
        let (relay, dir) = test_relay();
        let (out_tx, mut out_rx) = fake_hub();
        let handle = relay
            .start_task("task-list", out_tx)
            .await
            .expect("task starts");

        let client = tokio::spawn({
            let socket = handle.socket_path.clone();
            async move { ipc_request(&socket, &json!({"op": "list"})).await }
        });
        let frame = next_hub_frame(&mut out_rx).await;
        assert_eq!(frame.msg_type, "tool_list");
        assert_eq!(frame.payload["task_id"], "task-list");
        assert!(
            relay
                .handle_hub_frame(&protocol::create_envelope(
                    "tool_catalog",
                    json!({"tools": [{"name": "memory_read"}]}),
                ))
                .await
        );
        let reply = client.await.expect("client joins");
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["tools"][0]["name"], "memory_read");
        relay.close_task("task-list").await;
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn call_frame_shape_and_result_routing() {
        let (relay, dir) = test_relay();
        let (out_tx, mut out_rx) = fake_hub();
        let handle = relay.start_task("task-call", out_tx).await.expect("task");

        let client = tokio::spawn({
            let socket = handle.socket_path.clone();
            async move {
                ipc_request(
                    &socket,
                    &json!({"op": "call", "call_id": "c-1", "name": "memory_read",
                            "arguments": {"path": "x"}}),
                )
                .await
            }
        });
        let frame = next_hub_frame(&mut out_rx).await;
        assert_eq!(frame.msg_type, "tool_call");
        assert_eq!(frame.payload["task_id"], "task-call");
        assert_eq!(frame.payload["call_id"], "c-1");
        assert_eq!(frame.payload["name"], "memory_read");
        assert_eq!(frame.payload["arguments"]["path"], "x");
        assert!(
            relay.handle_hub_frame(&protocol::create_envelope(
                "tool_result",
                json!({"call_id": "c-1", "status": "success", "output": "value", "error": null}),
            )).await
        );
        let reply = client.await.expect("client joins");
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["status"], "success");
        assert_eq!(reply["output"], "value");
        assert_eq!(reply["error"], Value::Null);
        relay.close_task("task-call").await;
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn denied_status_and_reason_pass_through() {
        let (relay, dir) = test_relay();
        let (out_tx, mut out_rx) = fake_hub();
        let handle = relay.start_task("task-denied", out_tx).await.expect("task");
        let client = tokio::spawn({
            let socket = handle.socket_path.clone();
            async move {
                ipc_request(
                    &socket,
                    &json!({"op": "call", "call_id": "c-2", "name": "shell"}),
                )
                .await
            }
        });
        let frame = next_hub_frame(&mut out_rx).await;
        // Missing arguments defaults to null, matching the wire contract.
        assert_eq!(frame.payload["arguments"], Value::Null);
        assert!(
            relay.handle_hub_frame(&protocol::create_envelope(
                "tool_result",
                json!({"call_id": "c-2", "status": "denied", "output": "", "error": "not in tool_allowlist"}),
            )).await
        );
        let reply = client.await.expect("client joins");
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["status"], "denied");
        assert_eq!(reply["error"], "not in tool_allowlist");
        relay.close_task("task-denied").await;
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn invalid_identifiers_are_rejected_locally() {
        let (relay, dir) = test_relay();
        let (out_tx, mut out_rx) = fake_hub();
        let handle = relay.start_task("task-ident", out_tx).await.expect("task");
        for request in [
            json!({"op": "call", "call_id": "bad id", "name": "memory_read"}),
            json!({"op": "call", "call_id": "c", "name": "bad/name"}),
            json!({"op": "call", "call_id": "c", "name": "memory_read", "arguments": {"x": "y".repeat(300 * 1024)}}),
            json!({"op": "call", "call_id": 42, "name": "memory_read"}),
        ] {
            let reply = ipc_request(&handle.socket_path, &request).await;
            assert_eq!(reply["ok"], false, "request {request}");
        }
        // Nothing reached the hub.
        assert!(out_rx.try_recv().is_err());
        relay.close_task("task-ident").await;
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn malformed_requests_are_rejected() {
        let (relay, dir) = test_relay();
        let (out_tx, _out_rx) = fake_hub();
        let handle = relay.start_task("task-bad", out_tx).await.expect("task");
        for request in [
            json!("not an object"),
            json!({"nope": 1}),
            json!({"op": "frobnicate"}),
        ] {
            let reply = ipc_request(&handle.socket_path, &request).await;
            assert_eq!(reply["ok"], false, "request {request}");
        }
        relay.close_task("task-bad").await;
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn close_task_fails_in_flight_calls() {
        let (relay, dir) = test_relay();
        let (out_tx, mut out_rx) = fake_hub();
        let handle = relay.start_task("task-close", out_tx).await.expect("task");
        let client = tokio::spawn({
            let socket = handle.socket_path.clone();
            async move {
                ipc_request(
                    &socket,
                    &json!({"op": "call", "call_id": "c-3", "name": "memory_read"}),
                )
                .await
            }
        });
        let _frame = next_hub_frame(&mut out_rx).await;
        relay.close_task("task-close").await;
        let reply = client.await.expect("client joins");
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["status"], "error");
        assert_eq!(reply["error"], "task ended before the tool reply arrived");
        assert!(!handle.socket_path.exists(), "socket file removed");
        assert_eq!(relay.active_task_count().await, 0);
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn drop_hub_fails_waiters_and_new_requests_fast() {
        let (relay, dir) = test_relay();
        let (out_tx, mut out_rx) = fake_hub();
        let handle = relay.start_task("task-drop", out_tx).await.expect("task");
        let client = tokio::spawn({
            let socket = handle.socket_path.clone();
            async move {
                ipc_request(
                    &socket,
                    &json!({"op": "call", "call_id": "c-4", "name": "memory_read"}),
                )
                .await
            }
        });
        let _frame = next_hub_frame(&mut out_rx).await;
        relay.drop_hub().await;
        let reply = client.await.expect("client joins");
        assert_eq!(
            reply["error"],
            "hub connection closed before the tool reply arrived"
        );
        // EV-4: a later request on the still-open task IPC fails at once.
        let reply = ipc_request(
            &handle.socket_path,
            &json!({"op": "call", "call_id": "c-5", "name": "memory_read"}),
        )
        .await;
        assert_eq!(reply["ok"], false);
        assert_eq!(
            reply["error"],
            "hub connection closed before the tool reply arrived"
        );
        relay.close_task("task-drop").await;
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn duplicate_task_start_is_refused() {
        let (relay, dir) = test_relay();
        let (out_tx, _rx) = fake_hub();
        assert!(relay.start_task("task-dup", out_tx.clone()).await.is_some());
        assert!(relay.start_task("task-dup", out_tx).await.is_none());
        relay.close_task("task-dup").await;
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn duplicate_call_id_and_catalog_requests_are_refused() {
        let (relay, dir) = test_relay();
        let (out_tx, mut out_rx) = fake_hub();
        let handle = relay
            .start_task("task-dupcall", out_tx)
            .await
            .expect("task");
        let first = tokio::spawn({
            let socket = handle.socket_path.clone();
            async move {
                ipc_request(
                    &socket,
                    &json!({"op": "call", "call_id": "c-6", "name": "memory_read"}),
                )
                .await
            }
        });
        let _frame = next_hub_frame(&mut out_rx).await;
        // Same call_id while one is in flight: refused without a second frame.
        let reply = ipc_request(
            &handle.socket_path,
            &json!({"op": "call", "call_id": "c-6", "name": "memory_read"}),
        )
        .await;
        assert_eq!(reply["ok"], false);
        assert!(out_rx.try_recv().is_err());
        relay.close_task("task-dupcall").await;
        let reply = first.await.expect("first joins");
        assert_eq!(reply["error"], "task ended before the tool reply arrived");
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn tool_result_for_an_unknown_call_id_is_unroutable() {
        let (relay, dir) = test_relay();
        assert!(
            !relay
                .handle_hub_frame(&protocol::create_envelope(
                    "tool_result",
                    json!({"call_id": "nobody", "status": "success"}),
                ))
                .await
        );
        assert!(
            !relay
                .handle_hub_frame(&protocol::create_envelope(
                    "tool_catalog",
                    json!({"tools": []}),
                ))
                .await
        );
        assert!(
            !relay
                .handle_hub_frame(&protocol::create_envelope("task_request", json!({})))
                .await
        );
        std::fs::remove_dir_all(dir).ok();
    }
}
