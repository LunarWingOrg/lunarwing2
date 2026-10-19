use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::WebSocketStream;

use crate::executor::{self, ExecutorConfig, ExecutorEvent};
use crate::health::WorkerState;
use crate::protocol::{self, CancelPayload, Envelope, TaskRequest};
use crate::tool_proxy::{self, ToolProxyRelay};
use crate::workspace_path;

/// Active workspaces across every connection, mirroring the kimi bridge's
/// `activeWorkspaces` map: a workspace subtree may only be modified by one
/// task at a time so concurrent tasks cannot race each other's files.
#[derive(Default)]
pub struct WorkspaceRegistry {
    workspaces: Mutex<HashMap<PathBuf, String>>,
}

impl WorkspaceRegistry {
    fn new() -> Self {
        Self::default()
    }

    /// Atomically claim `workspace`: rejects with the existing owner's task
    /// id when an overlapping workspace is already registered, otherwise
    /// records `task_id` as the owner. Holding the lock across the overlap
    /// check and the insert closes the check-then-act window that would
    /// otherwise let two concurrent dispatches both claim the same subtree.
    async fn try_register(&self, workspace: PathBuf, task_id: &str) -> Result<(), String> {
        let mut workspaces = self.workspaces.lock().await;
        if let Some(owner) = workspaces
            .iter()
            .find(|(active, _)| workspace_path::workspace_paths_overlap(active, &workspace))
            .map(|(_, owner)| owner.clone())
        {
            return Err(owner);
        }
        workspaces.insert(workspace, task_id.to_string());
        Ok(())
    }

    async fn unregister(&self, workspace: &Path, task_id: &str) {
        let mut workspaces = self.workspaces.lock().await;
        if workspaces
            .get(workspace)
            .is_some_and(|owner| owner == task_id)
        {
            workspaces.remove(workspace);
        }
    }
}

pub struct BridgeConfig {
    pub bind_host: String,
    pub ws_port: u16,
    pub ws_path: String,
    pub health_port: u16,
    pub auth_token: Option<String>,
    pub worker_id: String,
    pub pebble_bin: String,
    pub model: String,
    pub permission_mode: String,
    pub workspace_root: String,
    /// Where this bridge's per-task relay sockets live
    /// (`LUNARWING_TOOL_PROXY_DIR`, default `/tmp/lunarwing-tool-proxy`).
    pub tool_proxy_dir: std::path::PathBuf,
}

/// Outbound envelopes for one WebSocket connection, drained by the writer
/// task so no code path needs to own the sink while a task runs.
type Outbound = mpsc::UnboundedSender<Message>;

impl BridgeConfig {
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            bind_host: env_or("WS_BIND_HOST", "0.0.0.0"),
            ws_port: env_or("WS_PORT", "9090").parse().unwrap_or(9090),
            ws_path: env_or("WS_PATH", "/ws/agent"),
            health_port: env_or("HEALTH_PORT", "8443").parse().unwrap_or(8443),
            auth_token: resolve_bridge_auth_token(
                std::env::var_os("AGENT_AUTH_TOKEN_FILE"),
                std::env::var("AGENT_AUTH_TOKEN").ok(),
            )?,
            worker_id: env_or("LUNARWING_WORKER_ID", "worker-pebble-01"),
            pebble_bin: env_or("PEBBLE_BIN", "pebble"),
            model: env_or("PEBBLE_MODEL", "openai/gpt-5.2"),
            permission_mode: env_or("PEBBLE_PERMISSION_MODE", "danger-full-access"),
            workspace_root: env_or("WORKSPACE_ROOT", "/workspace"),
            tool_proxy_dir: tool_proxy::tool_proxy_dir(),
        })
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Resolve the hub-handshake credential (WI#400): the entrypoint moves it
/// out of the process environment into a root-only file
/// (`AGENT_AUTH_TOKEN_FILE`) before spawning the bridge, so no long-lived
/// process a task can inspect carries it in its environ. The env var stays
/// as a fallback for dev launches that inject the token directly.
///
/// KW-8/LD-10: with the file pointer set, a failed or empty read is a
/// STARTUP ERROR — the entrypoint has already unset `AGENT_AUTH_TOKEN`, so
/// falling back would silently start with no bearer check
/// (`auth = disabled`). Inputs are arguments so the policy is testable
/// without touching the process environment.
fn resolve_bridge_auth_token(
    token_file: Option<std::ffi::OsString>,
    env_token: Option<String>,
) -> Result<Option<String>, String> {
    if let Some(path) = token_file {
        let shown = path.to_string_lossy().into_owned();
        let contents = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read AGENT_AUTH_TOKEN_FILE {shown}: {e}"))?;
        // Trim trailing CR/LF only (byte values 13 and 10) — the token
        // itself may contain any other whitespace.
        let token = contents
            .trim_end_matches(|c: char| c == char::from(13u8) || c == char::from(10u8))
            .to_string();
        if token.is_empty() {
            return Err(format!("AGENT_AUTH_TOKEN_FILE {shown} holds no token"));
        }
        return Ok(Some(token));
    }
    Ok(env_token.filter(|t| !t.is_empty()))
}

pub async fn run(
    config: BridgeConfig,
    state: Arc<WorkerState>,
) -> Result<(), Box<dyn std::error::Error>> {
    let addr: SocketAddr = format!("{}:{}", config.bind_host, config.ws_port).parse()?;
    let listener = TcpListener::bind(addr).await?;
    state.set_ready(true);
    eprintln!("[bridge] listening on ws://{addr}{}", config.ws_path);

    let config = Arc::new(config);
    let registry = Arc::new(WorkspaceRegistry::new());

    loop {
        let (stream, peer) = listener.accept().await?;
        eprintln!("[bridge] connection from {peer}");
        let config = Arc::clone(&config);
        let state = Arc::clone(&state);
        let registry = Arc::clone(&registry);
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, &config, &state, &registry).await {
                eprintln!("[bridge] connection error: {e}");
            }
            state.decrement_connections();
            eprintln!("[bridge] {peer} disconnected");
        });
    }
}

