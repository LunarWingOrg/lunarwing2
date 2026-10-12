//! Integration tests for the external-worker (EWE) WebSocket path.
//!
//! These tests stand up a mock worker speaking the `lunarwing-agent-v1`
//! protocol on an ephemeral loopback port
//! and drive the real
//! [`ExternalWorkerManager::execute_task`] against it. They close the largest
//! pre-existing test gap (T1 in SESSION-AUDIT-MT-DARKIRC-EWE-2026-06-23.md):
//! `connect_and_handshake`, `run_external_task`, and `execute_task` were
//! previously never exercised end-to-end.
//!
//! Coverage:
//! - happy path (task_progress + task_result success), with output accumulation
//! - failed result
//! - task timeout (worker hangs) -> `ExternalWorkerTimeout`
//! - cancel an in-flight task -> `ExternalTaskStatus::Cancelled`
//! - heartbeat-only timeout -> no `partial_output` evidence (WI#267: synthetic
//!   keep-alives must not be presented as a recovered investigation)
//! - timeout with genuine streamed evidence -> still preserved, with
//!   heartbeats stripped from a worker-supplied result output (WI#267)
//! - timed-out result with empty/heartbeat-only output -> falls back to the
//!   genuine-progress buffer in `res.output` and the persisted result
//!   message (!1237 finding 2)
//! - whitespace separator frames -> preserved verbatim in recovered timeout
//!   evidence (!1239 finding 1: opencode2 streams raw, untrimmed deltas)
//! - connection failure (unreachable endpoint) -> `ExternalWorkerConnectionFailed`
//! - protocol error (connection closed before `ready`) -> `ExternalWorkerProtocolError`
//! - connection-pool reuse across two sequential successful tasks
//! - subprotocol negotiation: a worker that still only accepts the removed
//!   legacy `ironclaw-agent-v1` name must be rejected at the upgrade
//!
//! No PostgreSQL / Docker / external services required; pure loopback WS.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use lunarwing_common::PromptContextRole;
use secrecy::SecretString;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use uuid::Uuid;

use lunarwing::channels::web::types::SseEvent;
use lunarwing::config::{
    CronWorkerConfig, ExternalWorkerConfig, ExternalWorkerMode, WorkerEndpoint,
};
use lunarwing::context::{ContextManager, JobContext, JobState};
#[cfg(feature = "libsql")]
use lunarwing::db::Database;
#[cfg(feature = "libsql")]
use lunarwing::db::libsql::LibSqlBackend;
use lunarwing::error::OrchestratorError;
use lunarwing::hooks::HookRegistry;
use lunarwing::orchestrator::ExternalWorkerManager;
use lunarwing::orchestrator::external_worker::{
    ConversationMessage, CronCommandOrigin, CronTick, ExternalTaskStatus, SUBPROTOCOL, TaskContext,
    ToolProxy,
};
use lunarwing::tools::Tool;
use lunarwing::tools::builtin::CreateJobTool;
#[cfg(feature = "libsql")]
use lunarwing::tools::builtin::{MemoryReadTool, MemoryWriteTool};
#[cfg(feature = "libsql")]
use lunarwing::workspace::Workspace;

/// How long we wait for any single test assertion before declaring the test
/// hung. The protocol path under test should resolve in well under this.
const TEST_BOUND: Duration = Duration::from_secs(15);

// ── Mock worker ─────────────────────────────────────────────────────

/// Scripted behaviour for a mock worker connection.
#[derive(Clone)]
enum MockBehavior {
    /// Send `ready`, then on each `task_request`: stream the given progress
    /// chunks, then a success result. Loops so the connection can be pooled and
    /// reused for subsequent tasks.
    Success {
        progress: Vec<String>,
        output: String,
    },
    /// Return one successful result, then drop the socket without a close
    /// frame. A pooled client must detect this before dispatching another task.
    SuccessThenDisconnect { output: String },
    /// Capture task-request envelope, then return success.
    Capture {
        requests: tokio::sync::mpsc::UnboundedSender<serde_json::Value>,
    },
    /// Send `ready`, then on `task_request`: a single failed result.
    Failed { error: String },
    /// Send `ready`, read `task_request`, then never reply (drives a timeout).
    Hang,
    /// WI#325: capture each `task_request` envelope, then wedge (never
    /// reply). The supervisor's heartbeat watchdog must cancel and
    /// re-dispatch on a fresh connection; every dispatch is captured.
    CaptureHang {
        requests: tokio::sync::mpsc::UnboundedSender<serde_json::Value>,
    },
    /// WI#325: capture each `task_request` envelope, then fail the task.
    /// Exercises the supervisor's re-entry on the Failed path (not just the
    /// heartbeat-lapse path).
    CaptureFail {
        requests: tokio::sync::mpsc::UnboundedSender<serde_json::Value>,
        error: String,
    },
    /// WI#325: enforce a per-workspace lock like the real kimi/pebble
    /// bridges (`workspacePathsOverlap`): a task that arrives while another
    /// holds the workspace is rejected with the "already being modified"
    /// error the bridges emit; the holder wedges for `hold` before
    /// releasing. Used to prove the cron supervisor's re-entry does not race
    /// its own cancel into the workspace lock, and that a busy rejection
    /// retries without consuming the re-entry budget.
    LockAwareHang {
        requests: tokio::sync::mpsc::UnboundedSender<serde_json::Value>,
        active: std::sync::Arc<AtomicUsize>,
        busy_rejections: std::sync::Arc<AtomicUsize>,
        hold: Duration,
    },
    /// Wait for and capture the daemon's cancel envelope.
    WaitForCancel {
        messages: tokio::sync::mpsc::UnboundedSender<serde_json::Value>,
    },
    /// WI#101-style worker with a live read loop: on `task_request` hold the
    /// task open; when the daemon's cancel envelope arrives, wait `ack_delay_ms`
    /// then ack with a `task_result` echoing the real task_id (WI#149 path).
    /// Every inbound envelope is forwarded to `messages` so tests can observe
    /// what the worker saw and in what order.
    CancelAck {
        messages: tokio::sync::mpsc::UnboundedSender<serde_json::Value>,
        ack_status: String,
        ack_delay_ms: u64,
    },
    /// Pre-WI#101 worker: receives the cancel envelope but never acks and
    /// never closes the socket (read loop parked inside the task). Drives the
    /// daemon's bounded-deadline fallback (WI#149).
    CancelNoAck,
    /// WI#324 proxied tool path: on `task_request`, send the scripted
    /// `tool_call` frames in order, forwarding every `tool_result` reply to
    /// `replies`, then finish with a success `task_result`. Loops so the
    /// connection stays poolable.
    ToolProxy {
        calls: Vec<(String, serde_json::Value)>,
        replies: tokio::sync::mpsc::UnboundedSender<serde_json::Value>,
    },
    /// Like `ToolProxy`, but the caller scripts raw frame payloads as
    /// `(type, payload)` pairs — for frames the typed builder cannot express
    /// (missing/mismatched `task_id`, `tool_list`). The literal string
    /// `@task_id@` inside a payload is replaced with the real task id.
    /// Every `tool_result`/`tool_catalog` reply is forwarded to `replies`.
    ToolProxyRaw {
        frames: Vec<(String, serde_json::Value)>,
        replies: tokio::sync::mpsc::UnboundedSender<serde_json::Value>,
    },
    /// Like `ToolProxyRaw`, but each frame is a verbatim pre-serialized JSON
    /// text — for frames no builder can express: escaped `"type"` spellings,
    /// oversized payloads (MR !1533 DP-1/DP-5). `@task_id@` inside the text
    /// is replaced with the real task id.
    ToolProxyVerbatim {
        frames: Vec<String>,
        replies: tokio::sync::mpsc::UnboundedSender<serde_json::Value>,
    },
    /// Send one text message in individually bounded continuation frames.
    FragmentedText(String),
    /// WI#267: a kimi-executor-shaped worker whose CLI wedged. Streams the
    /// given progress frames (typically synthetic `[worker] ` heartbeats,
    /// optionally interleaved with genuine deltas), then reports `timed_out`
    /// with the given result output. Never pooled (terminal failure).
    HeartbeatTimeout {
        progress: Vec<String>,
        result_output: String,
    },
    /// Complete the WebSocket upgrade but close without sending `ready`
    /// (drives a protocol error).
    CloseBeforeReady,
}

/// Which subprotocol names the mock worker accepts, simulating current and
/// outdated worker generations.
#[derive(Clone, Copy)]
enum SubprotocolPolicy {
    /// Current worker: accepts `lunarwing-agent-v1`.
    NewOnly,
    /// Outdated worker that still only knows the removed legacy name.
    LegacyOnly,
}

impl SubprotocolPolicy {
    /// Accepted names in preference order.
    fn accepted(self) -> &'static [&'static str] {
        match self {
            Self::NewOnly => &[SUBPROTOCOL],
            Self::LegacyOnly => &["ironclaw-agent-v1"],
        }
    }
}

/// Build a protocol envelope as a JSON string. The id/timestamp are opaque to
/// the client; hardcoded values are fine.
fn envelope(msg_type: &str, payload: serde_json::Value) -> String {
    serde_json::json!({
        "id": "mock-msg-id",
        "type": msg_type,
        "timestamp": "2026-01-01T00:00:00Z",
        "payload": payload,
    })
    .to_string()
}

/// Spawn a mock worker with no auth check. See [`spawn_mock_worker_full`].
async fn spawn_mock_worker_async(behavior: MockBehavior) -> (String, Arc<AtomicUsize>) {
    spawn_mock_worker_full(behavior, None, SubprotocolPolicy::NewOnly).await
}

/// Spawn a mock worker that requires a matching Bearer token on the WS upgrade.
async fn spawn_mock_worker_with_auth(
    behavior: MockBehavior,
    expected_token: Option<String>,
) -> (String, Arc<AtomicUsize>) {
    spawn_mock_worker_full(behavior, expected_token, SubprotocolPolicy::NewOnly).await
}

/// Spawn a mock worker with a specific subprotocol acceptance policy.
async fn spawn_mock_worker_with_subprotocol(
    behavior: MockBehavior,
    subprotocols: SubprotocolPolicy,
) -> (String, Arc<AtomicUsize>) {
    spawn_mock_worker_full(behavior, None, subprotocols).await
}

/// Spawn a mock worker bound to an ephemeral loopback port that optionally
/// requires a Bearer token matching `expected_token` on the WS upgrade and
/// negotiates subprotocols per `subprotocols`.
async fn spawn_mock_worker_full(
    behavior: MockBehavior,
    expected_token: Option<String>,
    subprotocols: SubprotocolPolicy,
) -> (String, Arc<AtomicUsize>) {
    spawn_mock_worker_on_host("127.0.0.1", behavior, expected_token, subprotocols).await
}

/// Like [`spawn_mock_worker_full`] but binds a specific host. `127.0.0.2` is
/// loopback-routed by the OS while the manager's strict host check classifies
/// it non-loopback (MR !1549 EF-3), letting tests exercise the cleartext
/// non-loopback strip against a real connection.
async fn spawn_mock_worker_on_host(
    host: &str,
    behavior: MockBehavior,
    expected_token: Option<String>,
    subprotocols: SubprotocolPolicy,
) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind((host, 0))
        .await
        .expect("bind mock listener");
    let addr = listener.local_addr().expect("local_addr");
    let url = format!("ws://{addr}");
    let conn_count = Arc::new(AtomicUsize::new(0));

    tokio::spawn(run_accept_loop(
        listener,
        behavior,
        conn_count.clone(),
        expected_token,
        subprotocols,
    ));

    (url, conn_count)
}

/// Accept loop: each accepted connection is handed to a per-connection handler.
async fn run_accept_loop(
    listener: TcpListener,
    behavior: MockBehavior,
    conn_count: Arc<AtomicUsize>,
    expected_token: Option<String>,
    subprotocols: SubprotocolPolicy,
) {
    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => break,
        };
        conn_count.fetch_add(1, Ordering::SeqCst);
        let b = behavior.clone();
        let tok = expected_token.clone();
        tokio::spawn(async move {
            let _ = handle_connection(stream, b, tok, subprotocols).await;
        });
    }
}

/// 401 response used to reject an unauthenticated WS upgrade.
fn unauthorized_response() -> ErrorResponse {
    tokio_tungstenite::tungstenite::http::Response::builder()
        .status(tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED)
        .body(Some("unauthorized".to_string()))
        .expect("valid error response")
}

/// 400 response used to reject a WS upgrade with no acceptable subprotocol.
fn bad_subprotocol_response() -> ErrorResponse {
    tokio_tungstenite::tungstenite::http::Response::builder()
        .status(tokio_tungstenite::tungstenite::http::StatusCode::BAD_REQUEST)
        .body(Some("no acceptable subprotocol".to_string()))
        .expect("valid error response")
}

