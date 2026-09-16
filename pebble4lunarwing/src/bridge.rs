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
}

/// Outbound envelopes for one WebSocket connection, drained by the writer
/// task so no code path needs to own the sink while a task runs.
type Outbound = mpsc::UnboundedSender<Message>;

impl BridgeConfig {
    pub fn from_env() -> Self {
        Self {
            bind_host: env_or("WS_BIND_HOST", "0.0.0.0"),
            ws_port: env_or("WS_PORT", "9090").parse().unwrap_or(9090),
            ws_path: env_or("WS_PATH", "/ws/agent"),
            health_port: env_or("HEALTH_PORT", "8443").parse().unwrap_or(8443),
            auth_token: std::env::var("AGENT_AUTH_TOKEN")
                .ok()
                .filter(|t| !t.is_empty()),
            worker_id: env_or("LUNARWING_WORKER_ID", "worker-pebble-01"),
            pebble_bin: env_or("PEBBLE_BIN", "pebble"),
            model: env_or("PEBBLE_MODEL", "openai/gpt-5.2"),
            permission_mode: env_or("PEBBLE_PERMISSION_MODE", "danger-full-access"),
            workspace_root: env_or("WORKSPACE_ROOT", "/workspace"),
        }
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
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
    let mut cancels: HashMap<String, oneshot::Sender<()>> = HashMap::new();

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
                dispatch_task(envelope, config, &out_tx, &mut cancels, registry).await;
            }
            "cancel" => cancel_task(&envelope, &mut cancels),
            "ping" => {
                let json = serde_json::to_string(&protocol::pong_envelope())?;
                if out_tx.send(Message::Text(json.into())).is_err() {
                    break;
                }
            }
            other => eprintln!("[bridge] unknown message type: {other}"),
        }
    }

    drop(out_tx);
    let _ = writer.await;
    Ok(())
}

/// Abort the task named by a `cancel` envelope. A cancel for an unknown or
/// already-finished task is logged and ignored (the kimi bridge behaves the
/// same); a known task's channel fires, the executor kills the pebble
/// subprocess, and the task handler emits its own `task_result` envelope.
fn cancel_task(envelope: &Envelope, cancels: &mut HashMap<String, oneshot::Sender<()>>) {
    let payload: CancelPayload = match serde_json::from_value(envelope.payload.clone()) {
        Ok(payload) => payload,
        Err(e) => {
            eprintln!("[bridge] cancel: invalid payload: {e}");
            return;
        }
    };
    match cancels.remove(&payload.task_id) {
        Some(tx) => {
            let _ = tx.send(());
            eprintln!("[bridge] task {}: cancel signal sent", payload.task_id);
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
    cancels: &mut HashMap<String, oneshot::Sender<()>>,
    registry: &Arc<WorkspaceRegistry>,
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
    let workspace = match workspace_path::resolve_workspace_path(
        &requested_dir,
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

    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let (cancel_tx, cancel_rx) = oneshot::channel();
    cancels.insert(request.task_id.clone(), cancel_tx);

    let exec_config = ExecutorConfig {
        pebble_bin: config.pebble_bin.clone(),
        model: config.model.clone(),
        permission_mode: config.permission_mode.clone(),
        workspace_root: config.workspace_root.clone(),
    };

    // Task handling is spawned, not awaited: the read loop must keep
    // consuming frames so mid-task `cancel` envelopes are received while the
    // pebble subprocess runs. The handler owns the workspace claim and the
    // cancel-map entry for the task's lifetime and clears both on exit, then
    // forwards every executor envelope through the outbound channel.
    {
        let registry = Arc::clone(registry);
        let task_id = request.task_id.clone();
        let out_tx = out_tx.clone();
        tokio::spawn(async move {
            executor::run_task(&exec_config, &request, event_tx, cancel_rx).await;
            registry.unregister(&workspace, &task_id).await;
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
        }
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
        let mut cancels = HashMap::new();
        let (tx_a, mut rx_a) = oneshot::channel();
        let (tx_b, mut rx_b) = oneshot::channel();
        cancels.insert("task-a".to_string(), tx_a);
        cancels.insert("task-b".to_string(), tx_b);

        cancel_task(&protocol::cancel_envelope("task-a"), &mut cancels);

        assert!(rx_a.try_recv().is_ok(), "task-a cancel must fire");
        assert!(
            rx_b.try_recv().is_err(),
            "task-b cancel must not fire from task-a's envelope"
        );
        assert!(cancels.contains_key("task-b"), "task-b stays registered");
        assert!(!cancels.contains_key("task-a"), "task-a entry consumed");
    }

    #[tokio::test]
    async fn cancel_for_unknown_task_is_ignored() {
        let mut cancels = HashMap::new();
        let (tx, mut rx) = oneshot::channel();
        cancels.insert("task-live".to_string(), tx);

        cancel_task(&protocol::cancel_envelope("task-ghost"), &mut cancels);

        assert!(rx.try_recv().is_err(), "no cancel may fire");
        assert!(cancels.contains_key("task-live"));
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
        let mut cancels = HashMap::new();

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        dispatch_task(
            request_envelope("task-1", Some(&root.workspace_root)),
            &root,
            &out_tx,
            &mut cancels,
            &registry,
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
        let mut cancels = HashMap::new();

        let outside = std::env::temp_dir().join("pebble-bridge-outside-escape");
        std::fs::create_dir_all(&outside).expect("create outside dir");
        let outside_str = outside.to_str().expect("utf-8 path").to_string();

        dispatch_task(
            request_envelope("task-escape", Some(&outside_str)),
            &root,
            &out_tx,
            &mut cancels,
            &registry,
        )
        .await;

        let rejection = next_envelope(&mut out_rx).await;
        assert_eq!(rejection.msg_type, "task_result");
        let payload: protocol::TaskResult =
            serde_json::from_value(rejection.payload).expect("task_result payload");
        assert_eq!(payload.status, "error");
        assert!(payload.error.as_deref().unwrap_or("").contains("escapes"));
        assert!(cancels.is_empty(), "rejected task must register no cancel");
        std::fs::remove_dir_all(&outside).ok();
    }
}