async fn handle_connection(
    stream: TcpStream,
    config: &BridgeConfig,
    state: &WorkerState,
    registry: &Arc<WorkspaceRegistry>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ws = upgrade_websocket(stream, &config.ws_path, config.auth_token.as_deref()).await?;
    state.increment_connections();
    let (mut write, mut read) = ws.split();

    let ready_json = serde_json::to_string(&protocol::ready_envelope(&config.worker_id))?;
    write.send(Message::Text(ready_json.into())).await?;
    eprintln!("[bridge] sent ready");

    // Cancel channels keyed by task id, mirroring the kimi bridge's
    // `activeTasks` map. The read loop stays live while tasks run (task
    // handling is spawned, not awaited), so a mid-task `cancel` envelope
    // reaches its channel instead of queueing behind the task.
    //
    // Shared as `Arc<Mutex<…>>` so the spawned task handler can remove its
    // own entry on natural completion (WI#158): without shared ownership
    // the handler could not touch the map and every completed task would
    // leak a dead `oneshot::Sender` + its task-id `String` for the life of
    // the connection.
    let cancels: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // WI#324: one tool-proxy relay per hub connection. Inbound
    // tool_result/tool_catalog frames route to its waiters; when the
    // connection closes the relay fails every pending call fast (EF-9/EV-4).
    let relay = Arc::new(ToolProxyRelay::in_dir(config.tool_proxy_dir.clone()));

    // Outbound envelopes are funneled through a channel so spawned task
    // handlers never touch the sink: one writer task drains it while the
    // read loop keeps consuming inbound frames.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();
    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if write.send(msg).await.is_err() {
                break;
            }
        }
    });

    while let Some(Ok(msg)) = read.next().await {
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Ping(d) => {
                if out_tx.send(Message::Pong(d)).is_err() {
                    break;
                }
                continue;
            }
            Message::Close(_) => break,
            _ => continue,
        };

        let Ok(envelope) = serde_json::from_str::<Envelope>(&text) else {
            eprintln!("[bridge] invalid envelope");
            continue;
        };

        match envelope.msg_type.as_str() {
            "task_request" => {
                dispatch_task(envelope, config, &out_tx, &cancels, registry, &relay).await;
            }
            "tool_result" | "tool_catalog" => {
                // Hub replies to proxied tool calls; route to the relay's
                // waiter by call_id. Unroutable replies are ignored (the
                // relay's timeout is the shim's safety net).
                relay.handle_hub_frame(&envelope).await;
            }
            "cancel" => cancel_task(&envelope, &cancels).await,
            "ping" => {
                let json = serde_json::to_string(&protocol::pong_envelope())?;
                if out_tx.send(Message::Text(json.into())).is_err() {
                    break;
                }
            }
            other => eprintln!("[bridge] unknown message type: {other}"),
        }
    }

    // The hub connection is gone: fail every parked tool call at once
    // instead of letting shims wait out the relay timeout (EF-9/EV-4).
    relay.drop_hub().await;

    drop(out_tx);
    let _ = writer.await;
    Ok(())
}

/// Abort the task named by a `cancel` envelope. A cancel for an unknown or
/// already-finished task is logged and ignored (the kimi bridge behaves the
/// same); a known task's channel fires, the executor kills the pebble
/// subprocess, and the task handler emits its own `task_result` envelope.
async fn cancel_task(
    envelope: &Envelope,
    cancels: &Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
) {
    let payload: CancelPayload = match serde_json::from_value(envelope.payload.clone()) {
        Ok(payload) => payload,
        Err(e) => {
            eprintln!("[bridge] cancel: invalid payload: {e}");
            return;
        }
    };
    let mut map = cancels.lock().await;
    match map.remove(&payload.task_id) {
        Some(tx) => {
            // The receiver is dropped (task already finished) when `send`
            // fails; the entry was removed so the signal is not falsely
            // reported as sent.
            if tx.send(()).is_ok() {
                eprintln!("[bridge] task {}: cancel signal sent", payload.task_id);
            } else {
                eprintln!(
                    "[bridge] task {}: cancel entry stale (already finished)",
                    payload.task_id
                );
            }
        }
        None => eprintln!(
            "[bridge] cancel: task {} not found or already finished",
            payload.task_id
        ),
    }
}

async fn upgrade_websocket(
    stream: TcpStream,
    expected_path: &str,
    auth_token: Option<&str>,
) -> Result<WebSocketStream<TcpStream>, tokio_tungstenite::tungstenite::Error> {
    let path = expected_path.to_string();
    let token = auth_token.map(String::from);

    #[allow(clippy::result_large_err)]
    tokio_tungstenite::accept_hdr_async(stream, move |req: &Request, mut response: Response| {
        if req.uri().path() != path {
            return Err(reject(StatusCode::NOT_FOUND));
        }
        if let Some(ref expected) = token {
            let bearer = req
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .strip_prefix("Bearer ")
                .unwrap_or("");
            if bearer != expected.as_str() {
                return Err(reject(StatusCode::UNAUTHORIZED));
            }
        }
        let protocols = req
            .headers()
            .get("sec-websocket-protocol")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let Some(matched) = negotiate_subprotocol(protocols) else {
            return Err(reject(StatusCode::BAD_REQUEST));
        };
        // Echo the negotiated subprotocol back: the orchestrator's client
        // rejects the handshake ("Server sent no subprotocol") unless the server
        // sets Sec-WebSocket-Protocol in the upgrade response — and the echoed
        // value must be one the client actually offered.
        response.headers_mut().insert(
            "sec-websocket-protocol",
            tokio_tungstenite::tungstenite::http::HeaderValue::from_static(matched),
        );
        Ok(response)
    })
    .await
}

