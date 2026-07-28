//! Integration tests for the external-worker (EWE) WebSocket path.
//!
//! These tests stand up a mock worker speaking the `lunarwing-agent-v1`
//! protocol (legacy alias `ironclaw-agent-v1`) on an ephemeral loopback port
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
//! - connection failure (unreachable endpoint) -> `ExternalWorkerConnectionFailed`
//! - protocol error (connection closed before `ready`) -> `ExternalWorkerProtocolError`
//! - connection-pool reuse across two sequential successful tasks
//! - subprotocol negotiation with legacy-only (`ironclaw-agent-v1`) and
//!   new-only (`lunarwing-agent-v1`) workers
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
use lunarwing::config::{ExternalWorkerConfig, LoadBalanceStrategy, WorkerEndpoint};
use lunarwing::context::{ContextManager, JobContext, JobState};
use lunarwing::error::OrchestratorError;
use lunarwing::orchestrator::ExternalWorkerManager;
use lunarwing::orchestrator::external_worker::{
    ConversationMessage, ExternalTaskStatus, SUBPROTOCOL, SUBPROTOCOL_LEGACY, TaskContext,
};
use lunarwing::tools::Tool;
use lunarwing::tools::builtin::CreateJobTool;

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
    /// Complete the WebSocket upgrade but close without sending `ready`
    /// (drives a protocol error).
    CloseBeforeReady,
}

/// Which subprotocol names the mock worker accepts, simulating worker
/// generations on either side of the ironclaw -> lunarwing rename.
#[derive(Clone, Copy)]
enum SubprotocolPolicy {
    /// Current worker: accepts both names, prefers `lunarwing-agent-v1`.
    PreferNew,
    /// Pre-rename worker: only knows `ironclaw-agent-v1`.
    LegacyOnly,
    /// Future worker with the legacy alias dropped: only `lunarwing-agent-v1`.
    NewOnly,
}

impl SubprotocolPolicy {
    /// Accepted names in preference order.
    fn accepted(self) -> &'static [&'static str] {
        match self {
            Self::PreferNew => &[SUBPROTOCOL, SUBPROTOCOL_LEGACY],
            Self::LegacyOnly => &[SUBPROTOCOL_LEGACY],
            Self::NewOnly => &[SUBPROTOCOL],
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
    spawn_mock_worker_full(behavior, None, SubprotocolPolicy::PreferNew).await
}

/// Spawn a mock worker that requires a matching Bearer token on the WS upgrade.
async fn spawn_mock_worker_with_auth(
    behavior: MockBehavior,
    expected_token: Option<String>,
) -> (String, Arc<AtomicUsize>) {
    spawn_mock_worker_full(behavior, expected_token, SubprotocolPolicy::PreferNew).await
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
/// `lunarwing-agent-v1` (or its legacy alias, per the policy) for the scripted
/// behavior. When `expected_token` is set, the WS upgrade is rejected unless
/// the client sends a matching `Authorization: Bearer <token>` header.
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
        // what real worker containers must do to stay compatible with both
        // old and new daemons.
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
            MockBehavior::CloseBeforeReady => unreachable!("handled above"),
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

fn config_for(url: &str) -> ExternalWorkerConfig {
    ExternalWorkerConfig {
        name: "mock".to_string(),
        url: url.to_string(),
        auth_token: None,
        timeout_ms: 30_000,
        endpoints: vec![WorkerEndpoint {
            url: url.to_string(),
            auth_token: None,
            weight: None,
        }],
        load_balance: LoadBalanceStrategy::RoundRobin,
        prompt_context: Default::default(),
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
        load_balance: LoadBalanceStrategy::RoundRobin,
        prompt_context: Default::default(),
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
async fn legacy_only_worker_still_negotiates() {
    // Regression test for the ironclaw -> lunarwing subprotocol rename: a
    // worker built before the rename only knows `ironclaw-agent-v1`. Because
    // the daemon offers both names (new first), the worker matches and echoes
    // the legacy alias, and the handshake + task must still succeed.
    let (url, _count) = spawn_mock_worker_with_subprotocol(
        MockBehavior::Success {
            progress: vec![],
            output: "ok".to_string(),
        },
        SubprotocolPolicy::LegacyOnly,
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
    .expect("execute_task errored against legacy-only worker")
    .expect("expected result");

    assert_eq!(res.status, ExternalTaskStatus::Success);
    assert_eq!(res.output, "ok");
}

#[tokio::test]
async fn new_only_worker_negotiates_primary_name() {
    // A worker that only accepts `lunarwing-agent-v1` rejects the upgrade
    // unless the daemon actually offers the new primary name — this fails if
    // the client offer ever regresses to the legacy name alone.
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

    assert!(
        matches!(
            err,
            OrchestratorError::ExternalWorkerConnectionFailed { .. }
        ),
        "got {err:?}"
    );
}