/// Per-connection protocol handler. Implements the server side of
/// `lunarwing-agent-v1` for the scripted behavior. When `expected_token` is
/// set, the WS upgrade is rejected unless the client sends a matching
/// `Authorization: Bearer <token>` header.
#[allow(clippy::result_large_err)] // Err type is fixed by tungstenite's Callback trait
async fn handle_connection(
    stream: tokio::net::TcpStream,
    behavior: MockBehavior,
    expected_token: Option<String>,
    subprotocols: SubprotocolPolicy,
) {
    let handshake = move |req: &Request, mut resp: Response| -> Result<Response, ErrorResponse> {
        // Validate Bearer auth when a token is configured.
        if let Some(expected) = expected_token.as_deref() {
            let bearer = req
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .unwrap_or("");
            if bearer != expected {
                return Err(unauthorized_response());
            }
        }
        // Echo the matched OFFERED subprotocol, never a static constant:
        // tungstenite rejects both "no subprotocol" (when one was offered)
        // and an echo that was not in the client's offer list. This mirrors
        // what real worker containers must do to stay compatible with the
        // daemon's offer.
        if let Some(offer) = req.headers().get("sec-websocket-protocol") {
            let offered: Vec<&str> = offer
                .to_str()
                .ok()
                .map(|v| v.split(',').map(str::trim).collect())
                .unwrap_or_default();
            let matched = subprotocols
                .accepted()
                .iter()
                .copied()
                .find(|name| offered.contains(name));
            match matched {
                Some(name) => {
                    resp.headers_mut().insert(
                        "sec-websocket-protocol",
                        name.parse().expect("valid header value"),
                    );
                }
                None => return Err(bad_subprotocol_response()),
            }
        }
        Ok(resp)
    };

    // For CloseBeforeReady we want the upgrade to complete (so the client treats
    // it as a protocol problem, not a connection problem) then close.
    let mut ws = match tokio_tungstenite::accept_hdr_async(stream, handshake).await {
        Ok(ws) => ws,
        Err(_) => return,
    };

    if matches!(behavior, MockBehavior::CloseBeforeReady) {
        let _ = ws.close(None).await;
        return;
    }

    // Send `ready`.
    let ready = envelope(
        "ready",
        serde_json::json!({ "worker_id": "mock-1", "version": "test", "mode": "job" }),
    );
    if ws.send(Message::Text(ready.into())).await.is_err() {
        return;
    }

    loop {
        // Wait for a `task_request` from the client.
        let req = match ws.next().await {
            Some(Ok(m)) if !m.is_close() => m,
            _ => break, // client closed / errored
        };
        let Ok(req_text) = req.to_text() else {
            break;
        };
        let Ok(req_envelope) = serde_json::from_str::<serde_json::Value>(req_text) else {
            break;
        };
        if req_envelope.get("type").and_then(|value| value.as_str()) == Some("ping") {
            let pong = envelope("pong", serde_json::json!({}));
            if ws.send(Message::Text(pong.into())).await.is_err() {
                return;
            }
            continue;
        }

        match behavior {
            MockBehavior::FragmentedText(ref text) => {
                use tokio_tungstenite::tungstenite::protocol::frame::{
                    Frame,
                    coding::{Data, OpCode},
                };
                let chunks = text.as_bytes().chunks(512 * 1024);
                let count = chunks.len();
                for (index, chunk) in chunks.enumerate() {
                    let opcode = if index == 0 {
                        Data::Text
                    } else {
                        Data::Continue
                    };
                    let frame =
                        Frame::message(chunk.to_vec(), OpCode::Data(opcode), index + 1 == count);
                    if ws.send(Message::Frame(frame)).await.is_err() {
                        return;
                    }
                }
                return;
            }
            MockBehavior::Success {
                ref progress,
                ref output,
            } => {
                for delta in progress {
                    let p = envelope(
                        "task_progress",
                        serde_json::json!({ "task_id": "t", "delta": delta, "done": false }),
                    );
                    if ws.send(Message::Text(p.into())).await.is_err() {
                        return;
                    }
                }
                let r = envelope(
                    "task_result",
                    serde_json::json!({
                        "task_id": "t",
                        "status": "success",
                        "output": output,
                        "error": null,
                        "duration_ms": 5,
                    }),
                );
                if ws.send(Message::Text(r.into())).await.is_err() {
                    return;
                }
                // Loop: the connection may be returned to the pool and reused.
            }
            MockBehavior::Failed { ref error } => {
                let r = envelope(
                    "task_result",
                    serde_json::json!({
                        "task_id": "t",
                        "status": "failed",
                        "output": "",
                        "error": error,
                        "duration_ms": 3,
                    }),
                );
                let _ = ws.send(Message::Text(r.into())).await;
                return; // failed tasks are not pooled
            }
            MockBehavior::SuccessThenDisconnect { ref output } => {
                let r = envelope(
                    "task_result",
                    serde_json::json!({
                        "task_id": "t",
                        "status": "success",
                        "output": output,
                        "error": null,
                        "duration_ms": 5,
                    }),
                );
                let _ = ws.send(Message::Text(r.into())).await;
                return;
            }
            MockBehavior::Capture { ref requests } => {
                let _ = requests.send(req_envelope);
                let result = envelope(
                    "task_result",
                    serde_json::json!({
                        "task_id": "t",
                        "status": "success",
                        "output": "captured",
                        "error": null,
                        "duration_ms": 1,
                    }),
                );
                if ws.send(Message::Text(result.into())).await.is_err() {
                    return;
                }
            }
            MockBehavior::Hang => {
                // Never reply; the client will time out or be cancelled. Hold
                // the connection open (bounded) so the read side stays pending.
                tokio::time::sleep(Duration::from_secs(30)).await;
                return;
            }
            MockBehavior::CaptureHang { ref requests } => {
                // Capture, then wedge: the daemon's watchdog must notice the
                // silence and (for cron missions) re-dispatch on a fresh
                // connection, which the accept loop hands to another
                // CaptureHang instance sharing the same capture channel.
                let _ = requests.send(req_envelope);
                tokio::time::sleep(Duration::from_secs(30)).await;
                return;
            }
            MockBehavior::CaptureFail {
                ref requests,
                ref error,
            } => {
                let _ = requests.send(req_envelope.clone());
                let task_id = req_envelope["payload"]["task_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let result = envelope(
                    "task_result",
                    serde_json::json!({
                        "task_id": task_id,
                        "status": "failed",
                        "output": "",
                        "error": error,
                        "duration_ms": 1,
                    }),
                );
                let _ = ws.send(Message::Text(result.into())).await;
                return;
            }
            MockBehavior::LockAwareHang {
                ref requests,
                ref active,
                ref busy_rejections,
                hold,
            } => {
                let task_id = req_envelope["payload"]["task_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if active.fetch_add(1, Ordering::SeqCst) > 0 {
                    active.fetch_sub(1, Ordering::SeqCst);
                    busy_rejections.fetch_add(1, Ordering::SeqCst);
                    let busy = envelope(
                        "task_result",
                        serde_json::json!({
                            "task_id": task_id,
                            "status": "error",
                            "output": "",
                            "error": format!(
                                "Workspace /workspace is already being modified by task another-task. \
                                 Cancel or finish that task before dispatching another task to this workspace."
                            ),
                            "duration_ms": 0,
                        }),
                    );
                    let _ = ws.send(Message::Text(busy.into())).await;
                    return;
                }
                let _ = requests.send(req_envelope);
                // The real worker's lock outlives the connection: a cancelled
                // task's subprocess dies on its own schedule (SIGTERM→SIGKILL
                // grace), so the workspace stays busy after the socket is
                // gone. Model that with a detached release timer that holds
                // the lock for `hold` AND until at least one busy rejection
                // has occurred (bounded), so the test is timing-safe.
                let active_release = active.clone();
                let rejections_release = busy_rejections.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(hold).await;
                    for _ in 0..300 {
                        if rejections_release.load(Ordering::SeqCst) > 0 {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    active_release.fetch_sub(1, Ordering::SeqCst);
                });
                // Wedge: read (rejecting concurrent requests busy) until the
                // daemon tears the connection down.
                loop {
                    match ws.next().await {
                        Some(Ok(m)) if !m.is_close() => {
                            let Ok(text) = m.to_text() else {
                                continue;
                            };
                            let Ok(env) = serde_json::from_str::<serde_json::Value>(text) else {
                                continue;
                            };
                            if env.get("type").and_then(|v| v.as_str()) == Some("ping") {
                                let pong = envelope("pong", serde_json::json!({}));
                                if ws.send(Message::Text(pong.into())).await.is_err() {
                                    break;
                                }
                                continue;
                            }
                            if env.get("type").and_then(|v| v.as_str()) == Some("cancel") {
                                continue; // wedge: cancel never resolves the task
                            }
                            let other_id = env["payload"]["task_id"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string();
                            busy_rejections.fetch_add(1, Ordering::SeqCst);
                            let busy = envelope(
                                "task_result",
                                serde_json::json!({
                                    "task_id": other_id,
                                    "status": "error",
                                    "output": "",
                                    "error": format!(
                                        "Workspace /workspace is already being modified by task {task_id}. \
                                         Cancel or finish that task before dispatching another task to this workspace."
                                    ),
                                    "duration_ms": 0,
                                }),
                            );
                            if ws.send(Message::Text(busy.into())).await.is_err() {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
                return;
            }
            MockBehavior::WaitForCancel { ref messages } => {
                while let Some(Ok(message)) = ws.next().await {
                    let Ok(text) = message.to_text() else {
                        continue;
                    };
                    let Ok(envelope) = serde_json::from_str::<serde_json::Value>(text) else {
                        continue;
                    };
                    let _ = messages.send(envelope.clone());
                    if envelope.get("type").and_then(|value| value.as_str()) == Some("cancel") {
                        return;
                    }
                }
                return;
            }
            MockBehavior::CancelAck {
                ref messages,
                ref ack_status,
                ack_delay_ms,
            } => {
                // WI#101 shape: the read loop stays live while the task is
                // parked, so the cancel envelope arrives mid-task and the
                // worker acks with a real task_result. The task_request was
                // already consumed by the outer loop and is handed in here.
                let _ = messages.send(req_envelope.clone());
                let task_id = req_envelope["payload"]["task_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                while let Some(Ok(message)) = ws.next().await {
                    let Ok(text) = message.to_text() else {
                        continue;
                    };
                    let Ok(msg_env) = serde_json::from_str::<serde_json::Value>(text) else {
                        continue;
                    };
                    if msg_env.get("type").and_then(|value| value.as_str()) != Some("cancel") {
                        continue;
                    }
                    let _ = messages.send(msg_env);
                    tokio::time::sleep(Duration::from_millis(ack_delay_ms)).await;
                    let ack = envelope(
                        "task_result",
                        serde_json::json!({
                            "task_id": task_id,
                            "status": ack_status,
                            "output": "",
                            "error": null,
                            "duration_ms": 0,
                        }),
                    );
                    let _ = ws.send(Message::Text(ack.into())).await;
                    return;
                }
            }
            MockBehavior::CancelNoAck => {
                // Pre-WI#101 shape: the cancel envelope queues unread (read
                // loop parked inside the task); never ack, never close.
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
            MockBehavior::ToolProxy {
                ref calls,
                ref replies,
            } => {
                // WI#324 shape: mid-task the worker issues proxied tool calls
                // over the control channel and blocks on each reply.
                let task_id = req_envelope["payload"]["task_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let mut failed = false;
                for (index, (name, args)) in calls.iter().enumerate() {
                    let call = envelope(
                        "tool_call",
                        serde_json::json!({
                            "task_id": task_id,
                            "call_id": format!("call-{index}"),
                            "name": name,
                            "arguments": args,
                        }),
                    );
                    if ws.send(Message::Text(call.into())).await.is_err() {
                        return;
                    }
                    // Read until the daemon's tool_result for this call arrives.
                    loop {
                        match ws.next().await {
                            Some(Ok(m)) if !m.is_close() => {
                                let Ok(text) = m.to_text() else { continue };
                                let Ok(env) = serde_json::from_str::<serde_json::Value>(text)
                                else {
                                    continue;
                                };
                                if env.get("type").and_then(|v| v.as_str()) == Some("tool_result") {
                                    let _ = replies.send(env);
                                    break;
                                }
                            }
                            _ => {
                                failed = true;
                                break;
                            }
                        }
                    }
                    if failed {
                        return;
                    }
                }
                let r = envelope(
                    "task_result",
                    serde_json::json!({
                        "task_id": task_id,
                        "status": "success",
                        "output": "tool calls done",
                        "error": null,
                        "duration_ms": 1,
                    }),
                );
                if ws.send(Message::Text(r.into())).await.is_err() {
                    return;
                }
                // Loop: success leaves the connection poolable.
            }
            MockBehavior::ToolProxyRaw {
                ref frames,
                ref replies,
            } => {
                // Raw scripted frames (WI#324 negative paths): each frame is
                // sent verbatim after @task_id@ substitution; the worker
                // blocks on each reply like the typed ToolProxy behavior.
                let task_id = req_envelope["payload"]["task_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let mut failed = false;
                for (msg_type, payload) in frames {
                    let substituted = payload.to_string().replace("@task_id@", &task_id);
                    let Ok(payload_json) = serde_json::from_str::<serde_json::Value>(&substituted)
                    else {
                        return;
                    };
                    let frame = envelope(msg_type, payload_json);
                    if ws.send(Message::Text(frame.into())).await.is_err() {
                        return;
                    }
                    loop {
                        match ws.next().await {
                            Some(Ok(m)) if !m.is_close() => {
                                let Ok(text) = m.to_text() else { continue };
                                let Ok(env) = serde_json::from_str::<serde_json::Value>(text)
                                else {
                                    continue;
                                };
                                let reply_type = env.get("type").and_then(|v| v.as_str());
                                if matches!(reply_type, Some("tool_result") | Some("tool_catalog"))
                                {
                                    let _ = replies.send(env);
                                    break;
                                }
                            }
                            _ => {
                                failed = true;
                                break;
                            }
                        }
                    }
                    if failed {
                        return;
                    }
                }
                let r = envelope(
                    "task_result",
                    serde_json::json!({
                        "task_id": task_id,
                        "status": "success",
                        "output": "raw frames done",
                        "error": null,
                        "duration_ms": 1,
                    }),
                );
                if ws.send(Message::Text(r.into())).await.is_err() {
                    return;
                }
                // Loop: success leaves the connection poolable.
            }
            MockBehavior::ToolProxyVerbatim {
                ref frames,
                ref replies,
            } => {
                // Verbatim pre-serialized frames (MR !1533 DP-1/DP-5): each
                // text is sent exactly as given after @task_id@ substitution,
                // so escaped `"type"` spellings and oversized payloads reach
                // the daemon byte-for-byte. The worker blocks on each reply
                // like the typed ToolProxy behavior.
                let task_id = req_envelope["payload"]["task_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let mut failed = false;
                for frame_text in frames {
                    let substituted = frame_text.replace("@task_id@", &task_id);
                    if ws.send(Message::Text(substituted.into())).await.is_err() {
                        return;
                    }
                    loop {
                        match ws.next().await {
                            Some(Ok(m)) if !m.is_close() => {
                                let Ok(text) = m.to_text() else { continue };
                                let Ok(env) = serde_json::from_str::<serde_json::Value>(text)
                                else {
                                    continue;
                                };
                                let reply_type = env.get("type").and_then(|v| v.as_str());
                                if matches!(reply_type, Some("tool_result") | Some("tool_catalog"))
                                {
                                    let _ = replies.send(env);
                                    break;
                                }
                            }
                            _ => {
                                failed = true;
                                break;
                            }
                        }
                    }
                    if failed {
                        return;
                    }
                }
                let r = envelope(
                    "task_result",
                    serde_json::json!({
                        "task_id": task_id,
                        "status": "success",
                        "output": "verbatim frames done",
                        "error": null,
                        "duration_ms": 1,
                    }),
                );
                if ws.send(Message::Text(r.into())).await.is_err() {
                    return;
                }
                // Loop: success leaves the connection poolable.
            }
            MockBehavior::HeartbeatTimeout {
                ref progress,
                ref result_output,
            } => {
                // WI#267 shape: the executor keeps its socket alive with
                // synthetic heartbeats while the wedged CLI streams nothing,
                // then its silence watchdog resolves `timed_out`. The result
                // output mirrors the executor's accumulator (genuine CLI
                // stdout only) — the test decides whether to fold heartbeats
                // in to exercise the daemon-side stripping.
                for delta in progress {
                    let p = envelope(
                        "task_progress",
                        serde_json::json!({ "task_id": "t", "delta": delta, "done": false }),
                    );
                    if ws.send(Message::Text(p.into())).await.is_err() {
                        return;
                    }
                }
                let r = envelope(
                    "task_result",
                    serde_json::json!({
                        "task_id": "t",
                        "status": "timed_out",
                        "output": result_output,
                        "error": "kimi produced no stream output for 900000ms",
                        "duration_ms": 900_000,
                    }),
                );
                let _ = ws.send(Message::Text(r.into())).await;
                return; // timed-out tasks are not pooled
            }
            MockBehavior::CloseBeforeReady => unreachable!("handled above"),
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

fn config_for(url: &str) -> ExternalWorkerConfig {
    ExternalWorkerConfig {
        name: "mock".to_string(),
        url: url.to_string(),
        timeout_ms: 30_000,
        endpoints: vec![WorkerEndpoint {
            url: url.to_string(),
            auth_token: None,
            weight: None,
        }],
        // `..Default::default()` so sibling-MR fields (e.g. tool_allowlist)
        // never break this literal outside Git's conflict markers (DE-4).
        ..Default::default()
    }
}

/// Like [`config_for`] but sets a Bearer auth token on both the worker config
/// and its endpoint, exercising the `SecretString` -> `expose_secret()` path.
fn config_for_with_auth(url: &str, token: &str) -> ExternalWorkerConfig {
    ExternalWorkerConfig {
        name: "mock".to_string(),
        url: url.to_string(),
        auth_token: Some(SecretString::from(token)),
        timeout_ms: 30_000,
        endpoints: vec![WorkerEndpoint {
            url: url.to_string(),
            auth_token: Some(SecretString::from(token)),
            weight: None,
        }],
        ..Default::default()
    }
}

/// Bind and immediately drop a listener to obtain a guaranteed-closed port.
async fn closed_port_url() -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = l.local_addr().expect("addr");
    drop(l);
    format!("ws://{addr}")
}

// ── Tests ────────────────────────────────────────────────────────────

#[tokio::test]
async fn cancel_ack_is_awaited_before_socket_teardown() {
    // WI#149 regression: after sending a cancel envelope mid-task, the daemon
    // must wait (bounded) for the worker's terminal frame before returning.
    // The worker delays its ack by 400ms; the pre-WI#149 fire-and-forget
    // daemon would return immediately, so the caller would receive the
    // daemon-side fallback result ("Cancelled by user" error) instead of the
    // worker's own ack (error null).
    let (message_tx, mut message_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::CancelAck {
        messages: message_tx,
        ack_status: "cancelled".to_string(),
        ack_delay_ms: 400,
    })
    .await;
    let mgr = Arc::new(ExternalWorkerManager::new(vec![config_for(&url)]));
    let job_id = Uuid::new_v4();

    let mgr_c = mgr.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        mgr_c.cancel_task(job_id).await;
    });

    let start = std::time::Instant::now();
    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            job_id,
            "mock",
            "task",
            Some(10_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected result");
    let elapsed = start.elapsed();

    assert_eq!(res.status, ExternalTaskStatus::Cancelled);
    // The fallback result carries error "Cancelled by user"; the worker ack
    // carries error null. Distinguishing them proves the terminal frame was
    // consumed rather than raced past.
    assert!(
        res.error.is_none(),
        "expected the worker's ack result, got the daemon fallback: {res:?}"
    );
    assert!(
        elapsed >= Duration::from_millis(400),
        "daemon returned before the worker's ack: {elapsed:?}"
    );

    // Ordering sanity: the worker saw the task_request, then the cancel.
    let first = message_rx.recv().await.expect("task_request captured");
    assert_eq!(first["type"], "task_request");
    let second = message_rx.recv().await.expect("cancel captured");
    assert_eq!(second["type"], "cancel");
    assert_eq!(second["payload"]["task_id"], job_id.to_string());
}

#[tokio::test]
async fn cancel_ack_timeout_falls_back_to_daemon_result() {
    // WI#149 fallback: a pre-WI#101 worker that never acks must not hang the
    // cancel path — the bounded deadline (3s) expires and the daemon-side
    // Cancelled result is returned.
    let (url, _count) = spawn_mock_worker_async(MockBehavior::CancelNoAck).await;
    let mgr = Arc::new(ExternalWorkerManager::new(vec![config_for(&url)]));
    let job_id = Uuid::new_v4();

    let mgr_c = mgr.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        mgr_c.cancel_task(job_id).await;
    });

    let start = std::time::Instant::now();
    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            job_id,
            "mock",
            "task",
            Some(10_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected result");
    let elapsed = start.elapsed();

    assert_eq!(res.status, ExternalTaskStatus::Cancelled);
    assert_eq!(res.error.as_deref(), Some("Cancelled by user"));
    // The wait happened (>= 2.5s of the 3s deadline) and is bounded (well
    // under the 15s test bound; the no-ack worker parks for 30s).
    assert!(
        elapsed >= Duration::from_millis(2_500),
        "ack deadline wait did not happen: {elapsed:?}"
    );
    assert!(
        elapsed < TEST_BOUND,
        "ack deadline wait is not bounded: {elapsed:?}"
    );
}

#[tokio::test]
async fn timeout_path_awaits_cancel_ack_before_return() {
    // WI#149: the timeout branch must also give the worker the bounded ack
    // window after sending its cancel envelope. The task deadline (300ms)
    // fires first, then the worker acks 300ms after receiving the cancel; the
    // daemon still returns ExternalWorkerTimeout, but only after consuming the
    // worker's ack — never immediately at the deadline.
    let (message_tx, mut message_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::CancelAck {
        messages: message_tx,
        ack_status: "cancelled".to_string(),
        ack_delay_ms: 300,
    })
    .await;
    let mgr = ExternalWorkerManager::new(vec![config_for(&url)]);
    let job_id = Uuid::new_v4();

    let start = std::time::Instant::now();
    let err = timeout(
        TEST_BOUND,
        mgr.execute_task(
            job_id,
            "mock",
            "task",
            Some(300),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect_err("expected ExternalWorkerTimeout");
    let elapsed = start.elapsed();

    assert!(
        matches!(err, OrchestratorError::ExternalWorkerTimeout { .. }),
        "got {err:?}"
    );
    // Cancel sent at the 300ms deadline, worker acks 300ms later; returning
    // only at ~600ms proves the daemon waited for the ack instead of tearing
    // down at the deadline.
    assert!(
        elapsed >= Duration::from_millis(500),
        "timeout path returned before the worker's ack: {elapsed:?}"
    );

    // The worker saw the task_request first, then the cancel.
    let first = message_rx.recv().await.expect("task_request captured");
    assert_eq!(first["type"], "task_request");
    let second = message_rx.recv().await.expect("cancel captured");
    assert_eq!(second["type"], "cancel");
    assert_eq!(second["payload"]["task_id"], job_id.to_string());
}

#[tokio::test]
async fn drain_pool_awaits_inflight_cancel_handoff() {
    // WI#149 shutdown handoff: drain_pool must signal cancel on every in-flight
    // task and wait for the runner's handoff before closing idle pool
    // connections. The worker delays its ack 500ms after the cancel; by the
    // time drain_pool returns, the cancel must have been received and the ack
    // consumed. The pre-WI#149 drain never signalled in-flight tasks at all,
    // so no cancel would ever reach the worker.
    let (message_tx, mut message_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::CancelAck {
        messages: message_tx,
        ack_status: "cancelled".to_string(),
        ack_delay_ms: 500,
    })
    .await;
    let mgr = Arc::new(ExternalWorkerManager::new(vec![config_for(&url)]));
    let job_id = Uuid::new_v4();

    // Dispatch a hanging task fire-and-forget (in-flight).
    let dispatched = mgr
        .execute_task(
            job_id,
            "mock",
            "task",
            Some(30_000),
            false,
            TaskContext::default(),
        )
        .await
        .expect("dispatch async task");
    assert!(dispatched.is_none(), "fire-and-forget returns no result");

    // Let the runner connect, dispatch, and park on the task.
    tokio::time::sleep(Duration::from_millis(300)).await;

    timeout(TEST_BOUND, mgr.drain_pool())
        .await
        .expect("drain timed out");

    // The worker must have received the task_request AND the cancel (forwarded
    // before its 500ms-delayed ack, which itself preceded the runner's return,
    // which drain waited for). Under the old drain the cancel was never sent.
    let first = message_rx
        .try_recv()
        .expect("task_request must have reached the worker before drain returned");
    assert_eq!(first["type"], "task_request");
    let second = message_rx
        .try_recv()
        .expect("cancel must have reached the worker before drain returned");
    assert_eq!(second["type"], "cancel");
    assert_eq!(second["payload"]["task_id"], job_id.to_string());
}

#[tokio::test]
async fn cancel_racing_task_deadline_still_resolves_cancelled() {
    // WI#149 edge: a user cancel arriving in the last seconds of the task
    // window must still resolve as Cancelled. The ack wait runs inside the
    // outer task-timeout future, so without capping the wait to the remaining
    // task budget the outer deadline would drop the future mid-handoff and
    // misreport TimedOut. The worker never acks (worst case: the full
    // remaining budget is consumed by the fallback).
    let (url, _count) = spawn_mock_worker_async(MockBehavior::CancelNoAck).await;
    let mgr = Arc::new(ExternalWorkerManager::new(vec![config_for(&url)]));
    let job_id = Uuid::new_v4();

    let mgr_c = mgr.clone();
    tokio::spawn(async move {
        // Task window is 2s; cancel at 1.5s leaves 500ms of budget — under
        // the 3s ack deadline, so the fallback must engage, bounded by the
        // remaining task budget rather than the full ack deadline.
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        mgr_c.cancel_task(job_id).await;
    });

    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            job_id,
            "mock",
            "task",
            Some(2_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected result");

    assert_eq!(
        res.status,
        ExternalTaskStatus::Cancelled,
        "cancel must win over the expiring task deadline"
    );
    assert_eq!(res.error.as_deref(), Some("Cancelled by user"));
}

#[tokio::test]
async fn happy_path_success_with_progress() {
    let (url, _count) = spawn_mock_worker_async(MockBehavior::Success {
        progress: vec!["Hello ".to_string(), "world".to_string()],
        output: "Hello world".to_string(),
    })
    .await;
    let mgr = ExternalWorkerManager::new(vec![config_for(&url)]);

    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            Uuid::new_v4(),
            "mock",
            "do the thing",
            Some(5_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected a result for wait=true");

    assert_eq!(res.status, ExternalTaskStatus::Success);
    // `task_result.output` is non-empty, so it takes precedence over the
    // accumulated progress deltas.
    assert_eq!(res.output, "Hello world");
    assert!(res.error.is_none());
}

#[tokio::test]
async fn task_request_serializes_typed_conversation_context() {
    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::Capture {
        requests: request_tx,
    })
    .await;
    let mgr = ExternalWorkerManager::new(vec![config_for(&url)]);
    let context = TaskContext {
        conversation_history: vec![
            ConversationMessage {
                role: PromptContextRole::User,
                content: "question".to_string(),
            },
            ConversationMessage {
                role: PromptContextRole::Assistant,
                content: "answer".to_string(),
            },
        ],
        ..Default::default()
    };

    timeout(
        TEST_BOUND,
        mgr.execute_task(
            Uuid::new_v4(),
            "mock",
            "authoritative task",
            Some(5_000),
            true,
            context,
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored");

    let request = request_rx.recv().await.expect("captured request");
    assert_eq!(request["payload"]["prompt"], "authoritative task");
    assert_eq!(
        request["payload"]["context"]["conversation_history"],
        serde_json::json!([
            {"role": "user", "content": "question"},
            {"role": "assistant", "content": "answer"}
        ])
    );
}

#[tokio::test]
async fn create_job_context_scope_is_opt_in_and_metadata_isolated() {
    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::Capture {
        requests: request_tx,
    })
    .await;
    let worker_manager = Arc::new(ExternalWorkerManager::new(vec![config_for(&url)]));
    let tool = CreateJobTool::new(Arc::new(ContextManager::new(5)))
        .with_external_workers(worker_manager, None);
    let mut context = JobContext::with_user("user-1", "chat", "test");
    context.metadata = serde_json::json!({
        "conv_secret": "must-not-cross",
        "notify_user": "routing-id"
    });
    context.prompt_context = Arc::from([
        ConversationMessage {
            role: PromptContextRole::User,
            content: "prior question".to_string(),
        },
        ConversationMessage {
            role: PromptContextRole::Assistant,
            content: "prior answer".to_string(),
        },
    ]);

    timeout(
        TEST_BOUND,
        tool.execute(
            serde_json::json!({
                "title": "Context job",
                "description": "Do work",
                "mode": "mock",
                "context_scope": "recent",
                "wait": true
            }),
            &context,
        ),
    )
    .await
    .expect("test timed out")
    .expect("create_job failed");
    let with_context = request_rx
        .recv()
        .await
        .expect("captured contextual request");
    assert_eq!(
        with_context["payload"]["context"]["conversation_history"],
        serde_json::json!([
            {"role": "user", "content": "prior question"},
            {"role": "assistant", "content": "prior answer"}
        ])
    );
    assert_eq!(
        with_context["payload"]["context"]["metadata"],
        serde_json::json!({})
    );
    assert!(!with_context.to_string().contains("must-not-cross"));
    assert!(!with_context.to_string().contains("routing-id"));

    timeout(
        TEST_BOUND,
        tool.execute(
            serde_json::json!({
                "title": "No context job",
                "description": "Do other work",
                "mode": "mock",
                "wait": true
            }),
            &context,
        ),
    )
    .await
    .expect("test timed out")
    .expect("create_job failed");
    let without_context = request_rx.recv().await.expect("captured plain request");
    assert_eq!(
        without_context["payload"]["context"]["conversation_history"],
        serde_json::json!([])
    );
}

#[tokio::test]
async fn happy_path_uses_accumulated_progress_when_output_empty() {
    // When the result's `output` is empty, the client falls back to the
    // concatenated progress deltas.
    let (url, _count) = spawn_mock_worker_async(MockBehavior::Success {
        progress: vec!["alpha ".to_string(), "beta".to_string()],
        output: String::new(),
    })
    .await;
    let mgr = ExternalWorkerManager::new(vec![config_for(&url)]);

    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            Uuid::new_v4(),
            "mock",
            "task",
            Some(5_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected result");

    assert_eq!(res.status, ExternalTaskStatus::Success);
    assert_eq!(res.output, "alpha beta");
}

/// WI#267 / !1235 finding 2 regression: heartbeat stripping is scoped to
/// timeout evidence. A *successful* worker result must come back verbatim —
/// no whitespace trimming and no deletion of lines that merely start with the
/// `[worker] ` prefix — and that exact value is what the job record persists
/// as `Completed: …`.
#[tokio::test]
async fn successful_result_output_is_returned_verbatim() {
    // Leading/trailing whitespace plus a genuine `[worker] `-prefixed log
    // line: both must survive a success where the old fix reworded the
    // buffer to "result\n[worker] initialized".
    let output = "  result\n[worker] initialized session\n";
    let (url, _count) = spawn_mock_worker_async(MockBehavior::Success {
        progress: vec![
            "[worker] Still waiting for Kimi Code response; task remains active.".to_string(),
        ],
        output: output.to_string(),
    })
    .await;
    let context_manager = Arc::new(ContextManager::new(5));
    let (event_tx, _event_rx) = tokio::sync::broadcast::channel(8);
    let mgr = ExternalWorkerManager::new(vec![config_for(&url)])
        .with_event_deps(event_tx, Arc::clone(&context_manager));
    let job_id = Uuid::new_v4();
    context_manager
        .register_sandbox_job(job_id, "test-user", "task", "task")
        .await
        .expect("register job");

    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            job_id,
            "mock",
            "task",
            Some(5_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected result");

    assert_eq!(res.status, ExternalTaskStatus::Success);
    assert_eq!(res.output, output);
}

#[tokio::test]
async fn failed_result_status() {
    let (url, _count) = spawn_mock_worker_async(MockBehavior::Failed {
        error: "boom".to_string(),
    })
    .await;
    let mgr = ExternalWorkerManager::new(vec![config_for(&url)]);

    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            Uuid::new_v4(),
            "mock",
            "task",
            Some(5_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected result");

    assert_eq!(res.status, ExternalTaskStatus::Failed);
    assert_eq!(res.error.as_deref(), Some("boom"));
}

#[tokio::test]
async fn task_timeout_when_worker_hangs() {
    let (message_tx, mut message_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::WaitForCancel {
        messages: message_tx,
    })
    .await;
    let mgr = ExternalWorkerManager::new(vec![config_for(&url)]);
    let job_id = Uuid::new_v4();

    // Short task timeout (covers only the response-read phase, which starts
    // after connect+handshake+send, all local and fast).
    let err = timeout(
        TEST_BOUND,
        mgr.execute_task(
            job_id,
            "mock",
            "task",
            Some(300),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect_err("expected ExternalWorkerTimeout");

    assert!(
        matches!(err, OrchestratorError::ExternalWorkerTimeout { .. }),
        "got {err:?}"
    );
    let cancel = timeout(TEST_BOUND, message_rx.recv())
        .await
        .expect("cancel envelope timed out")
        .expect("cancel envelope channel closed");
    assert_eq!(cancel["type"], "cancel");
    assert_eq!(cancel["payload"]["task_id"], job_id.to_string());
}

#[tokio::test]
async fn async_timeout_finalizes_job_and_emits_result() {
    let (url, _count) = spawn_mock_worker_async(MockBehavior::Hang).await;
    let context_manager = Arc::new(ContextManager::new(5));
    let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(8);
    let mgr = ExternalWorkerManager::new(vec![config_for(&url)])
        .with_event_deps(event_tx, Arc::clone(&context_manager));
    let job_id = Uuid::new_v4();
    context_manager
        .register_sandbox_job(job_id, "test-user", "task", "task")
        .await
        .expect("register job");

    let result = mgr
        .execute_task(
            job_id,
            "mock",
            "task",
            Some(100),
            false,
            TaskContext::default(),
        )
        .await
        .expect("dispatch async task");
    assert!(result.is_none());

    let status = timeout(TEST_BOUND, async {
        loop {
            let (event_job_id, _, event) = event_rx.recv().await.expect("job event");
            if event_job_id == job_id
                && let SseEvent::JobResult { status, .. } = event
            {
                break status;
            }
        }
    })
    .await
    .expect("result event timed out");

    assert_eq!(status, "timed_out");
    assert_eq!(
        context_manager
            .get_context(job_id)
            .await
            .expect("job context")
            .state,
        JobState::Failed
    );
}

/// WI#267: a wedged CLI behind a healthy executor streams only synthetic
/// `[worker] Still waiting…` heartbeats until the silence watchdog fires.
/// Nothing genuine was produced, so the timeout must NOT be preserved as
/// `partial_output` evidence — the exact inverse of the WI#263 short-evidence
/// case, which must keep working.
#[tokio::test]
async fn heartbeat_only_timeout_preserves_no_evidence() {
    let heartbeat = "[worker] Still waiting for Kimi Code response; task remains active.";
    let (url, _count) = spawn_mock_worker_async(MockBehavior::HeartbeatTimeout {
        progress: vec![heartbeat.to_string(); 4],
        result_output: String::new(),
    })
    .await;
    let context_manager = Arc::new(ContextManager::new(5));
    let (event_tx, _event_rx) = tokio::sync::broadcast::channel(8);
    let mgr = ExternalWorkerManager::new(vec![config_for(&url)])
        .with_event_deps(event_tx, Arc::clone(&context_manager));
    let job_id = Uuid::new_v4();
    context_manager
        .register_sandbox_job(job_id, "test-user", "task", "task")
        .await
        .expect("register job");

    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            job_id,
            "mock",
            "task",
            Some(5_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected a result for wait=true");

    assert_eq!(res.status, ExternalTaskStatus::TimedOut);
    assert!(
        res.output.is_empty(),
        "heartbeat-only timeout must not substitute heartbeats as output"
    );

    let ctx = context_manager
        .get_context(job_id)
        .await
        .expect("job context");
    assert!(
        ctx.metadata.get("partial_output").is_none(),
        "heartbeat-only timeout must not preserve keep-alives as evidence"
    );
    assert_eq!(ctx.state, JobState::Failed);
}

/// WI#267 companion: genuine streamed evidence around a timeout is still
/// preserved verbatim — the heartbeat exclusion must not swallow real work.
/// Also covers the daemon-side stripping of a worker `task_result.output`
/// that (unlike the current kimi executor) folded its keep-alives in.
#[tokio::test]
async fn timeout_with_genuine_evidence_still_preserves_it() {
    let heartbeat = "[worker] Still waiting for Kimi Code response; task remains active.";
    let genuine = "Investigation step: evidence gathered.";
    let (url, _count) = spawn_mock_worker_async(MockBehavior::HeartbeatTimeout {
        progress: vec![
            heartbeat.to_string(),
            genuine.to_string(),
            heartbeat.to_string(),
        ],
        // Old-executor shape: heartbeats concatenated into the result buffer.
        result_output: format!("{heartbeat}\n{genuine}\n{heartbeat}"),
    })
    .await;
    let context_manager = Arc::new(ContextManager::new(5));
    let (event_tx, _event_rx) = tokio::sync::broadcast::channel(8);
    let mgr = ExternalWorkerManager::new(vec![config_for(&url)])
        .with_event_deps(event_tx, Arc::clone(&context_manager));
    let job_id = Uuid::new_v4();
    context_manager
        .register_sandbox_job(job_id, "test-user", "task", "task")
        .await
        .expect("register job");

    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            job_id,
            "mock",
            "task",
            Some(5_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected a result for wait=true");

    assert_eq!(res.status, ExternalTaskStatus::TimedOut);
    // The worker's result output is heartbeat-stripped, not taken verbatim.
    assert_eq!(res.output, genuine);

    let ctx = context_manager
        .get_context(job_id)
        .await
        .expect("job context");
    let partial = ctx
        .metadata
        .get("partial_output")
        .and_then(|v| v.as_str())
        .expect("genuine evidence must still be preserved");
    assert_eq!(partial, genuine);
    assert!(!partial.contains("[worker]"));
}

/// !1239 finding 1 regression: whitespace separators must survive the
/// evidence gate. The opencode2 executor forwards raw, untrimmed
/// `session.text.delta` strings, so separators arrive as their own `" "` /
/// `"\n"` progress frames between genuine deltas. The heartbeat exclusion
/// used to drop those frames and concatenate the surrounding words, turning
/// `"alpha beta\ngamma"` into `"alphabetagamma"` in the recovered evidence.
#[tokio::test]
async fn timed_out_evidence_preserves_whitespace_separator_frames() {
    let (url, _count) = spawn_mock_worker_async(MockBehavior::HeartbeatTimeout {
        // opencode2 stream shape: separators split into their own frames.
        progress: vec![
            "alpha".to_string(),
            " ".to_string(),
            "beta".to_string(),
            "\n".to_string(),
            "gamma".to_string(),
        ],
        // Empty result output: the evidence comes from the daemon's own
        // streamed buffer, which is exactly what the gate corrupted.
        result_output: String::new(),
    })
    .await;
    let context_manager = Arc::new(ContextManager::new(5));
    let (event_tx, _event_rx) = tokio::sync::broadcast::channel(8);
    let mgr = ExternalWorkerManager::new(vec![config_for(&url)])
        .with_event_deps(event_tx, Arc::clone(&context_manager));
    let job_id = Uuid::new_v4();
    context_manager
        .register_sandbox_job(job_id, "test-user", "task", "task")
        .await
        .expect("register job");

    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            job_id,
            "mock",
            "task",
            Some(5_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected a result for wait=true");

    assert_eq!(res.status, ExternalTaskStatus::TimedOut);
    assert_eq!(
        res.output, "alpha beta\ngamma",
        "whitespace separator frames must not be dropped from recovered evidence"
    );

    let ctx = context_manager
        .get_context(job_id)
        .await
        .expect("job context");
    let partial = ctx
        .metadata
        .get("partial_output")
        .and_then(|v| v.as_str())
        .expect("separator-preserving evidence must be persisted");
    assert_eq!(partial, "alpha beta\ngamma");
}

/// !1237 finding 2 regression: a worker that streams genuine progress and
/// then reports `timed_out` with an empty (or heartbeat-only) result output
/// must keep the genuine progress in BOTH the caller-visible `res.output`
/// and the persisted `timed_out_result_message` — the fallback to the
/// daemon's own buffer happens before the result is constructed, not only in
/// the later metadata snapshot.
#[tokio::test]
async fn timed_out_result_with_empty_output_keeps_genuine_progress() {
    let heartbeat = "[worker] Still waiting for Kimi Code response; task remains active.";
    let genuine = "Investigation step: evidence gathered.";
    let (url, _count) = spawn_mock_worker_async(MockBehavior::HeartbeatTimeout {
        progress: vec![genuine.to_string(), heartbeat.to_string()],
        // Empty result output: the worker's watchdog fired with nothing in
        // its accumulator — the genuine work exists only in the daemon's
        // streamed buffer.
        result_output: String::new(),
    })
    .await;
    let context_manager = Arc::new(ContextManager::new(5));
    let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(8);
    let mgr = ExternalWorkerManager::new(vec![config_for(&url)])
        .with_event_deps(event_tx, Arc::clone(&context_manager));
    let job_id = Uuid::new_v4();
    context_manager
        .register_sandbox_job(job_id, "test-user", "task", "task")
        .await
        .expect("register job");

    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            job_id,
            "mock",
            "task",
            Some(5_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected a result for wait=true");

    assert_eq!(res.status, ExternalTaskStatus::TimedOut);
    // The immediate wait=true caller sees the real streamed work — not an
    // empty buffer that only the later metadata fallback would recover.
    // `final_msg` (the persisted `timed_out_result_message`) is derived from
    // this same `task_result.output`, so it carries the progress too.
    assert_eq!(res.output, genuine);

    // The result event fires (job resolves as timed_out end-to-end).
    timeout(TEST_BOUND, async {
        loop {
            let (event_job_id, _, event) = event_rx.recv().await.expect("job event");
            if event_job_id == job_id
                && let SseEvent::JobResult { .. } = event
            {
                break;
            }
        }
    })
    .await
    .expect("result event timed out");

    let ctx = context_manager
        .get_context(job_id)
        .await
        .expect("job context");
    let partial = ctx
        .metadata
        .get("partial_output")
        .and_then(|v| v.as_str())
        .expect("genuine progress must be preserved as partial_output");
    assert_eq!(partial, genuine);
}

/// !1237 finding 1 regression: a genuine `[worker] `-prefixed diagnostic —
/// the opencode executors' "Transient provider failure; retrying …" retry
/// notice — is real evidence of what the worker was doing and must remain in
/// the recoverable buffer when the task times out, while the synthetic
/// "Still waiting …" keep-alive does not.
#[tokio::test]
async fn timed_out_evidence_keeps_transient_failure_diagnostics() {
    let heartbeat = "[worker] Still waiting for OpenCode/model response; task remains active.";
    let transient = "[worker] Transient provider failure; retrying same OpenCode session (1/3).";
    let (url, _count) = spawn_mock_worker_async(MockBehavior::HeartbeatTimeout {
        progress: vec![transient.to_string(), heartbeat.to_string()],
        result_output: String::new(),
    })
    .await;
    let context_manager = Arc::new(ContextManager::new(5));
    let (event_tx, _event_rx) = tokio::sync::broadcast::channel(8);
    let mgr = ExternalWorkerManager::new(vec![config_for(&url)])
        .with_event_deps(event_tx, Arc::clone(&context_manager));
    let job_id = Uuid::new_v4();
    context_manager
        .register_sandbox_job(job_id, "test-user", "task", "task")
        .await
        .expect("register job");

    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            job_id,
            "mock",
            "task",
            Some(5_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected a result for wait=true");

    assert_eq!(res.status, ExternalTaskStatus::TimedOut);
    assert_eq!(
        res.output, transient,
        "the transient-failure diagnostic must remain recoverable evidence"
    );

    let ctx = context_manager
        .get_context(job_id)
        .await
        .expect("job context");
    let partial = ctx
        .metadata
        .get("partial_output")
        .and_then(|v| v.as_str())
        .expect("transient-failure diagnostics must be preserved");
    assert_eq!(partial, transient);
    assert!(!partial.contains("Still waiting"));
}

#[tokio::test]
async fn cancel_in_flight_task() {
    let (message_tx, mut message_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::WaitForCancel {
        messages: message_tx,
    })
    .await;
    let mgr = Arc::new(ExternalWorkerManager::new(vec![config_for(&url)]));
    let job_id = Uuid::new_v4();

    // Cancel shortly after dispatch from a sibling task.
    let mgr_c = mgr.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        mgr_c.cancel_task(job_id).await;
    });

    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            job_id,
            "mock",
            "task",
            Some(10_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected result");

    assert_eq!(res.status, ExternalTaskStatus::Cancelled);
    let cancel = timeout(TEST_BOUND, message_rx.recv())
        .await
        .expect("cancel envelope timed out")
        .expect("cancel envelope channel closed");
    assert_eq!(cancel["type"], "cancel");
    assert_eq!(cancel["payload"]["task_id"], job_id.to_string());
}

#[tokio::test]
async fn cancellation_before_dispatch_is_preserved() {
    let (message_tx, mut message_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, count) = spawn_mock_worker_async(MockBehavior::WaitForCancel {
        messages: message_tx,
    })
    .await;
    let mgr = ExternalWorkerManager::new(vec![config_for(&url)]);
    let job_id = Uuid::new_v4();

    mgr.reserve_task(job_id, "mock")
        .await
        .expect("reserve task");
    assert!(mgr.cancel_task(job_id).await);

    let result = timeout(
        TEST_BOUND,
        mgr.execute_task(
            job_id,
            "mock",
            "task",
            Some(10_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected result");

    assert_eq!(result.status, ExternalTaskStatus::Cancelled);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert!(message_rx.try_recv().is_err());
}

#[tokio::test]
async fn connection_failure_unreachable_endpoint() {
    let url = closed_port_url().await;
    let mgr = ExternalWorkerManager::new(vec![config_for(&url)]);

    let err = timeout(
        TEST_BOUND,
        mgr.execute_task(
            Uuid::new_v4(),
            "mock",
            "task",
            Some(5_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect_err("expected connection failure");

    assert!(
        matches!(
            err,
            OrchestratorError::ExternalWorkerConnectionFailed { .. }
        ),
        "got {err:?}"
    );
}

#[tokio::test]
async fn protocol_error_closed_before_ready() {
    let (url, _count) = spawn_mock_worker_async(MockBehavior::CloseBeforeReady).await;
    let mgr = ExternalWorkerManager::new(vec![config_for(&url)]);

    let err = timeout(
        TEST_BOUND,
        mgr.execute_task(
            Uuid::new_v4(),
            "mock",
            "task",
            Some(5_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect_err("expected protocol error");

    assert!(
        matches!(err, OrchestratorError::ExternalWorkerProtocolError { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn connection_pool_reuse_across_sequential_tasks() {
    // A successful task returns its connection to the pool; a second task to
    // the same endpoint should reuse it (no new TCP accept).
    let (url, conn_count) = spawn_mock_worker_async(MockBehavior::Success {
        progress: vec![],
        output: "ok".to_string(),
    })
    .await;
    let mgr = ExternalWorkerManager::new(vec![config_for(&url)]);

    let first = timeout(
        TEST_BOUND,
        mgr.execute_task(
            Uuid::new_v4(),
            "mock",
            "first",
            Some(5_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("first timed out")
    .expect("first errored")
    .expect("first result");
    assert_eq!(first.status, ExternalTaskStatus::Success);

    let second = timeout(
        TEST_BOUND,
        mgr.execute_task(
            Uuid::new_v4(),
            "mock",
            "second",
            Some(5_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("second timed out")
    .expect("second errored")
    .expect("second result");
    assert_eq!(second.status, ExternalTaskStatus::Success);

    // Exactly one TCP connection accepted: the second task reused the pooled one.
    assert_eq!(
        conn_count.load(Ordering::SeqCst),
        1,
        "expected pool reuse (1 connection), got {}",
        conn_count.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn stale_pooled_connection_reconnects_before_next_task() {
    let (url, conn_count) = spawn_mock_worker_async(MockBehavior::SuccessThenDisconnect {
        output: "ok".to_string(),
    })
    .await;
    let mgr = ExternalWorkerManager::new(vec![config_for(&url)]);

    for task in ["first", "second"] {
        let result = timeout(
            TEST_BOUND,
            mgr.execute_task(
                Uuid::new_v4(),
                "mock",
                task,
                Some(5_000),
                true,
                TaskContext::default(),
            ),
        )
        .await
        .expect("task timed out")
        .expect("task errored")
        .expect("expected result");

        assert_eq!(result.status, ExternalTaskStatus::Success);
        assert_eq!(result.output, "ok");
    }

    assert_eq!(
        conn_count.load(Ordering::SeqCst),
        2,
        "second task should reconnect after pooled socket closes"
    );
}

#[tokio::test]
async fn legacy_only_worker_is_rejected() {
    // Regression test for the removal of the `ironclaw-agent-v1` alias: the
    // daemon offers only `lunarwing-agent-v1`, so a worker that still only
    // accepts the legacy name must fail the upgrade (HTTP 400, no echo) and
    // surface as `ExternalWorkerConnectionFailed`.
    let (url, _count) = spawn_mock_worker_with_subprotocol(
        MockBehavior::Success {
            progress: vec![],
            output: "ok".to_string(),
        },
        SubprotocolPolicy::LegacyOnly,
    )
    .await;
    let mgr = ExternalWorkerManager::new(vec![config_for(&url)]);

    let err = timeout(
        TEST_BOUND,
        mgr.execute_task(
            Uuid::new_v4(),
            "mock",
            "task",
            Some(5_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect_err("expected connection failure against legacy-only worker");

    let OrchestratorError::ExternalWorkerConnectionFailed { ref reason, .. } = err else {
        panic!("got {err:?}");
    };
    assert!(
        reason.contains("lunarwing-agent-v1") && reason.contains("ironclaw-agent-v1"),
        "operator hint for the removed legacy subprotocol should surface in the failure reason: {reason}"
    );
}

#[tokio::test]
async fn new_only_worker_negotiates_primary_name() {
    // A worker that only accepts `lunarwing-agent-v1` rejects the upgrade
    // unless the daemon actually offers the primary name — this fails if
    // the client offer ever regresses to a different name.
    let (url, _count) = spawn_mock_worker_with_subprotocol(
        MockBehavior::Success {
            progress: vec![],
            output: "ok".to_string(),
        },
        SubprotocolPolicy::NewOnly,
    )
    .await;
    let mgr = ExternalWorkerManager::new(vec![config_for(&url)]);

    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            Uuid::new_v4(),
            "mock",
            "task",
            Some(5_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored against new-only worker")
    .expect("expected result");

    assert_eq!(res.status, ExternalTaskStatus::Success);
}

#[tokio::test]
async fn auth_correct_token_succeeds() {
    // End-to-end proof that the SecretString auth_token flows correctly:
    // config -> SecretString -> expose_secret() -> Bearer header -> worker
    // validates and accepts the upgrade -> task completes.
    let (url, _count) = spawn_mock_worker_with_auth(
        MockBehavior::Success {
            progress: vec![],
            output: "ok".to_string(),
        },
        Some("good-token".to_string()),
    )
    .await;
    let mgr = ExternalWorkerManager::new(vec![config_for_with_auth(&url, "good-token")]);

    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            Uuid::new_v4(),
            "mock",
            "task",
            Some(5_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected result");

    assert_eq!(res.status, ExternalTaskStatus::Success);
}

#[tokio::test]
async fn auth_wrong_token_rejected() {
    // A wrong token must be rejected at the WS upgrade: the worker demands
    // "good-token" but the config carries "bad-token" -> handshake rejected ->
    // ExternalWorkerConnectionFailed. This also rules out the mock accepting
    // any token (i.e. it really validates the secret).
    let (url, _count) = spawn_mock_worker_with_auth(
        MockBehavior::Success {
            progress: vec![],
            output: "ok".to_string(),
        },
        Some("good-token".to_string()),
    )
    .await;
    let mgr = ExternalWorkerManager::new(vec![config_for_with_auth(&url, "bad-token")]);

    let err = timeout(
        TEST_BOUND,
        mgr.execute_task(
            Uuid::new_v4(),
            "mock",
            "task",
            Some(5_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect_err("expected connection failure (rejected auth)");

    let OrchestratorError::ExternalWorkerConnectionFailed { ref reason, .. } = err else {
        panic!("got {err:?}");
    };
    assert!(
        !reason.contains("ironclaw-agent-v1"),
        "auth rejection must not carry the worker-upgrade hint (see !893 follow-up): {reason}"
    );
}

// ── WI#324: proxied tool path ────────────────────────────────────────

/// Test tool standing in for an in-process tenant tool. Records the
/// JobContext it was invoked with so the test can assert tenant scoping
/// (user_id + job_id come from the connection's job, never from the frame).
/// The name is configurable (MR !1533 DP-2): the proxy only dispatches the
/// WI#324 phase-1 capability set, so probes meant to execute register under
/// a phase-1 name (e.g. "knowledge_search") — the test registry is empty
/// apart from the probe, so the name resolves to the probe itself.
struct ProbeEchoTool {
    tool_name: String,
    seen: std::sync::Mutex<Vec<(String, Uuid)>>,
}

impl ProbeEchoTool {
    /// A probe registered under the phase-1 name `knowledge_search`.
    fn phase1() -> Arc<Self> {
        Arc::new(Self {
            tool_name: "knowledge_search".to_string(),
            seen: std::sync::Mutex::new(Vec::new()),
        })
    }
}

#[async_trait::async_trait]
impl Tool for ProbeEchoTool {
    fn name(&self) -> &str {
        &self.tool_name
    }
    fn description(&self) -> &str {
        "Echoes params back; records the job context"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<lunarwing::tools::ToolOutput, lunarwing::tools::ToolError> {
        self.seen
            .lock()
            .expect("probe lock")
            .push((ctx.user_id.clone(), ctx.job_id));
        Ok(lunarwing::tools::ToolOutput::success(
            serde_json::json!({ "echo": params, "user_id": ctx.user_id }),
            Duration::default(),
        ))
    }
    fn requires_sanitization(&self) -> bool {
        false
    }
}

fn test_safety() -> Arc<lunarwing::safety::SafetyLayer> {
    Arc::new(lunarwing::safety::SafetyLayer::new(
        &lunarwing::config::SafetyConfig {
            max_output_length: 100_000,
            injection_check_enabled: false,
        },
    ))
}

fn test_proxy(tools: Arc<lunarwing::tools::ToolRegistry>) -> ToolProxy {
    ToolProxy::new(tools, test_safety(), Arc::new(HookRegistry::new()))
}

#[tokio::test]
async fn tool_call_roundtrip_allowlist_and_deny_by_default() {
    // WI#324 acceptance: an allowlisted tool round-trips through the tenant's
    // in-process registry; a non-allowlisted tool is a hard "denied" reject,
    // never a silent empty result.
    let probe = ProbeEchoTool::phase1();
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools.register(probe.clone()).await;

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxy {
        calls: vec![
            (
                "knowledge_search".to_string(),
                serde_json::json!({ "k": "v" }),
            ),
            // Registered nowhere AND not allowlisted -> denied.
            (
                "memory_write".to_string(),
                serde_json::json!({ "path": "x" }),
            ),
        ],
        replies: reply_tx,
    })
    .await;

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["knowledge_search".to_string()];
    let mgr = ExternalWorkerManager::new(vec![config]).with_tool_proxy(test_proxy(tools));
    let job_id = Uuid::new_v4();

    let context = TaskContext {
        user_id: "tenant-user".to_string(),
        ..Default::default()
    };
    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(job_id, "mock", "task", Some(10_000), true, context),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected result");
    assert_eq!(res.status, ExternalTaskStatus::Success);

    let first = reply_rx.recv().await.expect("first tool_result");
    assert_eq!(first["payload"]["call_id"], "call-0");
    assert_eq!(first["payload"]["status"], "success");
    let output = first["payload"]["output"].as_str().expect("output string");
    assert!(
        output.contains("\"k\": \"v\"") || output.contains("\"k\":\"v\""),
        "echo output should carry the params, got: {output}"
    );
    assert!(
        output.contains("tenant-user"),
        "tool must run in the job's user scope, got: {output}"
    );

    let second = reply_rx.recv().await.expect("second tool_result");
    assert_eq!(second["payload"]["call_id"], "call-1");
    assert_eq!(second["payload"]["status"], "denied");
    assert!(
        second["payload"]["error"]
            .as_str()
            .expect("denial reason")
            .contains("tool_allowlist"),
        "denial must name the allowlist: {second}"
    );

    // The allowlisted call ran exactly once, scoped to this job + user.
    let seen = probe.seen.lock().expect("probe lock");
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0], ("tenant-user".to_string(), job_id));
}

/// Build a tick-shaped task context (WI#325) for proxy-authority tests.
fn cron_tick_context() -> TaskContext {
    let mut context = TaskContext {
        user_id: "tenant-user".to_string(),
        ..Default::default()
    };
    context.cron_tick = Some(CronTick {
        event: "cron.tick".to_string(),
        mission_id: "test-mission".to_string(),
        schedule: "0 15 10 * * *".to_string(),
        goal: "Watch the herd".to_string(),
        tick_index: 1,
        chain_index: 0,
        fired_at: "2026-10-12T10:15:00Z".to_string(),
    });
    context
}

#[tokio::test]
async fn cron_tick_tool_calls_use_cron_allowlist_not_worker_allowlist() {
    // MR !1506 CQ-10: a cron tick is standing, unattended tool authority —
    // tick jobs draw from cron.tool_allowlist, never the worker-level list.
    // Phase 1: worker-level grants the tool, cron list empty -> denied.
    let probe = ProbeEchoTool::phase1();
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools.register(probe.clone()).await;

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxy {
        calls: vec![(
            "knowledge_search".to_string(),
            serde_json::json!({ "k": "v" }),
        )],
        replies: reply_tx,
    })
    .await;

    let mut config = cron_config_with_schedule(&url, "0 15 10 * * *", 300, 2);
    config.tool_allowlist = vec!["knowledge_search".to_string()];
    let mgr = ExternalWorkerManager::new(vec![config]).with_tool_proxy(test_proxy(tools.clone()));
    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            Uuid::new_v4(),
            "mock",
            "task",
            Some(10_000),
            true,
            cron_tick_context(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected result");
    assert_eq!(res.status, ExternalTaskStatus::Success);

    let reply = reply_rx.recv().await.expect("tool_result");
    assert_eq!(
        reply["payload"]["status"], "denied",
        "a tick must not inherit the worker-level allowlist: {reply}"
    );
    assert!(
        probe.seen.lock().expect("probe lock").is_empty(),
        "a denied tick call must never reach the tool"
    );

    // Phase 2: the same grant under cron.tool_allowlist round-trips.
    let (reply_tx2, mut reply_rx2) = tokio::sync::mpsc::unbounded_channel();
    let (url2, _count2) = spawn_mock_worker_async(MockBehavior::ToolProxy {
        calls: vec![(
            "knowledge_search".to_string(),
            serde_json::json!({ "k": "v" }),
        )],
        replies: reply_tx2,
    })
    .await;
    let mut cron_config = cron_config_with_schedule(&url2, "0 15 10 * * *", 300, 2);
    cron_config
        .cron
        .as_mut()
        .expect("cron block")
        .tool_allowlist = vec!["knowledge_search".to_string()];
    let mgr2 =
        ExternalWorkerManager::new(vec![cron_config]).with_tool_proxy(test_proxy(tools.clone()));
    let job_id2 = Uuid::new_v4();
    let res2 = timeout(
        TEST_BOUND,
        mgr2.execute_task(
            job_id2,
            "mock",
            "task",
            Some(10_000),
            true,
            cron_tick_context(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected result");
    assert_eq!(res2.status, ExternalTaskStatus::Success);

    let reply2 = reply_rx2.recv().await.expect("tool_result");
    assert_eq!(reply2["payload"]["status"], "success");
    let seen = probe.seen.lock().expect("probe lock");
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0], ("tenant-user".to_string(), job_id2));
}

#[tokio::test]
async fn cron_worker_worker_level_allowlist_is_stripped_at_load() {
    // MR !1549 EF-1: a worker container is one authority domain shared by
    // every concurrent and successive job in it — any same-uid process can
    // connect to a concurrent task's relay socket and borrow that job's
    // grant. A cron worker's worker-level list would therefore be standing
    // authority for every tick, so the manager strips it at load: even an
    // ordinary (non-tick) dispatch to a cron-mode worker is denied.
    let probe = ProbeEchoTool::phase1();
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools.register(probe.clone()).await;

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxy {
        calls: vec![(
            "knowledge_search".to_string(),
            serde_json::json!({ "k": "v" }),
        )],
        replies: reply_tx,
    })
    .await;

    let mut config = cron_config_with_schedule(&url, "0 15 10 * * *", 300, 2);
    config.tool_allowlist = vec!["knowledge_search".to_string()];
    let mgr = ExternalWorkerManager::new(vec![config]).with_tool_proxy(test_proxy(tools));
    let job_id = Uuid::new_v4();
    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            job_id,
            "mock",
            "task",
            Some(10_000),
            true,
            TaskContext {
                user_id: "tenant-user".to_string(),
                ..Default::default()
            },
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected result");
    assert_eq!(res.status, ExternalTaskStatus::Success);

    let reply = reply_rx.recv().await.expect("tool_result");
    assert_eq!(
        reply["payload"]["status"], "denied",
        "a cron worker's stripped worker-level list must deny ordinary dispatches too: {reply}"
    );
    assert!(
        probe.seen.lock().expect("probe lock").is_empty(),
        "a denied call must never reach the tool"
    );
}

#[tokio::test]
async fn cron_allowlist_is_sanitized_at_manager_load() {
    // MR !1549 EF-3: pins that `ExternalWorkerManager::new` actually applies
    // `sanitize_tool_allowlist` to `cron.tool_allowlist` — deleting that call
    // must fail this test, because the cleartext non-loopback strip is
    // load-only (the dispatch gate never sees the URL).
    //
    // Phase 1: a cron worker reached over cleartext ws:// on a
    // non-loopback-classified host loses its whole cron list at load, so a
    // tick's knowledge_search call is denied even though it is listed.
    // (127.0.0.2 is loopback-routed by the OS but classified non-loopback by
    // the strict host check, which only special-cases 127.0.0.1/localhost.)
    let probe = ProbeEchoTool::phase1();
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools.register(probe.clone()).await;

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_on_host(
        "127.0.0.2",
        MockBehavior::ToolProxy {
            calls: vec![(
                "knowledge_search".to_string(),
                serde_json::json!({ "k": "v" }),
            )],
            replies: reply_tx,
        },
        None,
        SubprotocolPolicy::NewOnly,
    )
    .await;
    let mut cleartext_config = cron_config_with_schedule(&url, "0 15 10 * * *", 300, 2);
    cleartext_config
        .cron
        .as_mut()
        .expect("cron block")
        .tool_allowlist = vec!["knowledge_search".to_string()];
    let mgr = ExternalWorkerManager::new(vec![cleartext_config])
        .with_tool_proxy(test_proxy(tools.clone()));
    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            Uuid::new_v4(),
            "mock",
            "task",
            Some(10_000),
            true,
            cron_tick_context(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected result");
    assert_eq!(res.status, ExternalTaskStatus::Success);
    let reply = reply_rx.recv().await.expect("tool_result");
    assert_eq!(
        reply["payload"]["status"], "denied",
        "cleartext non-loopback must strip the cron list at load: {reply}"
    );
    assert!(probe.seen.lock().expect("probe lock").is_empty());

    // Phase 2 (loopback): "*" is not honored and "shell" is dropped, while a
    // real phase-1 grant still round-trips — all through the manager path.
    let (reply_tx2, mut reply_rx2) = tokio::sync::mpsc::unbounded_channel();
    let (url2, _count2) = spawn_mock_worker_async(MockBehavior::ToolProxy {
        calls: vec![
            ("shell".to_string(), serde_json::json!({ "c": "id" })),
            ("memory_read".to_string(), serde_json::json!({ "p": "x" })),
            (
                "knowledge_search".to_string(),
                serde_json::json!({ "k": "v" }),
            ),
        ],
        replies: reply_tx2,
    })
    .await;
    let mut config2 = cron_config_with_schedule(&url2, "0 15 10 * * *", 300, 2);
    config2
        .cron
        .as_mut()
        .expect("cron block")
        .tool_allowlist = vec![
        "*".to_string(),
        "shell".to_string(),
        "knowledge_search".to_string(),
    ];
    let mgr2 =
        ExternalWorkerManager::new(vec![config2]).with_tool_proxy(test_proxy(tools.clone()));
    let res2 = timeout(
        TEST_BOUND,
        mgr2.execute_task(
            Uuid::new_v4(),
            "mock",
            "task",
            Some(10_000),
            true,
            cron_tick_context(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected result");
    assert_eq!(res2.status, ExternalTaskStatus::Success);
    let shell_reply = reply_rx2.recv().await.expect("shell tool_result");
    assert_eq!(
        shell_reply["payload"]["status"], "denied",
        "the proxy-denylisted shell entry must be dropped at load: {shell_reply}"
    );
    let wildcard_reply = reply_rx2.recv().await.expect("memory_read tool_result");
    assert_eq!(
        wildcard_reply["payload"]["status"], "denied",
        "the dropped \"*\" must not grant memory_read: {wildcard_reply}"
    );
    let ok_reply = reply_rx2.recv().await.expect("knowledge_search tool_result");
    assert_eq!(ok_reply["payload"]["status"], "success");
    let seen = probe.seen.lock().expect("probe lock");
    assert_eq!(seen.len(), 1, "only the genuine grant may reach the tool");
}

#[tokio::test]
async fn tool_call_denied_when_allowlist_empty() {
    // Deny-by-default: no tool_allowlist configured at all -> every tool_call
    // is rejected, even one naming a registered tool.
    let probe = Arc::new(ProbeEchoTool {
        tool_name: "probe_echo".to_string(),
        seen: std::sync::Mutex::new(Vec::new()),
    });
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools.register(probe.clone()).await;

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxy {
        calls: vec![("probe_echo".to_string(), serde_json::json!({}))],
        replies: reply_tx,
    })
    .await;

    let mgr = ExternalWorkerManager::new(vec![config_for(&url)]).with_tool_proxy(test_proxy(tools));

    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            Uuid::new_v4(),
            "mock",
            "task",
            Some(10_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected result");
    assert_eq!(res.status, ExternalTaskStatus::Success);

    let reply = reply_rx.recv().await.expect("tool_result");
    assert_eq!(reply["payload"]["status"], "denied");
    assert!(
        probe.seen.lock().expect("probe lock").is_empty(),
        "a denied call must never reach the tool"
    );
}

#[tokio::test]
async fn tool_call_unknown_allowlisted_tool_is_an_error_not_a_hang() {
    // Allowlisted but not registered: the registry miss must come back as a
    // tool_result error frame, not a dropped reply (the worker blocks on it).
    // The name must be inside the phase-1 set (else the load-time sanitizer
    // drops it and the call never reaches resolution), so use a phase-1 tool
    // this test registry deliberately does not register.
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxy {
        calls: vec![("knowledge_search".to_string(), serde_json::json!({}))],
        replies: reply_tx,
    })
    .await;

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["knowledge_search".to_string()];
    let mgr = ExternalWorkerManager::new(vec![config]).with_tool_proxy(test_proxy(tools));

    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            Uuid::new_v4(),
            "mock",
            "task",
            Some(10_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected result");
    assert_eq!(res.status, ExternalTaskStatus::Success);

    let reply = reply_rx.recv().await.expect("tool_result");
    assert_eq!(reply["payload"]["status"], "error");
}

#[tokio::test]
async fn tool_call_without_wired_proxy_replies_error() {
    // A daemon built without with_tool_proxy must still answer tool_call
    // frames with an explicit error (never silently drop them), and the
    // allowlist gate runs first: a non-allowlisted name is "denied" even with
    // no proxy wired.
    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxy {
        calls: vec![
            ("memory_read".to_string(), serde_json::json!({})),
            ("memory_write".to_string(), serde_json::json!({})),
        ],
        replies: reply_tx,
    })
    .await;

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["memory_read".to_string()];
    let mgr = ExternalWorkerManager::new(vec![config]); // no with_tool_proxy

    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            Uuid::new_v4(),
            "mock",
            "task",
            Some(10_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected result");
    assert_eq!(res.status, ExternalTaskStatus::Success);

    let first = reply_rx.recv().await.expect("first tool_result");
    assert_eq!(first["payload"]["status"], "error");
    assert!(
        first["payload"]["error"]
            .as_str()
            .expect("error text")
            .contains("not configured"),
        "expected a proxy-not-configured error: {first}"
    );
    let second = reply_rx.recv().await.expect("second tool_result");
    assert_eq!(second["payload"]["status"], "denied");
}

// ── WI#324 / MR !1506 review: gate, audit, and sanitization coverage ──

/// Configurable probe tool: records invocations and lets the test decide the
/// name, the emitted output, and the autonomous-execution opt-out.
struct GateProbeTool {
    tool_name: String,
    output: String,
    autonomous: bool,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl Tool for GateProbeTool {
    fn name(&self) -> &str {
        &self.tool_name
    }
    fn description(&self) -> &str {
        "gate probe"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<lunarwing::tools::ToolOutput, lunarwing::tools::ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(lunarwing::tools::ToolOutput::success(
            serde_json::json!({ "result": self.output, "echo": params }),
            Duration::default(),
        ))
    }
    fn allows_autonomous_execution(&self) -> bool {
        self.autonomous
    }
}

/// A migrated libSQL store in a tempdir, for store-backed event/permission
/// assertions. libSQL-only: the proxy gate tests persist through the shared
/// `Database` trait, and this is the in-process backend available to tests.
#[cfg(feature = "libsql")]
async fn test_db() -> (Arc<dyn Database>, tempfile::TempDir) {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let backend = LibSqlBackend::new_local(&temp_dir.path().join("proxy_gate_test.db"))
        .await
        .expect("LibSqlBackend");
    backend.run_migrations().await.expect("migrations");
    (Arc::new(backend), temp_dir)
}

/// Drive one task against a mock worker and collect every scripted reply.
async fn run_proxy_task(
    mgr: &ExternalWorkerManager,
    job_id: Uuid,
    reply_rx: &mut tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>,
    expected_replies: usize,
) -> Vec<serde_json::Value> {
    let res = timeout(
        TEST_BOUND,
        mgr.execute_task(
            job_id,
            "mock",
            "task",
            Some(10_000),
            true,
            TaskContext {
                user_id: "tenant-user".to_string(),
                ..Default::default()
            },
        ),
    )
    .await
    .expect("test timed out")
    .expect("execute_task errored")
    .expect("expected result");
    assert_eq!(res.status, ExternalTaskStatus::Success);
    let mut replies = Vec::with_capacity(expected_replies);
    for _ in 0..expected_replies {
        replies.push(reply_rx.recv().await.expect("scripted reply"));
    }
    replies
}

#[tokio::test]
async fn tool_call_missing_or_mismatched_task_id_is_rejected() {
    // MR !1506 review (CQ-7): `task_id` is required and must equal the
    // connection's job id — a leftover worker process on a pooled socket must
    // not run calls in the next job's scope.
    let probe = ProbeEchoTool::phase1();
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools.register(probe.clone()).await;

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxyRaw {
        frames: vec![
            // Missing task_id entirely.
            (
                "tool_call".to_string(),
                serde_json::json!({"call_id": "c-missing", "name": "knowledge_search"}),
            ),
            // Mismatched task_id.
            (
                "tool_call".to_string(),
                serde_json::json!({
                    "call_id": "c-wrong",
                    "name": "knowledge_search",
                    "task_id": "00000000-0000-0000-0000-000000000000",
                }),
            ),
            // Correct task_id (template substituted by the mock).
            (
                "tool_call".to_string(),
                serde_json::json!({
                    "call_id": "c-right",
                    "name": "knowledge_search",
                    "task_id": "@task_id@",
                }),
            ),
        ],
        replies: reply_tx,
    })
    .await;

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["knowledge_search".to_string()];
    let mgr = ExternalWorkerManager::new(vec![config]).with_tool_proxy(test_proxy(tools));

    let replies = run_proxy_task(&mgr, Uuid::new_v4(), &mut reply_rx, 3).await;
    assert_eq!(replies[0]["payload"]["status"], "error");
    assert!(
        replies[0]["payload"]["error"]
            .as_str()
            .expect("error text")
            .contains("task_id"),
        "missing task_id must be explained: {}",
        replies[0]
    );
    assert_eq!(replies[1]["payload"]["status"], "error");
    assert_eq!(replies[2]["payload"]["status"], "success");
    assert_eq!(
        probe.seen.lock().expect("probe lock").len(),
        1,
        "only the correctly bound call may execute"
    );
}

#[tokio::test]
async fn tool_catalog_lists_only_callable_allowlisted_tools() {
    // MR !1506 review (CQ-6): the catalog is built from the allowlist through
    // user-scoped resolution and the autonomous gates — unknown names and
    // autonomous opt-outs are not advertised. Names must be inside the
    // phase-1 set (MR !1533 DP-2) to survive the load-time sanitizer.
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools
        .register(Arc::new(GateProbeTool {
            tool_name: "knowledge_search".to_string(),
            output: "ok".to_string(),
            autonomous: true,
            calls: AtomicUsize::new(0),
        }))
        .await;
    tools
        .register(Arc::new(GateProbeTool {
            tool_name: "knowledge_get_topic".to_string(),
            output: "ok".to_string(),
            autonomous: false,
            calls: AtomicUsize::new(0),
        }))
        .await;

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxyRaw {
        frames: vec![("tool_list".to_string(), serde_json::json!({}))],
        replies: reply_tx,
    })
    .await;

    let mut config = config_for(&url);
    config.tool_allowlist = vec![
        "knowledge_search".to_string(),
        "knowledge_get_topic".to_string(),
        // Phase-1 name this registry never registered: a resolution ghost.
        "gitlab_tool".to_string(),
    ];
    let mgr = ExternalWorkerManager::new(vec![config]).with_tool_proxy(test_proxy(tools));

    let replies = run_proxy_task(&mgr, Uuid::new_v4(), &mut reply_rx, 1).await;
    let names: Vec<&str> = replies[0]["payload"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(
        names,
        vec!["knowledge_search"],
        "catalog must contain only allowlisted, registered, autonomous-capable tools"
    );
}

#[tokio::test]
async fn tool_catalog_advertises_the_proxied_memory_write_schema() {
    // MR !1549 EF-4: memory_write's own schema accepts `layer` and defaults
    // `target` to daily_log — both always denied by the DX-2 guard. The
    // catalog entry must advertise the proxied schema instead: no `layer`,
    // `target` required.
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools
        .register(Arc::new(GateProbeTool {
            tool_name: "memory_write".to_string(),
            output: "ok".to_string(),
            autonomous: true,
            calls: AtomicUsize::new(0),
        }))
        .await;

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxyRaw {
        frames: vec![("tool_list".to_string(), serde_json::json!({}))],
        replies: reply_tx,
    })
    .await;

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["memory_write".to_string()];
    let mgr = ExternalWorkerManager::new(vec![config]).with_tool_proxy(test_proxy(tools));

    let replies = run_proxy_task(&mgr, Uuid::new_v4(), &mut reply_rx, 1).await;
    let entries = replies[0]["payload"]["tools"].as_array().expect("tools array");
    assert_eq!(entries.len(), 1);
    let params = &entries[0]["parameters"];
    let props = params["properties"].as_object().expect("properties");
    assert!(
        !props.contains_key("layer") && !props.contains_key("force"),
        "the proxied schema must not advertise layer/force: {params}"
    );
    let required = params["required"].as_array().expect("required");
    assert!(
        required.contains(&serde_json::json!("target")),
        "the proxied schema must require target: {params}"
    );
}

#[tokio::test]
async fn tool_call_autonomous_opt_out_is_denied() {
    // MR !1506 review (CQ-1): a tool that opts out of autonomous execution
    // must be refused even when it is named in the worker allowlist. The
    // probe uses a phase-1 name so it reaches the autonomous gate rather than
    // being dropped by the load-time sanitizer (MR !1533 DP-2).
    let probe = Arc::new(GateProbeTool {
        tool_name: "knowledge_get_topic".to_string(),
        output: "ok".to_string(),
        autonomous: false,
        calls: AtomicUsize::new(0),
    });
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools.register(probe.clone()).await;

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxy {
        calls: vec![("knowledge_get_topic".to_string(), serde_json::json!({}))],
        replies: reply_tx,
    })
    .await;

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["knowledge_get_topic".to_string()];
    let mgr = ExternalWorkerManager::new(vec![config]).with_tool_proxy(test_proxy(tools));

    let replies = run_proxy_task(&mgr, Uuid::new_v4(), &mut reply_rx, 1).await;
    assert_eq!(replies[0]["payload"]["status"], "denied");
    assert_eq!(
        probe.calls.load(Ordering::SeqCst),
        0,
        "an autonomous opt-out tool must never execute via the proxy"
    );
}

#[tokio::test]
async fn tool_call_denylisted_allowlist_entry_never_executes() {
    // MR !1506 review (CQ-1): autonomous-denylisted names (create_job,
    // restart, secret_*, tool_install, ssh, ...) are stripped from the
    // allowlist at manager load, so a configured grant is dead on arrival.
    let probe = Arc::new(GateProbeTool {
        tool_name: "create_job".to_string(),
        output: "pwned".to_string(),
        autonomous: true,
        calls: AtomicUsize::new(0),
    });
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools.register(probe.clone()).await;

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxy {
        calls: vec![("create_job".to_string(), serde_json::json!({}))],
        replies: reply_tx,
    })
    .await;

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["create_job".to_string()];
    let mgr = ExternalWorkerManager::new(vec![config]).with_tool_proxy(test_proxy(tools));

    let replies = run_proxy_task(&mgr, Uuid::new_v4(), &mut reply_rx, 1).await;
    assert_eq!(replies[0]["payload"]["status"], "denied");
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
}

#[cfg(feature = "libsql")]
#[tokio::test]
async fn tool_call_disabled_permission_is_denied() {
    // MR !1506 review (CQ-1): a user who set a tool to Disabled must not have
    // it executed by a worker, even when the worker allowlist names it.
    let (db, _tmp) = test_db().await;
    db.set_setting(
        "tenant-user",
        "tool_permissions.knowledge_search",
        &serde_json::json!("disabled"),
    )
    .await
    .expect("seed permission");

    let probe = ProbeEchoTool::phase1();
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools.register(probe.clone()).await;

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxy {
        calls: vec![("knowledge_search".to_string(), serde_json::json!({}))],
        replies: reply_tx,
    })
    .await;

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["knowledge_search".to_string()];
    let mgr = ExternalWorkerManager::new(vec![config])
        .with_tool_proxy(test_proxy(tools))
        .with_store(db);

    let replies = run_proxy_task(&mgr, Uuid::new_v4(), &mut reply_rx, 1).await;
    assert_eq!(replies[0]["payload"]["status"], "denied");
    assert!(
        replies[0]["payload"]["error"]
            .as_str()
            .expect("error text")
            .contains("disabled"),
        "denial must name the permission state: {}",
        replies[0]
    );
    assert!(probe.seen.lock().expect("probe lock").is_empty());
}

#[cfg(feature = "libsql")]
#[tokio::test]
async fn proxied_calls_persist_canonical_tool_events() {
    // MR !1506 review (CQ-4, WI#324 AC4): every proxied call appears in
    // job_events in the canonical tool_use {tool_name,input} /
    // tool_result {tool_name,success,output} shape — denied calls included,
    // with success:false so they cannot render as green checkmarks.
    let (db, _tmp) = test_db().await;

    let probe = ProbeEchoTool::phase1();
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools.register(probe.clone()).await;

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxy {
        calls: vec![
            (
                "knowledge_search".to_string(),
                serde_json::json!({ "k": "v" }),
            ),
            ("memory_write".to_string(), serde_json::json!({})),
        ],
        replies: reply_tx,
    })
    .await;

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["knowledge_search".to_string()];
    let mgr = ExternalWorkerManager::new(vec![config])
        .with_tool_proxy(test_proxy(tools))
        .with_store(db.clone());
    let job_id = Uuid::new_v4();

    // job_events.job_id references agent_jobs(id): seed the parent row so
    // the persisted events satisfy the FK (mirrors the create_job path).
    db.save_sandbox_job(&lunarwing::history::SandboxJobRecord {
        id: job_id,
        task: "task".to_string(),
        status: "running".to_string(),
        user_id: "tenant-user".to_string(),
        project_dir: String::new(),
        success: None,
        failure_reason: None,
        created_at: chrono::Utc::now(),
        started_at: Some(chrono::Utc::now()),
        completed_at: None,
        credential_grants_json: String::new(),
    })
    .await
    .expect("seed parent job row");

    let _replies = run_proxy_task(&mgr, job_id, &mut reply_rx, 2).await;

    let events = db
        .list_job_events(job_id, None)
        .await
        .expect("list job events");
    let tool_events: Vec<_> = events
        .iter()
        .filter(|e| matches!(e.event_type.as_str(), "tool_use" | "tool_result"))
        .collect();
    assert_eq!(
        tool_events.len(),
        4,
        "allowed + denied calls each produce tool_use + tool_result: {tool_events:?}"
    );

    assert_eq!(tool_events[0].event_type, "tool_use");
    assert_eq!(tool_events[0].data["tool_name"], "knowledge_search");
    assert!(
        tool_events[0].data["input"]
            .as_str()
            .expect("input preview")
            .contains("\"k\""),
        "input preview carries the call arguments: {}",
        tool_events[0].data["input"]
    );

    assert_eq!(tool_events[1].event_type, "tool_result");
    assert_eq!(tool_events[1].data["tool_name"], "knowledge_search");
    assert_eq!(tool_events[1].data["success"], true);

    assert_eq!(tool_events[2].event_type, "tool_use");
    assert_eq!(tool_events[2].data["tool_name"], "memory_write");
    assert_eq!(tool_events[3].event_type, "tool_result");
    assert_eq!(tool_events[3].data["tool_name"], "memory_write");
    assert_eq!(
        tool_events[3].data["success"], false,
        "a denied call must persist success:false"
    );
}

#[tokio::test]
async fn tool_output_is_sanitized_before_reaching_worker() {
    // MR !1506 review (CQ-2) / WI#324 AC5 regression guard: tool output and
    // error text pass through the SafetyLayer's LeakDetector before the reply
    // is sent — a secret-shaped string must never reach the worker verbatim.
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools
        .register(Arc::new(GateProbeTool {
            tool_name: "knowledge_search".to_string(),
            output: "aws key: AKIAIOSFODNN7EXAMPLE".to_string(),
            autonomous: true,
            calls: AtomicUsize::new(0),
        }))
        .await;

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxy {
        calls: vec![("knowledge_search".to_string(), serde_json::json!({}))],
        replies: reply_tx,
    })
    .await;

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["knowledge_search".to_string()];
    let mgr = ExternalWorkerManager::new(vec![config]).with_tool_proxy(test_proxy(tools));

    let replies = run_proxy_task(&mgr, Uuid::new_v4(), &mut reply_rx, 1).await;
    let output = replies[0]["payload"]["output"]
        .as_str()
        .expect("output string");
    assert!(
        !output.contains("AKIAIOSFODNN7EXAMPLE"),
        "secret-shaped output must be redacted or blocked before reply: {output}"
    );
}

#[tokio::test]
async fn tool_call_rejected_for_terminal_job() {
    // MR !1506 review (CQ-1/CQ-6): with a context manager wired, the real job
    // context is authoritative — a Cancelled (terminal) job gets no new tool
    // calls even if its worker socket is still streaming frames.
    let probe = ProbeEchoTool::phase1();
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools.register(probe.clone()).await;

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxy {
        calls: vec![("knowledge_search".to_string(), serde_json::json!({}))],
        replies: reply_tx,
    })
    .await;

    let cm = Arc::new(ContextManager::new(5));
    let job_id = Uuid::new_v4();
    cm.register_sandbox_job(job_id, "tenant-user", "t", "d")
        .await
        .expect("register job");
    cm.update_context(job_id, |ctx| ctx.state = JobState::Cancelled)
        .await
        .expect("mark cancelled");
    let (tx, _rx) = tokio::sync::broadcast::channel(8);

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["knowledge_search".to_string()];
    let mgr = ExternalWorkerManager::new(vec![config])
        .with_event_deps(tx, cm)
        .with_tool_proxy(test_proxy(tools));

    let replies = run_proxy_task(&mgr, job_id, &mut reply_rx, 1).await;
    assert_eq!(replies[0]["payload"]["status"], "error");
    assert!(
        replies[0]["payload"]["error"]
            .as_str()
            .expect("error text")
            .contains("Cancelled"),
        "terminal-state refusal must name the state: {}",
        replies[0]
    );
    assert!(probe.seen.lock().expect("probe lock").is_empty());
}

#[cfg(feature = "libsql")]
#[tokio::test]
async fn memory_write_read_roundtrip_and_mission_path_guard() {
    // WI#324 AC1: a worker job can call memory_write + memory_read and
    // observe round-trip persistence in the tenant scope. MR !1506 review
    // (CQ-1 scenario C / WI#322): the same worker may NOT write into
    // engine/projects/** mission records while that WI is open.
    let (db, _tmp) = test_db().await;
    let workspace = Arc::new(Workspace::new_with_db("tenant-user", db));

    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools
        .register(Arc::new(MemoryWriteTool::from_workspace(workspace.clone())))
        .await;
    tools
        .register(Arc::new(MemoryReadTool::from_workspace(workspace)))
        .await;

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxy {
        calls: vec![
            (
                "memory_write".to_string(),
                serde_json::json!({
                    "target": "notes/proxied.md",
                    "content": "lunar-proxied-roundtrip",
                    "append": false,
                }),
            ),
            (
                "memory_read".to_string(),
                serde_json::json!({ "path": "notes/proxied.md" }),
            ),
            (
                "memory_write".to_string(),
                serde_json::json!({
                    "target": "engine/projects/proj/missions/m1/mission.json",
                    "content": "forged mission",
                    "append": false,
                }),
            ),
            // MR !1521 DD-3 / MR !1533 DP-5: the engine-tree guard is NOT
            // missions-only — engine/projects/** project documents are
            // engine-internal too and must be refused the same way.
            (
                "memory_write".to_string(),
                serde_json::json!({
                    "target": "engine/projects/x/project.json",
                    "content": "forged project",
                    "append": false,
                }),
            ),
        ],
        replies: reply_tx,
    })
    .await;

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["memory_write".to_string(), "memory_read".to_string()];
    let mgr = ExternalWorkerManager::new(vec![config]).with_tool_proxy(test_proxy(tools));

    let replies = run_proxy_task(&mgr, Uuid::new_v4(), &mut reply_rx, 4).await;

    assert_eq!(
        replies[0]["payload"]["status"], "success",
        "memory_write must succeed: {}",
        replies[0]
    );
    assert_eq!(replies[1]["payload"]["status"], "success");
    let read_output = replies[1]["payload"]["output"]
        .as_str()
        .expect("read output");
    assert!(
        read_output.contains("lunar-proxied-roundtrip"),
        "memory_read must observe the proxied write (tenant scope): {read_output}"
    );

    assert_eq!(
        replies[2]["payload"]["status"], "denied",
        "mission-tree writes are refused while WI#322 is open: {}",
        replies[2]
    );
    assert!(
        replies[2]["payload"]["error"]
            .as_str()
            .expect("error text")
            .contains("WI#322"),
        "the refusal must name the guard: {}",
        replies[2]
    );

    assert_eq!(
        replies[3]["payload"]["status"], "denied",
        "engine/projects/** document writes are refused while WI#322 is open: {}",
        replies[3]
    );
    assert!(
        replies[3]["payload"]["error"]
            .as_str()
            .expect("error text")
            .contains("WI#322"),
        "the refusal must name the guard: {}",
        replies[3]
    );
}

// ── MR !1533 review (DP-1/DP-2/DP-3/DP-5): hardening pins ────────────

#[tokio::test]
async fn tool_call_oversized_frame_plain_and_escaped_type_is_rejected() {
    // MR !1533 DP-1/DP-5: a `tool_call` frame over the 256 KiB bound is
    // rejected with `error` whether its `type` is spelled plain or with a
    // JSON escape — serde_json cannot borrow an escaped string, which is why
    // the probe field is a `Cow`. A small escaped-type frame still classifies
    // and dispatches normally, and the read loop keeps serving later frames.
    let probe = Arc::new(GateProbeTool {
        tool_name: "knowledge_search".to_string(),
        output: "ok".to_string(),
        autonomous: true,
        calls: AtomicUsize::new(0),
    });
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools.register(probe.clone()).await;

    let pad = "A".repeat(300 * 1024);
    let oversized = |ty: &str, call_id: &str| {
        format!(
            r#"{{"id":"m","type":"{ty}","timestamp":"2026-01-01T00:00:00Z","payload":{{"task_id":"@task_id@","call_id":"{call_id}","name":"knowledge_search","arguments":{{"pad":"{pad}"}}}}}}"#
        )
    };
    let small_escaped = r#"{"id":"m","type":"\u0074ool_call","timestamp":"2026-01-01T00:00:00Z","payload":{"task_id":"@task_id@","call_id":"c-esc-small","name":"knowledge_search","arguments":{}}}"#
        .to_string();
    let small_plain = r#"{"id":"m","type":"tool_call","timestamp":"2026-01-01T00:00:00Z","payload":{"task_id":"@task_id@","call_id":"c-plain-small","name":"knowledge_search","arguments":{}}}"#
        .to_string();

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxyVerbatim {
        frames: vec![
            small_escaped,
            oversized("tool_call", "c-big-plain"),
            oversized("\\u0074ool_call", "c-big-esc"),
            small_plain,
        ],
        replies: reply_tx,
    })
    .await;

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["knowledge_search".to_string()];
    let mgr = ExternalWorkerManager::new(vec![config]).with_tool_proxy(test_proxy(tools));

    let replies = run_proxy_task(&mgr, Uuid::new_v4(), &mut reply_rx, 4).await;
    assert_eq!(
        replies[0]["payload"]["status"], "success",
        "a small escaped-type frame classifies and dispatches: {}",
        replies[0]
    );
    for (i, reply) in replies[1..3].iter().enumerate() {
        assert_eq!(reply["payload"]["status"], "error", "frame {i}: {reply}");
        assert!(
            reply["payload"]["error"]
                .as_str()
                .expect("error text")
                .contains("exceeds"),
            "oversized rejection must name the bound: {reply}"
        );
    }
    assert_eq!(
        replies[3]["payload"]["status"], "success",
        "the read loop keeps serving frames after oversized rejects: {}",
        replies[3]
    );
    assert_eq!(
        probe.calls.load(Ordering::SeqCst),
        2,
        "only the two small frames may execute"
    );
}

#[tokio::test]
async fn tool_call_giant_frame_is_capped_at_the_socket() {
    // MR !1540 DX-1/DX-3: both message and individual-frame limits are 16 MiB;
    // a larger frame fails the job at the transport boundary.
    let giant = format!(
        r#"{{"id":"m","type":"tool_call","timestamp":"2026-01-01T00:00:00Z","payload":{{"task_id":"@task_id@","call_id":"c-huge","name":"knowledge_search","arguments":{{"pad":"{}"}}}}}}"#,
        "A".repeat(17 * 1024 * 1024)
    );

    let (reply_tx, _reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxyVerbatim {
        frames: vec![giant],
        replies: reply_tx,
    })
    .await;

    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    let mut config = config_for(&url);
    config.tool_allowlist = vec!["knowledge_search".to_string()];
    let mgr = ExternalWorkerManager::new(vec![config]).with_tool_proxy(test_proxy(tools));

    let err = timeout(
        TEST_BOUND,
        mgr.execute_task(
            Uuid::new_v4(),
            "mock",
            "task",
            Some(10_000),
            true,
            TaskContext::default(),
        ),
    )
    .await
    .expect("test timed out")
    .expect_err("a frame past the socket cap must fail the job");
    assert!(
        matches!(err, OrchestratorError::ExternalWorkerProtocolError { .. }),
        "got {err:?}"
    );
    assert!(err.to_string().contains("Message too long"), "{err}");
}

#[tokio::test]
async fn tool_call_proxy_denylisted_allowlist_entry_never_executes() {
    // MR !1521 DD-4 / MR !1533 DP-2/DP-5: host-read dev tools are on the proxy
    // denylist AND outside the phase-1 capability set — an operator entry like
    // `tool_allowlist = ["read_file"]` is stripped at load, so the grant is
    // dead on arrival even though `read_file` passes the autonomous gates.
    let probe = Arc::new(GateProbeTool {
        tool_name: "read_file".to_string(),
        output: "pwned".to_string(),
        autonomous: true,
        calls: AtomicUsize::new(0),
    });
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools.register(probe.clone()).await;

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxy {
        calls: vec![("read_file".to_string(), serde_json::json!({}))],
        replies: reply_tx,
    })
    .await;

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["read_file".to_string()];
    let mgr = ExternalWorkerManager::new(vec![config]).with_tool_proxy(test_proxy(tools));

    let replies = run_proxy_task(&mgr, Uuid::new_v4(), &mut reply_rx, 1).await;
    assert_eq!(replies[0]["payload"]["status"], "denied");
    assert_eq!(
        probe.calls.load(Ordering::SeqCst),
        0,
        "a proxy-denylisted tool must never execute via the proxy"
    );
}

#[cfg(feature = "libsql")]
#[tokio::test]
async fn tool_call_cap_bounds_frames_and_marker_rows() {
    // MR !1521 DD-5 / MR !1533 DP-3/DP-5: every tool_call-typed frame counts
    // toward the 100-per-job cap; frame 101 is denied with exactly one marker
    // row persisted, and later frames are denied with no further rows. The cap
    // path validates worker-controlled name/call_id before persisting or
    // echoing (an invalid pair falls back to "unknown"/"" and is never stored).
    let (db, _tmp) = test_db().await;

    let probe = Arc::new(GateProbeTool {
        tool_name: "knowledge_search".to_string(),
        output: "ok".to_string(),
        autonomous: true,
        calls: AtomicUsize::new(0),
    });
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools.register(probe.clone()).await;

    let mut frames: Vec<(String, serde_json::Value)> = (0..102)
        .map(|i| {
            (
                "tool_call".to_string(),
                serde_json::json!({
                    "task_id": "@task_id@",
                    "call_id": format!("call-{i}"),
                    "name": "knowledge_search",
                    "arguments": {},
                }),
            )
        })
        .collect();
    // Past the cap with invalid identifiers: the reply must echo "" and no
    // row may carry the unvalidated values (MR !1533 DP-3).
    frames.push((
        "tool_call".to_string(),
        serde_json::json!({
            "task_id": "@task_id@",
            "call_id": "evil\nid",
            "name": "evil\nname",
            "arguments": {},
        }),
    ));

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxyRaw {
        frames,
        replies: reply_tx,
    })
    .await;

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["knowledge_search".to_string()];
    let mgr = ExternalWorkerManager::new(vec![config])
        .with_tool_proxy(test_proxy(tools))
        .with_store(db.clone());
    let job_id = Uuid::new_v4();

    // job_events.job_id references agent_jobs(id): seed the parent row.
    db.save_sandbox_job(&lunarwing::history::SandboxJobRecord {
        id: job_id,
        task: "task".to_string(),
        status: "running".to_string(),
        user_id: "tenant-user".to_string(),
        project_dir: String::new(),
        success: None,
        failure_reason: None,
        created_at: chrono::Utc::now(),
        started_at: Some(chrono::Utc::now()),
        completed_at: None,
        credential_grants_json: String::new(),
    })
    .await
    .expect("seed parent job row");

    let replies = run_proxy_task(&mgr, job_id, &mut reply_rx, 103).await;

    for reply in &replies[..100] {
        assert_eq!(reply["payload"]["status"], "success", "{reply}");
    }
    assert_eq!(
        probe.calls.load(Ordering::SeqCst),
        100,
        "exactly the in-cap frames may execute"
    );
    for reply in &replies[100..102] {
        assert_eq!(reply["payload"]["status"], "denied", "{reply}");
        assert!(
            reply["payload"]["error"]
                .as_str()
                .expect("error text")
                .contains("per-job proxied tool call cap"),
            "cap denial must name the cap: {reply}"
        );
    }
    assert_eq!(
        replies[102]["payload"]["status"], "denied",
        "{}",
        replies[102]
    );
    assert_eq!(
        replies[102]["payload"]["call_id"], "",
        "an invalid call_id is never echoed from the cap path: {}",
        replies[102]
    );

    let events = db
        .list_job_events(job_id, None)
        .await
        .expect("list job events");
    let cap_rows: Vec<_> = events
        .iter()
        .filter(|e| {
            e.event_type == "tool_result"
                && e.data["output"]
                    .as_str()
                    .is_some_and(|o| o.contains("call cap"))
        })
        .collect();
    assert_eq!(
        cap_rows.len(),
        1,
        "exactly one cap marker row, however many frames flood past the cap: {cap_rows:?}"
    );
    assert_eq!(
        cap_rows[0].data["tool_name"], "knowledge_search",
        "the marker row carries the validated name of the first capped frame"
    );
    assert_eq!(cap_rows[0].data["success"], false);
}

#[path = "external_worker_integration/proxy_review.rs"]
mod proxy_review;

// ── WI#325: cron worker mode ────────────────────────────────────────

/// Cron-mode worker config with a short watchdog so tests resolve quickly.
fn cron_config_for(
    url: &str,
    heartbeat_timeout_secs: u64,
    max_reentries: u32,
) -> ExternalWorkerConfig {
    cron_config_with_schedule(url, "0 15 10 * * *", heartbeat_timeout_secs, max_reentries)
}

fn cron_config_with_schedule(
    url: &str,
    schedule: &str,
    heartbeat_timeout_secs: u64,
    max_reentries: u32,
) -> ExternalWorkerConfig {
    ExternalWorkerConfig {
        name: "mock".to_string(),
        url: url.to_string(),
        timeout_ms: 30_000,
        endpoints: vec![WorkerEndpoint {
            url: url.to_string(),
            auth_token: None,
            weight: None,
        }],
        mode: ExternalWorkerMode::Cron,
        cron: Some(CronWorkerConfig {
            schedule: schedule.to_string(),
            goal: "Watch the herd".to_string(),
            mission_id: Some("test-mission".to_string()),
            heartbeat_timeout_secs: Some(heartbeat_timeout_secs),
            tick_timeout_ms: Some(60_000),
            max_reentries: Some(max_reentries),
            allow_subminute_schedule: None,
            project_dir: None,
            timezone: None,
            tool_allowlist: Vec::new(),
        }),
        ..Default::default()
    }
}

/// Spawn a manager with its cron supervisor running; returns the manager and
/// the shutdown sender so tests can signal teardown.
fn spawn_cron_manager(
    config: ExternalWorkerConfig,
) -> (
    Arc<ExternalWorkerManager>,
    Arc<ContextManager>,
    tokio::sync::broadcast::Sender<()>,
) {
    let context_manager = Arc::new(ContextManager::new(16));
    let (event_tx, _event_rx) = tokio::sync::broadcast::channel(16);
    let mgr = Arc::new(
        ExternalWorkerManager::new(vec![config])
            .with_event_deps(event_tx, Arc::clone(&context_manager)),
    );
    let (shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel(1);
    mgr.spawn_cron_supervisors("owner-1", shutdown_rx);
    (mgr, context_manager, shutdown_tx)
}

/// A cron tick dispatch must put the structured `cron.tick` event on the
/// wire (WI#325 ask 1): mission id, schedule, goal, tick/chain position —
/// the worker surfaces as a live mission without a bespoke integration.
#[tokio::test]
async fn cron_tick_reaches_worker_with_structured_tick_context() {
    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::Capture {
        requests: request_tx,
    })
    .await;
    let (mgr, _cm, _shutdown) = spawn_cron_manager(cron_config_for(&url, 10, 0));

    timeout(TEST_BOUND, mgr.fire_cron_tick_now("mock", "owner-1"))
        .await
        .expect("tick timed out")
        .expect("tick dispatch errored");

    let request = request_rx.recv().await.expect("task_request captured");
    let tick = &request["payload"]["context"]["cron_tick"];
    assert_eq!(tick["event"], "cron.tick");
    assert_eq!(tick["mission_id"], "test-mission");
    assert_eq!(tick["schedule"], "0 15 10 * * *");
    assert_eq!(tick["goal"], "Watch the herd");
    assert_eq!(tick["tick_index"], 0);
    assert_eq!(tick["chain_index"], 0);
    assert!(
        tick["fired_at"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    // The prompt carries the mission goal and the workspace handoff contract.
    let prompt = request["payload"]["prompt"].as_str().expect("prompt");
    assert!(prompt.contains("Watch the herd"));
    // No explicit project_dir → the default missions/<mission_id> is sent.
    assert_eq!(
        request["payload"]["context"]["project_dir"].as_str(),
        Some("missions/test-mission")
    );
    assert!(prompt.contains("missions/test-mission"));
}

/// The re-entry supervisor (WI#325 ask 3): when a tick job wedges (no
/// heartbeat within the window), the supervisor cancels it and dispatches a
/// fresh bounded job for the same tick, advancing chain_index under the same
/// mission id with a fresh job id per link.
#[tokio::test]
async fn cron_supervisor_redispatches_wedged_tick_job() {
    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::CaptureHang {
        requests: request_tx,
    })
    .await;
    let (mgr, _cm, _shutdown) = spawn_cron_manager(cron_config_for(&url, 2, 1));

    timeout(TEST_BOUND, mgr.fire_cron_tick_now("mock", "owner-1"))
        .await
        .expect("tick timed out")
        .expect("tick errored");

    let first = request_rx.recv().await.expect("first dispatch captured");
    let second = timeout(TEST_BOUND * 3, request_rx.recv())
        .await
        .expect("re-dispatch after heartbeat lapse timed out")
        .expect("re-dispatch captured");
    let first_tick = &first["payload"]["context"]["cron_tick"];
    let second_tick = &second["payload"]["context"]["cron_tick"];
    assert_eq!(first_tick["chain_index"], 0);
    assert_eq!(second_tick["chain_index"], 1);
    // Same mission across the chain…
    assert_eq!(first_tick["mission_id"], "test-mission");
    assert_eq!(second_tick["mission_id"], "test-mission");
    // …but each chain link is its own observable job.
    assert_ne!(first["payload"]["task_id"], second["payload"]["task_id"]);
}

/// Task-mode workers must reject the manual cron fire path: cron ticks are a
/// mission-supervisor concept, not a per-dispatch flag (WI#325 ask 2 — the
/// mode comes from worker config).
#[tokio::test]
async fn cron_manual_fire_rejects_task_mode_worker() {
    let (request_tx, _request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::Capture {
        requests: request_tx,
    })
    .await;
    let mgr = Arc::new(ExternalWorkerManager::new(vec![config_for(&url)]));

    let err = mgr
        .fire_cron_tick_now("mock", "owner-1")
        .await
        .expect_err("task-mode worker must reject cron fire");
    assert!(matches!(
        err,
        OrchestratorError::ExternalWorkerNotCron { .. }
    ));
}

/// A cron-mode worker whose supervisor never started (e.g. invalid config)
/// reports that distinctly from a task-mode worker.
#[tokio::test]
async fn cron_manual_fire_without_supervisor_reports_not_running() {
    let (request_tx, _request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::Capture {
        requests: request_tx,
    })
    .await;
    // Cron-mode config but spawn_cron_supervisors never called.
    let mgr = Arc::new(ExternalWorkerManager::new(vec![cron_config_for(
        &url, 10, 0,
    )]));

    let err = mgr
        .fire_cron_tick_now("mock", "owner-1")
        .await
        .expect_err("missing supervisor must error");
    assert!(matches!(
        err,
        OrchestratorError::CronSupervisorNotRunning { .. }
    ));
}

/// The scheduled loop itself (not just the manual fire path) dispatches a
/// tick when the cron schedule fires, with a monotonic tick_index.
#[tokio::test]
async fn cron_scheduled_loop_fires_on_schedule() {
    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::Capture {
        requests: request_tx,
    })
    .await;
    // Per-second schedule; spawn-time validation only checks parseability
    // (the runaway floor is enforced at config resolution, tested in
    // config::sandbox tests).
    let (_mgr, _cm, _shutdown) =
        spawn_cron_manager(cron_config_with_schedule(&url, "* * * * * *", 10, 0));

    let first = timeout(TEST_BOUND, request_rx.recv())
        .await
        .expect("first scheduled tick timed out")
        .expect("first scheduled tick captured");
    let second = timeout(TEST_BOUND, request_rx.recv())
        .await
        .expect("second scheduled tick timed out")
        .expect("second scheduled tick captured");
    let first_index = first["payload"]["context"]["cron_tick"]["tick_index"]
        .as_u64()
        .expect("tick_index");
    let second_index = second["payload"]["context"]["cron_tick"]["tick_index"]
        .as_u64()
        .expect("tick_index");
    assert!(
        second_index > first_index,
        "scheduled ticks must be monotonic: {first_index} then {second_index}"
    );
}

/// Shutdown mid-tick: the watch loop observes the shutdown signal, cancels
/// the in-flight link with the shutdown reason, and the mission loop exits.
#[tokio::test]
async fn cron_shutdown_mid_tick_cancels_running_job() {
    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::CaptureHang {
        requests: request_tx,
    })
    .await;
    let (mgr, cm, shutdown) = spawn_cron_manager(cron_config_for(&url, 60, 3));

    timeout(TEST_BOUND, mgr.fire_cron_tick_now("mock", "owner-1"))
        .await
        .expect("tick timed out")
        .expect("tick errored");
    let request = request_rx.recv().await.expect("dispatch captured");
    let job_id = Uuid::parse_str(
        request["payload"]["task_id"]
            .as_str()
            .expect("task_id string"),
    )
    .expect("task_id uuid");

    let _ = shutdown.send(());

    // The job must reach Cancelled with the shutdown reason, and no further
    // dispatch may follow.
    timeout(TEST_BOUND * 2, async {
        loop {
            if let Ok(ctx) = cm.get_context(job_id).await
                && ctx.state == JobState::Cancelled
            {
                return ctx;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("job never reached Cancelled after shutdown");
    let ctx = cm.get_context(job_id).await.expect("job context");
    let reason = ctx
        .transitions
        .iter()
        .rev()
        .find(|t| t.to == JobState::Cancelled)
        .and_then(|t| t.reason.clone())
        .unwrap_or_default();
    assert!(
        reason.contains("daemon shutdown"),
        "cancel reason must name the shutdown, got: {reason}"
    );
    assert!(
        timeout(Duration::from_secs(3), request_rx.recv())
            .await
            .is_err(),
        "no further dispatch may follow a shutdown"
    );
}

/// A failed link (worker returns an error result) is re-dispatched on the
/// Failed path, not just the heartbeat-lapse path.
#[tokio::test]
async fn cron_reentry_on_failed_link() {
    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::CaptureFail {
        requests: request_tx,
        error: "boom".to_string(),
    })
    .await;
    let (mgr, _cm, _shutdown) = spawn_cron_manager(cron_config_for(&url, 30, 1));

    timeout(TEST_BOUND, mgr.fire_cron_tick_now("mock", "owner-1"))
        .await
        .expect("tick timed out")
        .expect("tick errored");

    let first = request_rx.recv().await.expect("first dispatch captured");
    let second = timeout(TEST_BOUND, request_rx.recv())
        .await
        .expect("re-dispatch after failure timed out")
        .expect("re-dispatch captured");
    assert_eq!(first["payload"]["context"]["cron_tick"]["chain_index"], 0);
    assert_eq!(second["payload"]["context"]["cron_tick"]["chain_index"], 1);
}

/// An owner cancel of the tick job ends the tick with NO re-entry: a user
/// cancel is a deliberate stop, not a wedge.
#[tokio::test]
async fn cron_user_cancel_ends_tick_without_reentry() {
    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::CaptureHang {
        requests: request_tx,
    })
    .await;
    // Long heartbeat so the lapse watchdog cannot fire before the cancel.
    let (mgr, _cm, _shutdown) = spawn_cron_manager(cron_config_for(&url, 300, 3));

    timeout(TEST_BOUND, mgr.fire_cron_tick_now("mock", "owner-1"))
        .await
        .expect("tick timed out")
        .expect("tick errored");
    let request = request_rx.recv().await.expect("dispatch captured");
    let job_id = Uuid::parse_str(
        request["payload"]["task_id"]
            .as_str()
            .expect("task_id string"),
    )
    .expect("task_id uuid");

    assert!(mgr.cancel_task(job_id).await, "cancel must find the job");

    assert!(
        timeout(TEST_BOUND, request_rx.recv()).await.is_err(),
        "a user-cancelled tick must not be re-dispatched"
    );
}

/// The re-entry budget is honored: with max_reentries = 2 a deterministically
/// failing worker gets exactly 3 dispatches (initial + 2 re-entries), then
/// the tick is abandoned.
#[tokio::test]
async fn cron_tick_abandoned_after_max_reentries() {
    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::CaptureFail {
        requests: request_tx,
        error: "boom".to_string(),
    })
    .await;
    let (mgr, _cm, _shutdown) = spawn_cron_manager(cron_config_for(&url, 30, 2));

    timeout(TEST_BOUND, mgr.fire_cron_tick_now("mock", "owner-1"))
        .await
        .expect("tick timed out")
        .expect("tick errored");

    for expected_chain in 0..=2u64 {
        let request = timeout(TEST_BOUND, request_rx.recv())
            .await
            .expect("dispatch timed out")
            .expect("dispatch captured");
        assert_eq!(
            request["payload"]["context"]["cron_tick"]["chain_index"],
            expected_chain
        );
    }
    assert!(
        timeout(TEST_BOUND, request_rx.recv()).await.is_err(),
        "a tick past its re-entry budget must not dispatch again"
    );
    assert_eq!(
        mgr.cron_mission_status("mock", "owner-1")
            .await
            .unwrap()
            .consecutive_abandoned,
        1,
        "the tick must be abandoned, not waiting to dispatch an illegal fourth link"
    );
}

/// CU-1 with a lock-enforcing mock: the supervisor cancels the wedged link,
/// waits for it to release the workspace, and a busy rejection retries the
/// same chain link instead of spending re-entry budget.
#[tokio::test]
async fn cron_busy_rejection_retries_without_spending_reentry_budget() {
    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let active = std::sync::Arc::new(AtomicUsize::new(0));
    let busy_rejections = std::sync::Arc::new(AtomicUsize::new(0));
    let (url, _count) = spawn_mock_worker_async(MockBehavior::LockAwareHang {
        requests: request_tx,
        active: active.clone(),
        busy_rejections: busy_rejections.clone(),
        hold: Duration::from_secs(2),
    })
    .await;
    let (mgr, _cm, _shutdown) = spawn_cron_manager(cron_config_for(&url, 2, 1));

    timeout(TEST_BOUND, mgr.fire_cron_tick_now("mock", "owner-1"))
        .await
        .expect("tick timed out")
        .expect("tick errored");

    // Only lock-holding links are captured (busy rejections are not): link 1
    // (chain 0) wedges holding the lock; lapse → supervisor cancel → bounded
    // terminal wait → backoff; link 2 (chain 1) arrives while the mock still
    // holds the lock and is rejected busy (not captured); the busy retry —
    // still chain 1, a fresh job id — lands after the lock frees and is
    // captured. The retry happening at all, with chain 1 == max_reentries,
    // proves the busy rejection did not consume the re-entry budget.
    let first = request_rx.recv().await.expect("link 1 captured");
    let second = timeout(TEST_BOUND * 4, request_rx.recv())
        .await
        .expect("busy retry timed out")
        .expect("busy retry captured");

    assert_eq!(first["payload"]["context"]["cron_tick"]["chain_index"], 0);
    assert_eq!(second["payload"]["context"]["cron_tick"]["chain_index"], 1);
    assert_ne!(first["payload"]["task_id"], second["payload"]["task_id"]);
    assert_eq!(
        busy_rejections.load(Ordering::SeqCst),
        1,
        "exactly one dispatch should have hit the held workspace lock"
    );
    // The retry wedges too, lapses, and the tick is then abandoned at the
    // re-entry cap: no third link is dispatched.
    assert!(
        timeout(TEST_BOUND, request_rx.recv()).await.is_err(),
        "a tick at its re-entry cap must not dispatch again"
    );
    assert_eq!(
        mgr.cron_mission_status("mock", "owner-1")
            .await
            .unwrap()
            .consecutive_abandoned,
        1
    );
}

/// Owner-gated mission control: pause/resume/status flow through the
/// supervisor command channel, and a different owner is rejected.
#[tokio::test]
async fn cron_mission_pause_resume_status_owner_gated() {
    let (request_tx, _request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::Capture {
        requests: request_tx,
    })
    .await;
    let (mgr, _cm, _shutdown) = spawn_cron_manager(cron_config_for(&url, 30, 0));

    // Status of a running mission.
    let status = mgr
        .cron_mission_status("mock", "owner-1")
        .await
        .expect("status");
    assert_eq!(status.mission_id, "test-mission");
    assert!(!status.paused);

    // Pause persists in live state.
    mgr.pause_cron_mission("mock", "owner-1", CronCommandOrigin::OwnerTool)
        .await
        .expect("pause");
    let status = mgr
        .cron_mission_status("mock", "owner-1")
        .await
        .expect("status after pause");
    assert!(status.paused);

    // A different owner may not drive the mission.
    let err = mgr
        .pause_cron_mission("mock", "someone-else", CronCommandOrigin::OwnerTool)
        .await
        .expect_err("owner gate");
    assert!(matches!(err, OrchestratorError::CronMissionNotOwned { .. }));
    let err = mgr
        .fire_cron_tick_now("mock", "someone-else")
        .await
        .expect_err("owner gate on fire");
    assert!(matches!(err, OrchestratorError::CronMissionNotOwned { .. }));

    // Resume clears the pause.
    mgr.resume_cron_mission("mock", "owner-1", CronCommandOrigin::OwnerTool)
        .await
        .expect("resume");
    let status = mgr
        .cron_mission_status("mock", "owner-1")
        .await
        .expect("status after resume");
    assert!(!status.paused);
}

/// WI#325 logging (ask 4): every tick transition lands in the persisted
/// `job_events` store — structured `cron_tick` on dispatch and
/// `cron_tick_end` on resolution.
#[cfg(feature = "libsql")]
#[tokio::test]
async fn cron_tick_events_persist_to_job_events() {
    use lunarwing::db::Database;
    use lunarwing::db::libsql::LibSqlBackend;

    // A temp-file db: `:memory:` libsql databases are per-connection, and the
    // backend opens a fresh connection per call.
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let backend = LibSqlBackend::new_local(&temp_dir.path().join("cron.db"))
        .await
        .expect("test db");
    backend.run_migrations().await.expect("migrations");
    let db: Arc<dyn Database> = Arc::new(backend);

    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::Capture {
        requests: request_tx,
    })
    .await;
    let context_manager = Arc::new(ContextManager::new(16));
    let (event_tx, _event_rx) = tokio::sync::broadcast::channel(16);
    let mgr = Arc::new(
        ExternalWorkerManager::new(vec![cron_config_for(&url, 30, 0)])
            .with_event_deps(event_tx, Arc::clone(&context_manager))
            .with_store(db.clone()),
    );
    let (shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel(1);
    mgr.spawn_cron_supervisors("owner-1", shutdown_rx);
    let _ = shutdown_tx;

    timeout(TEST_BOUND, mgr.fire_cron_tick_now("mock", "owner-1"))
        .await
        .expect("tick timed out")
        .expect("tick errored");
    let request = request_rx.recv().await.expect("dispatch captured");
    let job_id = Uuid::parse_str(
        request["payload"]["task_id"]
            .as_str()
            .expect("task_id string"),
    )
    .expect("task_id uuid");

    // The tick resolves asynchronously after the dispatch frame; poll the
    // store until both supervisor rows are present.
    timeout(TEST_BOUND, async {
        loop {
            let events = db
                .list_job_events(job_id, None)
                .await
                .expect("list job events");
            let has_tick = events.iter().any(|e| e.event_type == "cron_tick");
            let has_end = events.iter().any(|e| e.event_type == "cron_tick_end");
            if has_tick && has_end {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("cron events never persisted");

    let events = db.list_job_events(job_id, None).await.expect("events");
    let tick_row = events
        .iter()
        .find(|e| e.event_type == "cron_tick")
        .expect("cron_tick row");
    assert_eq!(tick_row.data["mission_id"], "test-mission");
    let end_row = events
        .iter()
        .find(|e| e.event_type == "cron_tick_end")
        .expect("cron_tick_end row");
    assert_eq!(end_row.data["outcome"], "completed");
}

/// MR !1522 DE-3: owner commands are serviced while a tick chain is running
/// — status and pause must answer immediately, not after the chain ends.
/// (On the pre-fix loop this test timed out: `run_tick` was awaited inside
/// the sleep arm, so the command channel was not polled during a tick.)
#[tokio::test]
async fn cron_status_and_pause_answer_during_running_tick() {
    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::CaptureHang {
        requests: request_tx,
    })
    .await;
    // Long heartbeat: the link stays "alive" for the whole test, so the tick
    // chain is in its watch phase when the commands arrive.
    let (mgr, _cm, _shutdown) = spawn_cron_manager(cron_config_for(&url, 300, 3));

    timeout(TEST_BOUND, mgr.fire_cron_tick_now("mock", "owner-1"))
        .await
        .expect("tick timed out")
        .expect("tick errored");
    request_rx.recv().await.expect("dispatch captured");

    // Status during the running tick answers promptly.
    let status = timeout(TEST_BOUND, mgr.cron_mission_status("mock", "owner-1"))
        .await
        .expect("status starved by the running tick")
        .expect("status");
    assert_eq!(status.mission_id, "test-mission");
    assert!(!status.paused);

    // Pause during the running tick applies immediately...
    timeout(
        TEST_BOUND,
        mgr.pause_cron_mission("mock", "owner-1", CronCommandOrigin::OwnerTool),
    )
    .await
    .expect("pause starved by the running tick")
    .expect("pause");
    let status = timeout(TEST_BOUND, mgr.cron_mission_status("mock", "owner-1"))
        .await
        .expect("status starved")
        .expect("status");
    assert!(status.paused);

    // ...and resume restores it (also immediately).
    timeout(
        TEST_BOUND,
        mgr.resume_cron_mission("mock", "owner-1", CronCommandOrigin::OwnerTool),
    )
    .await
    .expect("resume starved")
    .expect("resume");
    let status = timeout(TEST_BOUND, mgr.cron_mission_status("mock", "owner-1"))
        .await
        .expect("status starved")
        .expect("status");
    assert!(!status.paused);
}

/// MR !1522 DE-3: an owner pause stops the re-entry chain at the next link
/// boundary — a wedged link that lapses while paused is NOT re-dispatched.
#[tokio::test]
async fn cron_pause_stops_pending_reentries() {
    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::CaptureHang {
        requests: request_tx,
    })
    .await;
    // Short heartbeat so the link lapses quickly; re-entries would follow.
    let (mgr, _cm, _shutdown) = spawn_cron_manager(cron_config_for(&url, 2, 3));

    timeout(TEST_BOUND, mgr.fire_cron_tick_now("mock", "owner-1"))
        .await
        .expect("tick timed out")
        .expect("tick errored");
    request_rx.recv().await.expect("link 1 captured");

    mgr.pause_cron_mission("mock", "owner-1", CronCommandOrigin::OwnerTool)
        .await
        .expect("pause");

    // Without the boundary check the chain would re-dispatch after the lapse
    // (~2s) + terminal wait + backoff (~4s). Allow generous slack.
    assert!(
        timeout(TEST_BOUND * 2, request_rx.recv()).await.is_err(),
        "a paused mission must not re-dispatch its tick chain"
    );
    let status = mgr
        .cron_mission_status("mock", "owner-1")
        .await
        .expect("status");
    assert!(status.paused);
}

/// MR !1522 DE-6: a non-retryable worker rejection abandons the tick with NO
/// re-dispatch, and three consecutive abandons trip the circuit breaker into
/// a persisted owner-visible pause.
#[tokio::test]
async fn cron_non_retryable_abandons_and_breaker_trips() {
    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::CaptureFail {
        requests: request_tx,
        error: "timeout_ms must be an integer between 1 and 14400000".to_string(),
    })
    .await;
    // High re-entry budget: if non-retryable classification regressed, the
    // tick would re-dispatch several times instead of abandoning at once.
    let (mgr, _cm, _shutdown) = spawn_cron_manager(cron_config_for(&url, 30, 5));

    for tick in 1..=3u32 {
        timeout(TEST_BOUND, mgr.fire_cron_tick_now("mock", "owner-1"))
            .await
            .expect("tick timed out")
            .expect("tick errored");
        let request = timeout(TEST_BOUND, request_rx.recv())
            .await
            .expect("dispatch timed out")
            .expect("dispatch captured");
        assert_eq!(
            request["payload"]["context"]["cron_tick"]["chain_index"], 0,
            "tick {tick}: a non-retryable rejection must not re-dispatch"
        );
        // The status assertion below pins abandonment. This short negative
        // window alone cannot exclude a 5s poll + 4s backoff re-dispatch.
        assert!(
            timeout(Duration::from_secs(8), request_rx.recv())
                .await
                .is_err(),
            "tick {tick}: non-retryable rejection must abandon immediately"
        );
        let status = mgr
            .cron_mission_status("mock", "owner-1")
            .await
            .expect("status");
        assert_eq!(status.consecutive_abandoned, tick, "tick {tick}");
    }

    let status = mgr
        .cron_mission_status("mock", "owner-1")
        .await
        .expect("status");
    assert!(
        status.paused,
        "circuit breaker must pause the mission after 3 consecutive abandons"
    );

    // Owner resume clears the breaker (and is how the mission recovers).
    mgr.resume_cron_mission("mock", "owner-1", CronCommandOrigin::OwnerTool)
        .await
        .expect("resume");
    let status = mgr
        .cron_mission_status("mock", "owner-1")
        .await
        .expect("status");
    assert!(!status.paused);
    assert_eq!(status.consecutive_abandoned, 0);
}

/// MR !1522 DE-6: a terminal tick job's in-memory context is evicted so a
/// long-lived mission cannot grow the ContextManager without bound (the DB
/// record remains the durable log).
#[tokio::test]
async fn cron_tick_context_is_evicted_after_resolution() {
    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::Capture {
        requests: request_tx,
    })
    .await;
    let (mgr, cm, _shutdown) = spawn_cron_manager(cron_config_for(&url, 30, 0));

    timeout(TEST_BOUND, mgr.fire_cron_tick_now("mock", "owner-1"))
        .await
        .expect("tick timed out")
        .expect("tick errored");
    let request = request_rx.recv().await.expect("dispatch captured");
    let job_id = Uuid::parse_str(
        request["payload"]["task_id"]
            .as_str()
            .expect("task_id string"),
    )
    .expect("task_id uuid");

    // The Capture mock completes the job; the supervisor then persists the
    // tick end and evicts the context.
    timeout(TEST_BOUND, async {
        loop {
            if cm.get_context(job_id).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("terminal tick context was never evicted");
}

/// MR !1522 DE-9: for a non-owner caller the gate returns one uniform
/// `CronMissionNotOwned` error whether the named worker is cron-mode with a
/// live supervisor, a nonexistent worker, or plain task-mode —
/// the error surface must not reveal worker modes. The owner still gets the
/// precise distinctions.
#[tokio::test]
async fn cron_owner_gate_is_uniform_for_non_owners() {
    let (request_tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::Capture {
        requests: request_tx,
    })
    .await;
    let mut plain = config_for(&url);
    plain.name = "plain".to_string();
    let context_manager = Arc::new(ContextManager::new(16));
    let (event_tx, _event_rx) = tokio::sync::broadcast::channel(16);
    let mgr = Arc::new(
        ExternalWorkerManager::new(vec![cron_config_for(&url, 30, 0), plain])
            .with_event_deps(event_tx, Arc::clone(&context_manager)),
    );
    let (_shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel(1);
    mgr.spawn_cron_supervisors("owner-1", shutdown_rx);

    for worker in ["mock", "plain", "nonexistent"] {
        let err = mgr
            .fire_cron_tick_now(worker, "someone-else")
            .await
            .expect_err("non-owner must be rejected");
        assert!(
            matches!(err, OrchestratorError::CronMissionNotOwned { .. }),
            "non-owner probing {worker} must get the uniform not-owned error, got: {err}"
        );
    }
    // The owner retains the precise distinctions.
    let err = mgr
        .fire_cron_tick_now("plain", "owner-1")
        .await
        .expect_err("task-mode worker must reject cron fire");
    assert!(matches!(
        err,
        OrchestratorError::ExternalWorkerNotCron { .. }
    ));
    mgr.fire_cron_tick_now("mock", "owner-1")
        .await
        .expect("owner fire on cron worker succeeds");
}

/// MR !1522 DE-6: the persisted pause flag and tick counter survive a
/// supervisor restart — the mission resumes paused with its tick index
/// intact instead of silently resuming from zero.
#[cfg(feature = "libsql")]
#[tokio::test]
async fn cron_pause_and_tick_counter_survive_restart() {
    use lunarwing::db::Database;
    use lunarwing::db::libsql::LibSqlBackend;

    let temp_dir = tempfile::tempdir().expect("temp dir");
    let backend = LibSqlBackend::new_local(&temp_dir.path().join("cron-restart.db"))
        .await
        .expect("test db");
    backend.run_migrations().await.expect("migrations");
    let db: Arc<dyn Database> = Arc::new(backend);

    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::Capture {
        requests: request_tx,
    })
    .await;

    // First incarnation: per-second schedule; let two ticks fire, then pause.
    let context_manager = Arc::new(ContextManager::new(16));
    let (event_tx, _event_rx) = tokio::sync::broadcast::channel(16);
    let mgr_a = Arc::new(
        ExternalWorkerManager::new(vec![cron_config_with_schedule(&url, "* * * * * *", 30, 0)])
            .with_event_deps(event_tx, Arc::clone(&context_manager))
            .with_store(db.clone()),
    );
    let (shutdown_a, shutdown_rx_a) = tokio::sync::broadcast::channel(1);
    mgr_a.spawn_cron_supervisors("owner-1", shutdown_rx_a);

    timeout(TEST_BOUND * 2, async {
        loop {
            let status = mgr_a
                .cron_mission_status("mock", "owner-1")
                .await
                .expect("status");
            if status.tick_index >= 2 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("two scheduled ticks never fired");
    for _ in 0..2 {
        timeout(TEST_BOUND, request_rx.recv())
            .await
            .unwrap()
            .expect("persisted tick reached worker");
    }
    mgr_a
        .pause_cron_mission("mock", "owner-1", CronCommandOrigin::OwnerTool)
        .await
        .expect("pause");
    // pause_cron_mission returns when the command is queued, not applied:
    // wait until status reflects the pause (which is ordered after the
    // persisted write in the supervisor's command handling) before teardown,
    // so the shutdown broadcast cannot win the select race against it.
    timeout(TEST_BOUND, async {
        loop {
            let status = mgr_a
                .cron_mission_status("mock", "owner-1")
                .await
                .expect("status");
            if status.paused {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("pause was never applied");
    let _ = shutdown_a.send(());
    drop(mgr_a);

    // Second incarnation over the same store: paused and tick_index persist.
    let context_manager_b = Arc::new(ContextManager::new(16));
    let (event_tx_b, _event_rx_b) = tokio::sync::broadcast::channel(16);
    let mgr_b = Arc::new(
        ExternalWorkerManager::new(vec![cron_config_with_schedule(&url, "* * * * * *", 30, 0)])
            .with_event_deps(event_tx_b, Arc::clone(&context_manager_b))
            .with_store(db.clone()),
    );
    let (shutdown_b, shutdown_rx_b) = tokio::sync::broadcast::channel(1);
    mgr_b.spawn_cron_supervisors("owner-1", shutdown_rx_b);

    let status = mgr_b
        .cron_mission_status("mock", "owner-1")
        .await
        .expect("status after restart");
    assert!(status.paused, "persisted pause must survive the restart");
    assert!(
        status.tick_index >= 2,
        "persisted tick counter must survive the restart, got {}",
        status.tick_index
    );
    // DN-4 / DO-4: after restart, resume still uses the last real job row as
    // its audit anchor. Status drains the command before reading the store.
    let job_id: Uuid = serde_json::from_value(
        db.get_setting("owner-1", "cron-mission:test-mission:last-job")
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    mgr_b
        .resume_cron_mission("mock", "owner-1", CronCommandOrigin::OwnerTool)
        .await
        .unwrap();
    assert!(
        !mgr_b
            .cron_mission_status("mock", "owner-1")
            .await
            .unwrap()
            .paused
    );
    let events = db.list_job_events(job_id, None).await.unwrap();
    for kind in ["cron_paused", "cron_resumed"] {
        assert!(
            events
                .iter()
                .any(|event| event.event_type == kind && event.data["origin"] == "owner_tool"),
            "missing persisted {kind} lifecycle event"
        );
    }
    let _ = shutdown_b.send(());
}

/// MR !1531 DR-2 / DN-6: when the job-row write fails, the tick must end
/// without the worker ever being asked to run — a failed persist prevents
/// dispatch. Sabotages only the `agent_jobs` table; the settings store
/// (pause flag / tick counter) keeps working so the supervisor stays alive.
#[cfg(feature = "libsql")]
#[tokio::test]
async fn cron_tick_with_failing_job_persistence_never_dispatches() {
    use lunarwing::db::Database;
    use lunarwing::db::libsql::LibSqlBackend;

    let temp_dir = tempfile::tempdir().expect("temp dir");
    let backend = LibSqlBackend::new_local(&temp_dir.path().join("cron-persist-fail.db"))
        .await
        .expect("test db");
    backend.run_migrations().await.expect("migrations");
    backend
        .shared_db()
        .connect()
        .expect("raw connection")
        .execute("ALTER TABLE agent_jobs RENAME TO agent_jobs_sabotaged", ())
        .await
        .expect("sabotage agent_jobs");
    let db: Arc<dyn Database> = Arc::new(backend);

    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::Capture {
        requests: request_tx,
    })
    .await;

    let context_manager = Arc::new(ContextManager::new(16));
    let (event_tx, _event_rx) = tokio::sync::broadcast::channel(16);
    let mgr = Arc::new(
        ExternalWorkerManager::new(vec![cron_config_with_schedule(&url, "* * * * * *", 30, 0)])
            .with_event_deps(event_tx, Arc::clone(&context_manager))
            .with_store(db),
    );
    let (shutdown, shutdown_rx) = tokio::sync::broadcast::channel(1);
    mgr.spawn_cron_supervisors("owner-1", shutdown_rx);

    // A tick fires (the counter advances) but no request may reach the worker.
    timeout(TEST_BOUND * 2, async {
        loop {
            let status = mgr
                .cron_mission_status("mock", "owner-1")
                .await
                .expect("status");
            if status.tick_index >= 1 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("no tick fired");
    // Give the capacity-retry loop room to misbehave: three more seconds with
    // no request proves dispatch is gated on the persisted job row.
    let leak = timeout(Duration::from_secs(3), request_rx.recv()).await;
    assert!(
        leak.is_err(),
        "worker received a dispatch despite the failed job-row persist: {leak:?}"
    );
    let _ = shutdown.send(());
}

#[cfg(feature = "libsql")]
async fn assert_cron_gateway_auth(client: &reqwest::Client, addr: std::net::SocketAddr) {
    let url = format!("http://{addr}/api/workers/mock/cron");
    assert_eq!(client.get(&url).send().await.unwrap().status(), 401);
    assert_eq!(
        client
            .post(format!("{url}?token=owner-test-token"))
            .json(&serde_json::json!({"action":"resume"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    for worker in ["mock", "nonexistent"] {
        let endpoint = format!("http://{addr}/api/workers/{worker}/cron");
        assert_eq!(
            client
                .get(&endpoint)
                .bearer_auth("other-test-token")
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        assert_eq!(
            client
                .post(&endpoint)
                .bearer_auth("other-test-token")
                .json(&serde_json::json!({"action":"resume"}))
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }
    assert_eq!(
        client
            .post(&url)
            .bearer_auth("owner-test-token")
            .json(&serde_json::json!({"action":"resume", "user_id":"other"}))
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
}

/// DN-3: recovery uses the authenticated gateway independently of either LLM
/// engine. No chat turn or model/tool invocation participates in this route.
#[cfg(feature = "libsql")]
#[tokio::test]
async fn cron_gateway_recovers_persisted_pause_and_rejects_nonowners() {
    use std::collections::HashMap;

    use lunarwing::channels::web::{
        auth::{MultiAuthState, UserIdentity},
        test_helpers::TestGatewayBuilder,
    };
    use lunarwing::db::{Database, libsql::LibSqlBackend};

    let dir = tempfile::tempdir().unwrap();
    let backend = LibSqlBackend::new_local(&dir.path().join("recovery.db"))
        .await
        .unwrap();
    backend.run_migrations().await.unwrap();
    let db: Arc<dyn Database> = Arc::new(backend);
    db.set_setting(
        "owner-1",
        "cron-mission:test-mission:paused",
        &serde_json::json!(true),
    )
    .await
    .unwrap();
    let (events, _) = tokio::sync::broadcast::channel(16);
    let mgr = Arc::new(
        // Unreachable worker; a Feb-29 schedule can never fire mid-test
        // (DR-7 — the daily 10:15 UTC schedule could, in principle).
        ExternalWorkerManager::new(vec![cron_config_with_schedule(
            "ws://127.0.0.1:9",
            "0 0 0 29 2 *",
            30,
            0,
        )])
        .with_event_deps(events, Arc::new(ContextManager::new(16)))
        .with_store(db.clone()),
    );
    let (shutdown, rx) = tokio::sync::broadcast::channel(1);
    mgr.spawn_cron_supervisors("owner-1", rx);
    let auth = MultiAuthState::multi(HashMap::from([
        (
            "owner-test-token".into(),
            UserIdentity {
                user_id: "owner-1".into(),
                workspace_read_scopes: vec![],
            },
        ),
        (
            "other-test-token".into(),
            UserIdentity {
                user_id: "other".into(),
                workspace_read_scopes: vec![],
            },
        ),
    ]));
    let (addr, _state) = TestGatewayBuilder::new()
        .user_id("owner-1")
        .external_worker_manager(mgr.clone())
        .start_multi(auth)
        .await
        .unwrap();
    let client = reqwest::Client::builder()
        .timeout(TEST_BOUND)
        .build()
        .unwrap();
    let url = format!("http://{addr}/api/workers/mock/cron");
    assert_cron_gateway_auth(&client, addr).await;
    assert!(
        mgr.cron_mission_status("mock", "owner-1")
            .await
            .unwrap()
            .paused
    );
    for (action, paused) in [("status", true), ("resume", false), ("pause", true)] {
        let response = if action == "status" {
            client
                .get(&url)
                .bearer_auth("owner-test-token")
                .send()
                .await
                .unwrap()
        } else {
            client
                .post(&url)
                .bearer_auth("owner-test-token")
                .json(&serde_json::json!({"action":action}))
                .send()
                .await
                .unwrap()
        };
        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["action"], action);
        assert_eq!(body["mission"]["paused"], paused);
        assert_eq!(body["mission"]["worker_name"], "mock");
        assert_eq!(
            db.get_setting("owner-1", "cron-mission:test-mission:paused")
                .await
                .unwrap(),
            Some(serde_json::json!(paused))
        );
    }
    let _ = shutdown.send(());
}

/// MR !1531 DR-4: the handler-level owner gate fires before any manager
/// lookup — a non-owner gets 403 even when the gateway has no external worker
/// manager at all (never a 503 that would leak the gateway's shape).
#[cfg(feature = "libsql")]
#[tokio::test]
async fn cron_gateway_owner_gate_precedes_manager_lookup() {
    use std::collections::HashMap;

    use lunarwing::channels::web::{
        auth::{MultiAuthState, UserIdentity},
        test_helpers::TestGatewayBuilder,
    };

    let auth = MultiAuthState::multi(HashMap::from([(
        "other-test-token".to_string(),
        UserIdentity {
            user_id: "other".to_string(),
            workspace_read_scopes: vec![],
        },
    )]));
    let (addr, _state) = TestGatewayBuilder::new()
        .user_id("owner-1")
        .start_multi(auth)
        .await
        .unwrap();
    let client = reqwest::Client::builder()
        .timeout(TEST_BOUND)
        .build()
        .unwrap();
    let url = format!("http://{addr}/api/workers/mock/cron");
    let get_status = client
        .get(&url)
        .bearer_auth("other-test-token")
        .send()
        .await
        .unwrap()
        .status();
    let post_status = client
        .post(&url)
        .bearer_auth("other-test-token")
        .json(&serde_json::json!({"action":"pause"}))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(get_status, 403);
    assert_eq!(post_status, 403);
}

#[tokio::test]
async fn cron_mission_tool_success_routes_pause_status_resume() {
    use lunarwing::context::JobContext;
    use lunarwing::tools::{Tool, builtin::CronMissionTool};
    // Unreachable worker on a Feb-29 schedule: no tick can fire mid-test
    // (DR-7).
    let (mgr, _cm, shutdown) = spawn_cron_manager(cron_config_with_schedule(
        "ws://127.0.0.1:9",
        "0 0 0 29 2 *",
        30,
        0,
    ));
    let tool = CronMissionTool::new(mgr);
    let owner = JobContext {
        user_id: "owner-1".to_string(),
        ..Default::default()
    };
    for (action, paused) in [
        ("pause", true),
        ("status", true),
        ("resume", false),
        ("status", false),
    ] {
        let output = tool
            .execute(
                serde_json::json!({"worker":"mock", "action":action}),
                &owner,
            )
            .await
            .unwrap();
        assert_eq!(output.result["action"], action);
        assert_eq!(output.result["mission"]["paused"], paused);
        assert_eq!(output.result["mission"]["mission_id"], "test-mission");
        assert_eq!(output.result["mission"]["consecutive_abandoned"], 0);
    }
    let _ = shutdown.send(());
}