/// Pick the subprotocol to echo from a comma-separated client offer.
/// Only the primary `lunarwing-agent-v1` name is accepted; the removed
/// `ironclaw-agent-v1` alias is not.
fn negotiate_subprotocol(offer: &str) -> Option<&'static str> {
    let offered: Vec<&str> = offer.split(',').map(str::trim).collect();
    if offered.contains(&protocol::SUBPROTOCOL) {
        Some(protocol::SUBPROTOCOL)
    } else {
        None
    }
}

fn reject(status: StatusCode) -> http::Response<Option<String>> {
    http::Response::builder()
        .status(status)
        .body(None)
        .expect("response")
}

async fn dispatch_task(
    envelope: Envelope,
    config: &BridgeConfig,
    out_tx: &Outbound,
    cancels: &Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
    registry: &Arc<WorkspaceRegistry>,
    relay: &Arc<ToolProxyRelay>,
) {
    let request: TaskRequest = match serde_json::from_value(envelope.payload) {
        Ok(request) => request,
        Err(e) => {
            eprintln!("[bridge] task_request: invalid payload: {e}");
            return;
        }
    };

    // Resolve and confine the project dir before spawning anything: a
    // daemon-supplied path that escapes WORKSPACE_ROOT must fail closed with
    // an error envelope instead of pointing the pebble CLI at an arbitrary
    // host directory.
    let requested_dir = request
        .context
        .project_dir
        .clone()
        .unwrap_or_else(|| config.workspace_root.clone());
    let workspace = match workspace_path::resolve_task_workspace(
        &request.context,
        std::path::Path::new(&config.workspace_root),
    ) {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!(
                "[bridge] task {}: rejected workspace path '{}': {e}",
                request.task_id, requested_dir
            );
            send_result(
                out_tx,
                &protocol::result_envelope(&request.task_id, "error", "", Some(&e.to_string()), 0),
            );
            return;
        }
    };

    if let Err(owner) = registry
        .try_register(workspace.clone(), &request.task_id)
        .await
    {
        let error = format!(
            "Workspace {} is already being modified by task {owner}. \
             Cancel or finish that task before dispatching another task to this workspace.",
            workspace.display()
        );
        eprintln!(
            "[bridge] task {}: rejected, workspace owned by {owner}",
            request.task_id
        );
        send_result(
            out_tx,
            &protocol::result_envelope(&request.task_id, "error", "", Some(&error), 0),
        );
        return;
    }

    eprintln!("[bridge] task {}: starting", request.task_id);

    // WI#324: start the task's tool-proxy relay slot BEFORE the executor
    // spawns pebble — pebble lists its MCP servers' tools at startup, so the
    // socket must exist by then. Setup failure degrades the task to
    // tool-less rather than failing it (EF-2); the env is only injected when
    // the slot came up.
    let proxy = relay.start_task(&request.task_id, out_tx.clone()).await;
    let proxy_env = proxy
        .as_ref()
        .map(|handle| tool_proxy::proxy_env_for_task(&request.task_id, &handle.socket_path));

    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let (cancel_tx, cancel_rx) = oneshot::channel();
    cancels
        .lock()
        .await
        .insert(request.task_id.clone(), cancel_tx);

    let exec_config = ExecutorConfig {
        pebble_bin: config.pebble_bin.clone(),
        model: config.model.clone(),
        permission_mode: config.permission_mode.clone(),
        workspace_root: config.workspace_root.clone(),
    };

    // Task handling is spawned, not awaited: the read loop must keep
    // consuming frames so mid-task `cancel` envelopes are received while the
    // pebble subprocess runs. The handler owns the workspace claim and removes
    // its own cancel-map entry on exit (WI#158: previously the entry was never
    // removed on natural completion, leaking a dead `oneshot::Sender` per
    // task). It then forwards every executor envelope through the outbound
    // channel.
    {
        let registry = Arc::clone(registry);
        let cancels = Arc::clone(cancels);
        let relay = Arc::clone(relay);
        let task_id = request.task_id.clone();
        let out_tx = out_tx.clone();
        tokio::spawn(async move {
            executor::run_task(
                &exec_config,
                &request,
                event_tx,
                cancel_rx,
                proxy_env.as_ref(),
            )
            .await;
            // The task is over: close its relay slot (fails any call still
            // in flight) before releasing the workspace claim.
            relay.close_task(&task_id).await;
            registry.unregister(&workspace, &task_id).await;
            // Remove the cancel entry so completed tasks do not leak. A
            // cancel arriving after this point finds `None` and logs
            // "not found or already finished" — which is then accurate.
            cancels.lock().await.remove(&task_id);
            while let Some(event) = event_rx.recv().await {
                let env = match &event {
                    ExecutorEvent::Progress(e) | ExecutorEvent::Result(e) => e,
                };
                let json = serde_json::to_string(env).unwrap_or_default();
                if out_tx.send(Message::Text(json.into())).is_err() {
                    break;
                }
            }
        });
    }
}

