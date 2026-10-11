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
use lunarwing::config::{ExternalWorkerConfig, WorkerEndpoint};
use lunarwing::context::{ContextManager, JobContext, JobState};
#[cfg(feature = "libsql")]
use lunarwing::db::Database;
#[cfg(feature = "libsql")]
use lunarwing::db::libsql::LibSqlBackend;
use lunarwing::error::OrchestratorError;
use lunarwing::hooks::HookRegistry;
use lunarwing::orchestrator::ExternalWorkerManager;
use lunarwing::orchestrator::external_worker::{
    ConversationMessage, ExternalTaskStatus, SUBPROTOCOL, TaskContext, ToolProxy,
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
    let listener = TcpListener::bind("127.0.0.1:0")
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
struct ProbeEchoTool {
    seen: std::sync::Mutex<Vec<(String, Uuid)>>,
}

#[async_trait::async_trait]
impl Tool for ProbeEchoTool {
    fn name(&self) -> &str {
        "probe_echo"
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
    let probe = Arc::new(ProbeEchoTool {
        seen: std::sync::Mutex::new(Vec::new()),
    });
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools.register(probe.clone()).await;

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxy {
        calls: vec![
            ("probe_echo".to_string(), serde_json::json!({ "k": "v" })),
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
    config.tool_allowlist = vec!["probe_echo".to_string()];
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

#[tokio::test]
async fn tool_call_denied_when_allowlist_empty() {
    // Deny-by-default: no tool_allowlist configured at all -> every tool_call
    // is rejected, even one naming a registered tool.
    let probe = Arc::new(ProbeEchoTool {
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
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxy {
        calls: vec![("ghost_tool".to_string(), serde_json::json!({}))],
        replies: reply_tx,
    })
    .await;

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["ghost_tool".to_string()];
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
    let probe = Arc::new(ProbeEchoTool {
        seen: std::sync::Mutex::new(Vec::new()),
    });
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools.register(probe.clone()).await;

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxyRaw {
        frames: vec![
            // Missing task_id entirely.
            (
                "tool_call".to_string(),
                serde_json::json!({"call_id": "c-missing", "name": "probe_echo"}),
            ),
            // Mismatched task_id.
            (
                "tool_call".to_string(),
                serde_json::json!({
                    "call_id": "c-wrong",
                    "name": "probe_echo",
                    "task_id": "00000000-0000-0000-0000-000000000000",
                }),
            ),
            // Correct task_id (template substituted by the mock).
            (
                "tool_call".to_string(),
                serde_json::json!({
                    "call_id": "c-right",
                    "name": "probe_echo",
                    "task_id": "@task_id@",
                }),
            ),
        ],
        replies: reply_tx,
    })
    .await;

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["probe_echo".to_string()];
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
    // autonomous opt-outs are not advertised.
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools
        .register(Arc::new(GateProbeTool {
            tool_name: "probe_echo".to_string(),
            output: "ok".to_string(),
            autonomous: true,
            calls: AtomicUsize::new(0),
        }))
        .await;
    tools
        .register(Arc::new(GateProbeTool {
            tool_name: "interactive_only_probe".to_string(),
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
        "probe_echo".to_string(),
        "interactive_only_probe".to_string(),
        "ghost_tool".to_string(),
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
        vec!["probe_echo"],
        "catalog must contain only allowlisted, registered, autonomous-capable tools"
    );
}

#[tokio::test]
async fn tool_call_autonomous_opt_out_is_denied() {
    // MR !1506 review (CQ-1): a tool that opts out of autonomous execution
    // must be refused even when it is named in the worker allowlist.
    let probe = Arc::new(GateProbeTool {
        tool_name: "interactive_only_probe".to_string(),
        output: "ok".to_string(),
        autonomous: false,
        calls: AtomicUsize::new(0),
    });
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools.register(probe.clone()).await;

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxy {
        calls: vec![("interactive_only_probe".to_string(), serde_json::json!({}))],
        replies: reply_tx,
    })
    .await;

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["interactive_only_probe".to_string()];
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
        "tool_permissions.probe_echo",
        &serde_json::json!("disabled"),
    )
    .await
    .expect("seed permission");

    let probe = Arc::new(ProbeEchoTool {
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

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["probe_echo".to_string()];
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

    let probe = Arc::new(ProbeEchoTool {
        seen: std::sync::Mutex::new(Vec::new()),
    });
    let tools = Arc::new(lunarwing::tools::ToolRegistry::new());
    tools.register(probe.clone()).await;

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxy {
        calls: vec![
            ("probe_echo".to_string(), serde_json::json!({ "k": "v" })),
            ("memory_write".to_string(), serde_json::json!({})),
        ],
        replies: reply_tx,
    })
    .await;

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["probe_echo".to_string()];
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
    assert_eq!(tool_events[0].data["tool_name"], "probe_echo");
    assert!(
        tool_events[0].data["input"]
            .as_str()
            .expect("input preview")
            .contains("\"k\""),
        "input preview carries the call arguments: {}",
        tool_events[0].data["input"]
    );

    assert_eq!(tool_events[1].event_type, "tool_result");
    assert_eq!(tool_events[1].data["tool_name"], "probe_echo");
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
            tool_name: "probe_echo".to_string(),
            output: "aws key: AKIAIOSFODNN7EXAMPLE".to_string(),
            autonomous: true,
            calls: AtomicUsize::new(0),
        }))
        .await;

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (url, _count) = spawn_mock_worker_async(MockBehavior::ToolProxy {
        calls: vec![("probe_echo".to_string(), serde_json::json!({}))],
        replies: reply_tx,
    })
    .await;

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["probe_echo".to_string()];
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
    let probe = Arc::new(ProbeEchoTool {
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
    config.tool_allowlist = vec!["probe_echo".to_string()];
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
        ],
        replies: reply_tx,
    })
    .await;

    let mut config = config_for(&url);
    config.tool_allowlist = vec!["memory_write".to_string(), "memory_read".to_string()];
    let mgr = ExternalWorkerManager::new(vec![config]).with_tool_proxy(test_proxy(tools));

    let replies = run_proxy_task(&mgr, Uuid::new_v4(), &mut reply_rx, 3).await;

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
}