fn send_result(out_tx: &Outbound, envelope: &Envelope) {
    match serde_json::to_string(&envelope) {
        Ok(json) => {
            if out_tx.send(Message::Text(json.into())).is_err() {
                eprintln!("[bridge] outbound channel closed; result dropped");
            }
        }
        Err(e) => eprintln!("[bridge] failed to serialize result envelope: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// KW-8/LD-10: the hub-handshake credential resolves fail-closed. The
    /// file wins over the env var, only a trailing CR/LF is trimmed, an
    /// empty/newline-only/missing file is a startup error (never a silent
    /// fallback to the already-unset env, which would disable the bearer
    /// check), and with no file pointer the dev env fallback still works.
    #[test]
    fn bridge_auth_token_file_is_preferred_and_fails_closed() {
        let dir = std::env::temp_dir().join(format!(
            "pb-ld10-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("create dir");

        let good = dir.join("good");
        std::fs::write(&good, "file-token\n").expect("write token file");
        // The file wins over a populated env var.
        assert_eq!(
            resolve_bridge_auth_token(
                Some(good.clone().into_os_string()),
                Some("env-token".to_string()),
            )
            .expect("file read"),
            Some("file-token".to_string())
        );
        // Only trailing CR/LF is trimmed; interior and other whitespace
        // survives (the token itself may contain spaces).
        let crlf = dir.join("crlf");
        std::fs::write(&crlf, "tok\r\n\n").expect("write crlf token");
        assert_eq!(
            resolve_bridge_auth_token(Some(crlf.into_os_string()), None).expect("trimmed read"),
            Some("tok".to_string())
        );
        let interior = dir.join("interior");
        std::fs::write(&interior, "a b\tc\n").expect("write interior token");
        assert_eq!(
            resolve_bridge_auth_token(Some(interior.into_os_string()), None)
                .expect("interior whitespace kept"),
            Some("a b\tc".to_string())
        );

        // An empty or newline-only file is an error.
        let empty = dir.join("empty");
        std::fs::write(&empty, "").expect("write empty file");
        let error = resolve_bridge_auth_token(Some(empty.into_os_string()), Some("env".into()))
            .expect_err("empty file must fail");
        assert!(
            error.contains("holds no token"),
            "unexpected error: {error}"
        );
        let newline = dir.join("newline");
        std::fs::write(&newline, "\r\n\n").expect("write newline-only file");
        assert!(resolve_bridge_auth_token(Some(newline.into_os_string()), None).is_err());

        // A missing file is an error even with the env fallback populated.
        let missing = dir.join("missing");
        let error = resolve_bridge_auth_token(
            Some(missing.into_os_string()),
            Some("env-token".to_string()),
        )
        .expect_err("unreadable file must fail");
        assert!(
            error.contains("cannot read AGENT_AUTH_TOKEN_FILE"),
            "error names the file pointer: {error}"
        );

        // With no file pointer, the dev env fallback still works (and an
        // empty env var means no auth, as before).
        assert_eq!(
            resolve_bridge_auth_token(None, Some("env-token".to_string())).expect("env fallback"),
            Some("env-token".to_string())
        );
        assert_eq!(
            resolve_bridge_auth_token(None, Some(String::new())).expect("empty env"),
            None
        );
        assert_eq!(
            resolve_bridge_auth_token(None, None).expect("nothing set"),
            None
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn registry_rejects_overlapping_workspaces_and_frees_on_unregister() {
        let registry = WorkspaceRegistry::new();
        let project = PathBuf::from("/workspace/project");

        assert!(registry
            .try_register(project.clone(), "task-1")
            .await
            .is_ok());
        assert_eq!(
            registry
                .try_register(PathBuf::from("/workspace/project/src"), "task-2")
                .await,
            Err("task-1".to_string())
        );
        assert!(registry
            .try_register(PathBuf::from("/workspace/other"), "task-2")
            .await
            .is_ok());

        // Unregister under a different task id must not free the workspace.
        registry.unregister(&project, "task-other").await;
        assert_eq!(
            registry.try_register(project.clone(), "task-3").await,
            Err("task-1".to_string())
        );

        registry.unregister(&project, "task-1").await;
        assert!(registry.try_register(project, "task-4").await.is_ok());
    }

    #[tokio::test]
    async fn registry_claims_are_atomic_under_concurrency() {
        // The overlap check and the insert share one critical section, so
        // concurrent dispatches racing on the same subtree must yield exactly
        // one winner; losers report that winner as the owner.
        let registry = Arc::new(WorkspaceRegistry::new());
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let registry = Arc::clone(&registry);
                let workspace = PathBuf::from("/workspace/shared");
                tokio::spawn(
                    async move { registry.try_register(workspace, &format!("task-{i}")).await },
                )
            })
            .collect();

        let results = futures_util::future::join_all(handles).await;
        let mut owners: Vec<String> = Vec::new();
        let mut winners = 0;
        for result in results {
            match result.expect("join") {
                Ok(()) => winners += 1,
                Err(owner) => owners.push(owner),
            }
        }
        assert_eq!(winners, 1, "exactly one concurrent claim may win");
        assert!(
            owners.iter().all(|owner| owner == &owners[0]),
            "every loser must report the same owner"
        );
    }

    #[test]
    fn negotiate_prefers_primary() {
        assert_eq!(
            negotiate_subprotocol("lunarwing-agent-v1"),
            Some(protocol::SUBPROTOCOL)
        );
    }

    #[test]
    fn negotiate_accepts_primary_alone() {
        assert_eq!(
            negotiate_subprotocol("lunarwing-agent-v1"),
            Some(protocol::SUBPROTOCOL)
        );
    }

    #[test]
    fn negotiate_ignores_legacy_alias() {
        // The `ironclaw-agent-v1` alias was removed: an old daemon offering
        // only that name must be rejected, and it must not shadow the
        // primary name when both appear.
        assert_eq!(negotiate_subprotocol("ironclaw-agent-v1"), None);
        assert_eq!(
            negotiate_subprotocol("lunarwing-agent-v1, ironclaw-agent-v1"),
            Some(protocol::SUBPROTOCOL)
        );
        assert_eq!(
            negotiate_subprotocol("ironclaw-agent-v1, lunarwing-agent-v1"),
            Some(protocol::SUBPROTOCOL)
        );
    }

    #[test]
    fn negotiate_rejects_unknown_offers() {
        assert_eq!(negotiate_subprotocol(""), None);
        assert_eq!(negotiate_subprotocol("some-other-proto"), None);
    }

    fn test_config() -> BridgeConfig {
        BridgeConfig {
            bind_host: "127.0.0.1".to_string(),
            ws_port: 0,
            ws_path: "/ws/agent".to_string(),
            health_port: 0,
            auth_token: None,
            worker_id: "worker-test".to_string(),
            pebble_bin: "/nonexistent-pebble".to_string(),
            model: "m".to_string(),
            permission_mode: "default".to_string(),
            workspace_root: workspace_root_str(),
            tool_proxy_dir: unique_proxy_dir(),
        }
    }

    /// A per-test relay dir, so tests never touch the shared default
    /// `/tmp/lunarwing-tool-proxy` (short name: unix socket paths cap at
    /// about 108 bytes).
    fn unique_proxy_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static PROXY_COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = PROXY_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("pbp-{}-{unique}", std::process::id()))
    }

    fn test_relay() -> (Arc<ToolProxyRelay>, PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static RELAY_COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = RELAY_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "pebble-bridge-relay-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create relay dir");
        (
            Arc::new(ToolProxyRelay::with_overrides(
                dir.clone(),
                std::time::Duration::from_secs(5),
            )),
            dir,
        )
    }

    /// A stub pebble binary that just sleeps, so a task stays alive while
    /// the test drives its tool-proxy relay slot. `exec` keeps the slept
    /// process in the killed pid so cancel tears the pipes down at once.
    fn sleeping_pebble_stub(dir: &Path) -> String {
        let stub = dir.join("pebble-stub.sh");
        std::fs::write(&stub, "#!/bin/sh\nexec sleep 30\n").expect("write stub");
        std::fs::set_permissions(&stub, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod stub");
        stub.to_str().expect("utf-8 stub").to_string()
    }

    fn workspace_root_str() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("pebble-bridge-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create workspace root");
        root.to_str().expect("utf-8 path").to_string()
    }

    fn request_envelope(task_id: &str, project_dir: Option<&str>) -> Envelope {
        protocol::create_envelope(
            "task_request",
            protocol::TaskRequest {
                task_id: task_id.to_string(),
                prompt: "work".to_string(),
                context: protocol::TaskContext {
                    project_dir: project_dir.map(String::from),
                    ..Default::default()
                },
                timeout_ms: Some(30_000),
            },
        )
    }

    async fn next_envelope(rx: &mut mpsc::UnboundedReceiver<Message>) -> Envelope {
        let timeout = std::time::Duration::from_secs(5);
        let msg = tokio::time::timeout(timeout, rx.recv())
            .await
            .expect("envelope within timeout")
            .expect("channel open");
        let Message::Text(text) = msg else {
            panic!("expected text message, got {msg:?}");
        };
        serde_json::from_str::<Envelope>(text.as_str()).expect("valid envelope JSON")
    }

    #[tokio::test]
    async fn cancel_fires_the_matching_tasks_channel() {
        let cancels: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (tx_a, mut rx_a) = oneshot::channel();
        let (tx_b, mut rx_b) = oneshot::channel();
        cancels.lock().await.insert("task-a".to_string(), tx_a);
        cancels.lock().await.insert("task-b".to_string(), tx_b);

        cancel_task(&protocol::cancel_envelope("task-a"), &cancels).await;

        assert!(rx_a.try_recv().is_ok(), "task-a cancel must fire");
        assert!(
            rx_b.try_recv().is_err(),
            "task-b cancel must not fire from task-a's envelope"
        );
        assert!(
            cancels.lock().await.contains_key("task-b"),
            "task-b stays registered"
        );
        assert!(
            !cancels.lock().await.contains_key("task-a"),
            "task-a entry consumed"
        );
    }

    #[tokio::test]
    async fn cancel_for_unknown_task_is_ignored() {
        let cancels: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (tx, mut rx) = oneshot::channel();
        cancels.lock().await.insert("task-live".to_string(), tx);

        cancel_task(&protocol::cancel_envelope("task-ghost"), &cancels).await;

        assert!(rx.try_recv().is_err(), "no cancel may fire");
        assert!(cancels.lock().await.contains_key("task-live"));
    }

    #[tokio::test]
    async fn dispatch_returns_without_waiting_for_the_task() {
        // Regression for WI#101: dispatch must come back to the read loop
        // immediately. `pebble_bin` points at a nonexistent binary, so the
        // executor's result (a spawn-failure error envelope) arrives later
        // through the outbound channel, never inline.
        let root = test_config();
        let registry = Arc::new(WorkspaceRegistry::new());
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let cancels: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (relay, _relay_dir) = test_relay();

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        dispatch_task(
            request_envelope("task-1", Some(&root.workspace_root)),
            &root,
            &out_tx,
            &cancels,
            &registry,
            &relay,
        )
        .await;
        assert!(
            tokio::time::Instant::now() < deadline,
            "dispatch must not await the task"
        );

        // The executor still reports the spawn failure asynchronously.
        let result = next_envelope(&mut out_rx).await;
        assert_eq!(result.msg_type, "task_result");
        let payload: protocol::TaskResult =
            serde_json::from_value(result.payload).expect("task_result payload");
        assert_eq!(payload.task_id, "task-1");
        assert_eq!(payload.status, "error");
        assert!(payload.error.is_some());

        // Workspace claim is released once the task handler finishes.
        let workspace = std::path::PathBuf::from(
            std::path::Path::new(&root.workspace_root)
                .canonicalize()
                .expect("canonical root")
                .to_str()
                .expect("utf-8")
                .to_string(),
        );
        for _ in 0..50 {
            if registry
                .try_register(workspace.clone(), "task-next")
                .await
                .is_ok()
            {
                registry.unregister(&workspace, "task-next").await;
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("workspace claim was never released after the task finished");
    }

    #[tokio::test]
    async fn dispatch_rejecting_escaping_project_dir_leaves_no_cancel_entry() {
        let root = test_config();
        let registry = Arc::new(WorkspaceRegistry::new());
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let cancels: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (relay, _relay_dir) = test_relay();

        let outside = std::env::temp_dir().join("pebble-bridge-outside-escape");
        std::fs::create_dir_all(&outside).expect("create outside dir");
        let outside_str = outside.to_str().expect("utf-8 path").to_string();

        dispatch_task(
            request_envelope("task-escape", Some(&outside_str)),
            &root,
            &out_tx,
            &cancels,
            &registry,
            &relay,
        )
        .await;

        let rejection = next_envelope(&mut out_rx).await;
        assert_eq!(rejection.msg_type, "task_result");
        let payload: protocol::TaskResult =
            serde_json::from_value(rejection.payload).expect("task_result payload");
        assert_eq!(payload.status, "error");
        assert!(payload.error.as_deref().unwrap_or("").contains("escapes"));
        assert!(
            cancels.lock().await.is_empty(),
            "rejected task must register no cancel"
        );
        std::fs::remove_dir_all(&outside).ok();
    }

    #[tokio::test]
    async fn cron_dispatch_bootstraps_missing_directory_before_executor() {
        let config = test_config();
        let registry = Arc::new(WorkspaceRegistry::new());
        let cancels = Arc::new(Mutex::new(HashMap::new()));
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let (relay, _relay_dir) = test_relay();
        let mut request = request_envelope("cron-first", Some("missions/pr-digest"));
        request.payload["context"]["cron_tick"] = serde_json::json!({"event":"cron.tick"});
        dispatch_task(request, &config, &out_tx, &cancels, &registry, &relay).await;
        let result = next_envelope(&mut out_rx).await;
        assert!(std::path::Path::new(&config.workspace_root)
            .join("missions/pr-digest")
            .is_dir());
        // The fixture deliberately has no pebble executable: it must reach
        // the executor, rather than fail the directory preflight (DN-2).
        assert_eq!(result.msg_type, "task_result");
        assert!(result.payload["error"]
            .as_str()
            .unwrap()
            .starts_with("failed to spawn pebble:"));
        std::fs::remove_dir_all(config.workspace_root).unwrap();
    }

    /// Regression for WI#158: every task that completes naturally (not via
    /// cancel) must remove its own cancel-map entry. Previously the spawned
    /// handler only released the workspace claim, leaking a dead
    /// `oneshot::Sender` per task for the life of the connection. After N
    /// tasks finish on one connection the map must be empty.
    #[tokio::test]
    async fn cancels_map_is_empty_after_tasks_complete() {
        let root = test_config();
        let registry = Arc::new(WorkspaceRegistry::new());
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let cancels: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (relay, _relay_dir) = test_relay();

        // Dispatch 3 tasks with distinct workspaces so they do not collide
        // on the workspace registry. Each fails fast (nonexistent pebble bin)
        // and the handler removes its cancel entry on exit.
        for i in 0..3 {
            let task_workspace =
                std::env::temp_dir().join(format!("pebble-wi158-task-{i}-{}", std::process::id()));
            std::fs::create_dir_all(&task_workspace).expect("create task workspace");
            let ws_str = task_workspace.to_str().expect("utf-8 path").to_string();

            dispatch_task(
                request_envelope(&format!("task-{i}"), Some(&ws_str)),
                &root,
                &out_tx,
                &cancels,
                &registry,
                &relay,
            )
            .await;
        }

        // Drain the 3 error results so the handlers finish.
        for _ in 0..3 {
            let result = next_envelope(&mut out_rx).await;
            assert_eq!(result.msg_type, "task_result");
        }

        // Poll until all handlers have removed their entries or timeout.
        for _ in 0..100 {
            if cancels.lock().await.is_empty() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!(
            "cancels map leaked entries after tasks completed: {:?}",
            cancels.lock().await.keys().collect::<Vec<_>>()
        );
    }

    /// Regression for WI#158 log-accuracy nit: when a cancel envelope arrives
    /// for a task whose receiver was already dropped (task finished but the
    /// stale entry somehow lingered), the log must not say "cancel signal
    /// sent". This is a defensive test — under the shared-map fix the entry is
    /// removed on completion, so a post-completion cancel finds `None` and logs
    /// "not found or already finished" instead.
    #[tokio::test]
    async fn cancel_against_dead_receiver_does_not_claim_signal_sent() {
        let cancels: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = oneshot::channel();
        // Simulate a finished task: receiver is dropped, but the sender is
        // still in the map (the pre-fix leak condition).
        cancels.lock().await.insert("task-dead".to_string(), tx);
        drop(rx);

        cancel_task(&protocol::cancel_envelope("task-dead"), &cancels).await;

        // The entry is removed regardless (send-then-remove or remove-then-
        // send), but the key point is the receiver got nothing.
        assert!(
            cancels.lock().await.is_empty(),
            "dead entry must be removed"
        );
    }

    async fn ipc_call(socket_path: &Path, request: &serde_json::Value) -> serde_json::Value {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut conn = tokio::net::UnixStream::connect(socket_path)
            .await
            .expect("connect to task socket");
        conn.write_all(serde_json::to_string(request).unwrap().as_bytes())
            .await
            .expect("write request");
        conn.write_all(b"\n").await.expect("write newline");
        let mut reply = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            conn.read_to_end(&mut reply),
        )
        .await
        .expect("reply within timeout")
        .expect("read reply");
        serde_json::from_str(String::from_utf8(reply).expect("utf-8").trim()).expect("reply JSON")
    }

    async fn read_until(rx: &mut mpsc::UnboundedReceiver<Message>, msg_type: &str) -> Envelope {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let env = tokio::time::timeout(deadline - tokio::time::Instant::now(), rx.recv())
                .await
                .expect("frame before deadline")
                .expect("channel open");
            let Message::Text(text) = env else { continue };
            let env: Envelope = serde_json::from_str(text.as_str()).expect("envelope");
            if env.msg_type == msg_type {
                return env;
            }
        }
    }

    /// WI#324 dispatch-level pin: an accepted task gets a live relay slot
    /// (socket on disk before the executor spawns), an IPC call emits a
    /// hub-bound `tool_call` frame carrying the task binding, the routed
    /// `tool_result` resolves the shim's call, and task teardown closes the
    /// slot (socket removed).
    #[tokio::test]
    async fn dispatched_task_gets_a_working_relay_slot() {
        let root = test_config();
        let stub = sleeping_pebble_stub(Path::new(&root.workspace_root));
        let config = BridgeConfig {
            pebble_bin: stub,
            ..root
        };
        let registry = Arc::new(WorkspaceRegistry::new());
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let cancels: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (relay, relay_dir) = test_relay();

        dispatch_task(
            request_envelope("task-relay", None),
            &config,
            &out_tx,
            &cancels,
            &registry,
            &relay,
        )
        .await;
        let socket_path = crate::tool_proxy::task_socket_path_in(&relay_dir, "task-relay");
        assert!(socket_path.exists(), "relay socket must exist pre-spawn");

        let client = tokio::spawn({
            let socket_path = socket_path.clone();
            async move {
                ipc_call(
                    &socket_path,
                    &serde_json::json!({"op": "call", "call_id": "b-1", "name": "memory_read",
                                        "arguments": {"path": "x"}}),
                )
                .await
            }
        });
        let frame = read_until(&mut out_rx, "tool_call").await;
        assert_eq!(frame.payload["task_id"], "task-relay");
        assert_eq!(frame.payload["call_id"], "b-1");
        assert_eq!(frame.payload["name"], "memory_read");
        assert!(
            relay
                .handle_hub_frame(&protocol::create_envelope(
                    "tool_result",
                    serde_json::json!({"call_id": "b-1", "status": "success", "output": "v", "error": null}),
                ))
                .await
        );
        let reply = client.await.expect("client joins");
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["status"], "success");
        assert_eq!(reply["output"], "v");

        cancel_task(&protocol::cancel_envelope("task-relay"), &cancels).await;
        let result = read_until(&mut out_rx, "task_result").await;
        assert_eq!(result.payload["status"], "cancelled");
        for _ in 0..50 {
            if !socket_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!socket_path.exists(), "teardown removed the socket");
        std::fs::remove_dir_all(&relay_dir).ok();
        std::fs::remove_dir_all(&config.workspace_root).ok();
    }

    type WsStream = tokio_tungstenite::WebSocketStream<TcpStream>;

    async fn ws_send_envelope(ws: &mut WsStream, envelope: &Envelope) {
        ws.send(Message::Text(
            serde_json::to_string(envelope).unwrap().into(),
        ))
        .await
        .expect("send envelope");
    }

    async fn ws_read_until(ws: &mut WsStream, msg_type: &str, timeout_secs: u64) -> Envelope {
        tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
            loop {
                let msg = ws.next().await.expect("frame").expect("ok");
                let Message::Text(text) = msg else { continue };
                let env: Envelope = serde_json::from_str(text.as_str()).expect("envelope");
                if env.msg_type == msg_type {
                    break env;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{msg_type} frame within {timeout_secs}s"))
    }

    /// WI#324 read-loop routing pin (KL-5 analog): the whole path over a
    /// real WebSocket — `task_request` in, `tool_call` out, `tool_result`
    /// back in, shim reply out — proves the read loop's
    /// `tool_result`/`tool_catalog` arm actually reaches the relay (a dropped
    /// arm leaves the IPC call parked until the relay timeout).
    #[tokio::test]
    async fn tool_frames_route_over_a_real_connection() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let root = test_config();
        let stub = sleeping_pebble_stub(Path::new(&root.workspace_root));
        let config = BridgeConfig {
            pebble_bin: stub,
            ..root
        };
        let state = crate::health::WorkerState::new(&config.worker_id);
        let registry = Arc::new(WorkspaceRegistry::new());
        let proxy_dir = config.tool_proxy_dir.clone();

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn({
            let config = BridgeConfig {
                workspace_root: config.workspace_root.clone(),
                pebble_bin: config.pebble_bin.clone(),
                ..config
            };
            async move {
                let (stream, _) = listener.accept().await.expect("accept");
                handle_connection(stream, &config, &state, &registry)
                    .await
                    .expect("connection serves");
            }
        });

        let stream = TcpStream::connect(addr).await.expect("connect");
        let url = format!("ws://{addr}/ws/agent");
        let mut request = url.into_client_request().expect("client request");
        request.headers_mut().insert(
            "sec-websocket-protocol",
            tokio_tungstenite::tungstenite::http::HeaderValue::from_static("lunarwing-agent-v1"),
        );
        let (mut ws, _) = tokio_tungstenite::client_async(request, stream)
            .await
            .expect("ws handshake");

        // ready arrives first.
        let ready = ws_read_until(&mut ws, "ready", 5).await;
        assert_eq!(ready.payload["worker_id"], "worker-test");

        let task_id = format!("ws-task-{}", std::process::id());
        let request_env = protocol::create_envelope(
            "task_request",
            protocol::TaskRequest {
                task_id: task_id.clone(),
                prompt: "work".to_string(),
                context: protocol::TaskContext::default(),
                timeout_ms: Some(30_000),
            },
        );
        ws_send_envelope(&mut ws, &request_env).await;

        // The connection's relay lives in the per-test proxy dir
        // (test_config's unique_proxy_dir — LD-17: never the shared
        // /tmp/lunarwing-tool-proxy); the task id namespaces the socket.
        let socket_path = crate::tool_proxy::task_socket_path_in(&proxy_dir, &task_id);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while !socket_path.exists() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "relay socket never appeared"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let client = tokio::spawn({
            let socket_path = socket_path.clone();
            async move {
                ipc_call(
                    &socket_path,
                    &serde_json::json!({"op": "call", "call_id": "ws-1", "name": "knowledge_search",
                                        "arguments": {"query": "q"}}),
                )
                .await
            }
        });

        // The relay's tool_call frame comes out on the wire.
        let tool_call = ws_read_until(&mut ws, "tool_call", 5).await;
        assert_eq!(tool_call.payload["task_id"], task_id.as_str());
        assert_eq!(tool_call.payload["call_id"], "ws-1");

        // Answer it as the hub would; the shim's IPC call must resolve.
        let result_env = protocol::create_envelope(
            "tool_result",
            serde_json::json!({"call_id": "ws-1", "status": "denied", "output": "", "error": "not in tool_allowlist"}),
        );
        ws_send_envelope(&mut ws, &result_env).await;
        let reply = client.await.expect("client joins");
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["status"], "denied");
        assert_eq!(reply["error"], "not in tool_allowlist");

        // Cancel the task so the sleeping stub dies, and the cancelled
        // result comes back over the wire.
        ws_send_envelope(&mut ws, &protocol::cancel_envelope(&task_id)).await;
        let cancelled = ws_read_until(&mut ws, "task_result", 10).await;
        assert_eq!(cancelled.payload["status"], "cancelled");

        drop(ws);
        server.await.expect("server joins");
        std::fs::remove_dir_all(&config.workspace_root).ok();
    }

    /// M24 pin: the bridge hands the issued relay slot to the pebble child.
    /// The stub prints the two proxy keys; the task output must carry the
    /// slot's real socket path and the task id.
    #[tokio::test]
    async fn dispatched_pebble_sees_the_issued_proxy_env() {
        let root = test_config();
        let stub = Path::new(&root.workspace_root).join("pebble-env-stub.sh");
        let script = r#"#!/bin/sh
echo "SOCK=$LUNARWING_TOOL_PROXY_SOCKET"
echo "TASK=$LUNARWING_TASK_ID"
"#;
        std::fs::write(&stub, script).expect("write stub");
        std::fs::set_permissions(&stub, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod stub");
        let config = BridgeConfig {
            pebble_bin: stub.to_str().expect("utf-8 stub").to_string(),
            ..root
        };
        let registry = Arc::new(WorkspaceRegistry::new());
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let cancels: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (relay, relay_dir) = test_relay();
        dispatch_task(
            request_envelope("task-env", None),
            &config,
            &out_tx,
            &cancels,
            &registry,
            &relay,
        )
        .await;
        let result = read_until(&mut out_rx, "task_result").await;
        let output = result.payload["output"]
            .as_str()
            .expect("output")
            .to_string();
        let socket = crate::tool_proxy::task_socket_path_in(&relay_dir, "task-env");
        assert!(
            output.contains(&format!("SOCK={}", socket.display())),
            "the child must see the issued socket: {output}"
        );
        assert!(
            output.contains("TASK=task-env"),
            "the child must see the task id: {output}"
        );
        std::fs::remove_dir_all(&relay_dir).ok();
        std::fs::remove_dir_all(&config.workspace_root).ok();
    }

    /// JU-3/KL-9 analog: the read loop's `drop_hub` call site. A hub that
    /// goes away mid-call must fail the parked IPC call at once, not after
    /// the relay timeout (300 s by default for the connection's relay).
    #[tokio::test]
    async fn hub_close_fails_a_parked_call_over_a_real_connection() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let root = test_config();
        let stub = sleeping_pebble_stub(Path::new(&root.workspace_root));
        let config = BridgeConfig {
            pebble_bin: stub,
            ..root
        };
        let state = crate::health::WorkerState::new(&config.worker_id);
        let registry = Arc::new(WorkspaceRegistry::new());
        let proxy_dir = config.tool_proxy_dir.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let workspace_root = config.workspace_root.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_connection(stream, &config, &state, &registry).await;
        });
        let stream = TcpStream::connect(addr).await.expect("connect");
        let mut request = format!("ws://{addr}/ws/agent")
            .into_client_request()
            .expect("client request");
        request.headers_mut().insert(
            "sec-websocket-protocol",
            tokio_tungstenite::tungstenite::http::HeaderValue::from_static("lunarwing-agent-v1"),
        );
        let (mut ws, _) = tokio_tungstenite::client_async(request, stream)
            .await
            .expect("ws handshake");
        let _ = ws_read_until(&mut ws, "ready", 5).await;
        let task_id = format!("ws-drop-{}", std::process::id());
        let request_env = protocol::create_envelope(
            "task_request",
            protocol::TaskRequest {
                task_id: task_id.clone(),
                prompt: "work".to_string(),
                context: protocol::TaskContext::default(),
                timeout_ms: Some(30_000),
            },
        );
        ws_send_envelope(&mut ws, &request_env).await;
        let socket_path = crate::tool_proxy::task_socket_path_in(&proxy_dir, &task_id);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while !socket_path.exists() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "relay socket never appeared"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let client = tokio::spawn({
            let socket_path = socket_path.clone();
            async move {
                ipc_call(
                    &socket_path,
                    &serde_json::json!({"op": "call", "call_id": "drop-1", "name": "memory_read"}),
                )
                .await
            }
        });
        let _ = ws_read_until(&mut ws, "tool_call", 5).await;
        // The hub goes away without answering.
        drop(ws);
        let reply = client.await.expect("client joins");
        assert_eq!(
            reply["error"],
            "hub connection closed before the tool reply arrived"
        );
        server.abort();
        std::fs::remove_dir_all(&workspace_root).ok();
    }
}
