//! WebSocket client for external workers speaking the `lunarwing-agent-v1`
//! protocol.
//!
//! External workers are persistent containers (Pebble, OpenCode, Kimi Code, etc.) that the
//! orchestrator connects to on demand rather than creating per-job.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use chrono::Utc;
use futures::{SinkExt, StreamExt};
pub use lunarwing_common::PromptContextMessage as ConversationMessage;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock, broadcast, watch};
use uuid::Uuid;

use crate::channels::web::types::SseEvent;
use crate::config::{ExternalWorkerConfig, LoadBalanceStrategy};
use crate::context::{ContextManager, JobContext, JobState};
use crate::db::Database;
use crate::error::OrchestratorError;
use crate::hooks::{HookError, HookEvent, HookOutcome, HookRegistry};
use crate::safety::SafetyLayer;
use crate::tools::permissions::{
    PermissionState, gitlab_permission_alias_conflict, permission_setting_keys,
};
use crate::tools::rate_limiter::RateLimitResult;
use crate::tools::{
    ApprovalContext, Tool, ToolRegistry, is_autonomous_tool_denylisted, prepare_tool_params,
    redact_params,
};
use crate::workspace::{is_engine_internal_path, normalize_path};

// ── Protocol types ──────────────────────────────────────────────────

/// Primary WebSocket subprotocol spoken by external workers.
pub const SUBPROTOCOL: &str = "lunarwing-agent-v1";

#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    id: String,
    #[serde(rename = "type")]
    msg_type: String,
    timestamp: String,
    payload: serde_json::Value,
}

impl Envelope {
    fn new(msg_type: &str, payload: serde_json::Value) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            msg_type: msg_type.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            payload,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ReadyPayload {
    worker_id: String,
    #[allow(dead_code)]
    version: String,
    #[allow(dead_code)]
    mode: String,
}

#[derive(Debug, Deserialize)]
struct TaskProgressPayload {
    #[allow(dead_code)]
    task_id: String,
    delta: String,
    #[allow(dead_code)]
    done: bool,
}

/// Context passed to external workers with task requests.
///
/// All fields are optional with serde defaults for backward compatibility —
/// older workers that don't understand these fields will still work.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TaskContext {
    /// Workspace/project directory path.
    // Omit when None: TS bridges treat an explicit JSON `null` differently
    // from an absent field. Kimi's `lunarwing_runtime.ts` rejects
    // `project_dir: null` ("context.project_dir must be a string") while
    // accepting an absent key, so serializing None as null breaks every kimi
    // job dispatched without an explicit project dir. Pebble (Rust) and
    // OpenCode (TS) treat both shapes as None/undefined.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<String>,
    /// Recent conversation messages for context.
    pub conversation_history: Vec<ConversationMessage>,
    /// Environment variables to inject into the worker process.
    pub environment: HashMap<String, String>,
    /// User ID who initiated the task.
    pub user_id: String,
    /// Arbitrary metadata key-value pairs.
    pub metadata: HashMap<String, String>,
}

pub fn build_task_context(
    user_id: &str,
    project_dir: Option<&str>,
    environment: HashMap<String, String>,
    conversation_history: Vec<ConversationMessage>,
    metadata: HashMap<String, String>,
) -> TaskContext {
    TaskContext {
        user_id: user_id.to_string(),
        project_dir: project_dir.map(String::from),
        environment,
        conversation_history,
        metadata,
    }
}

/// Whether a worker endpoint URL is a cleartext `ws://` reach to a
/// non-loopback host — the condition that strips the tool allowlist (CQ-7).
///
/// Classification uses `http::Uri::host()` — the host the connection would
/// actually go to — and FAILS CLOSED (MR !1521 DD-9): a cleartext URL that
/// cannot be confidently classified (unparseable, missing authority, or
/// carrying userinfo) counts as non-loopback. Userinfo must never be
/// classified as loopback: `ws://localhost:9000@agent-hub.example/kimi`
/// connects to `agent-hub.example`, and a naive split-based host extraction
/// used to call that "localhost" and keep the allowlist.
fn is_cleartext_non_loopback(url: &str) -> bool {
    if !url.starts_with("ws://") {
        return false;
    }
    let Ok(uri) = url.parse::<http::Uri>() else {
        return true;
    };
    let Some(authority) = uri.authority() else {
        return true;
    };
    if authority.as_str().contains('@') {
        return true;
    }
    uri.host().is_none_or(|host| !is_loopback(host))
}

/// Host display for the cleartext-endpoint warning, taken from the parsed
/// URI (never from worker-config raw text semantics).
fn endpoint_host_display(url: &str) -> String {
    url.parse::<http::Uri>()
        .ok()
        .and_then(|uri| uri.host().map(str::to_string))
        .unwrap_or_else(|| "[unparseable]".to_string())
}

fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host == "127.0.0.1"
        || host == "::1"
        || host == "[::1]"
        || host == "0.0.0.0"
}

/// Operator-facing explanation appended to a worker handshake rejection when
/// the failure signature matches a subprotocol mismatch (HTTP 400/426 with no
/// echoed `Sec-WebSocket-Protocol`), the visible symptom of the daemon
/// running ahead of a still-legacy-only worker during a rolling upgrade.
const LEGACY_SUBPROTOCOL_HINT: &str = "the worker rejected the WebSocket \
     subprotocol offer; since 2.1.1 the daemon offers only `lunarwing-agent-v1` \
     (the `ironclaw-agent-v1` alias was removed), so this is usually a worker \
     container from an older release — recreate/upgrade worker containers in \
     the same maintenance window as the daemon";

/// Render a worker connect error, enriching the subprotocol-mismatch failure
/// signature (HTTP 400/426 rejection without an echoed subprotocol header)
/// with the rolling-upgrade operator hint. Any other error keeps its plain
/// `Display` form so unrelated failures — e.g. an auth rejection (401) with no
/// echoed protocol — stay unambiguous.
fn describe_connect_failure(error: &tokio_tungstenite::tungstenite::Error) -> String {
    use tokio_tungstenite::tungstenite::Error;
    use tokio_tungstenite::tungstenite::http::StatusCode;

    if let Error::Http(response) = error
        && matches!(
            response.status(),
            StatusCode::BAD_REQUEST | StatusCode::UPGRADE_REQUIRED
        )
    {
        let echoed_subprotocol = response.headers().get("sec-websocket-protocol").is_some();
        if !echoed_subprotocol {
            return format!("{error} ({LEGACY_SUBPROTOCOL_HINT})");
        }
    }
    error.to_string()
}

#[derive(Debug, Deserialize)]
struct TaskResultPayload {
    #[allow(dead_code)]
    task_id: String,
    status: String,
    output: String,
    error: Option<String>,
    duration_ms: u64,
}

/// Zero-allocation probe of an inbound frame's `type` field, borrowed from
/// the raw text (MR !1521 DD-5): lets the read loop reject an oversized
/// `tool_call` frame BEFORE its payload is parsed into a heap `Value`.
#[derive(Deserialize)]
struct TypeProbe<'a> {
    #[serde(rename = "type")]
    msg_type: &'a str,
}

/// Optional `tool_list` payload (MR !1521 DD-10): when the worker sends a
/// `task_id` it must match the connection's job, same binding rule as
/// `tool_call`. Absent payload/absent field stays accepted for older
/// workers; a mismatch is an error.
#[derive(Debug, Deserialize)]
struct ToolListPayload {
    #[serde(default)]
    task_id: Option<String>,
}

// ── Proxied tool path (WI#324) ──────────────────────────────────────

/// Tenant tool surface exposed to external workers over the existing WS
/// control channel (WI#324 "capability proxy"). Holds the in-process tool
/// registry, safety layer, and hook registry so a worker's `tool_call` frame
/// is dispatched with the same autonomous-execution hard gates as the other
/// background paths (jobs/routines) — the worker never receives credentials,
/// only sanitized tool results.
#[derive(Clone)]
pub struct ToolProxy {
    tools: Arc<ToolRegistry>,
    safety: Arc<SafetyLayer>,
    hooks: Arc<HookRegistry>,
}

impl ToolProxy {
    pub fn new(
        tools: Arc<ToolRegistry>,
        safety: Arc<SafetyLayer>,
        hooks: Arc<HookRegistry>,
    ) -> Self {
        Self {
            tools,
            safety,
            hooks,
        }
    }
}

/// Worker → daemon `tool_call` frame payload (WI#324).
#[derive(Debug, Deserialize)]
struct ToolCallPayload {
    /// Worker-assigned correlation id, echoed verbatim in the reply.
    call_id: String,
    /// Registered tool name, e.g. "memory_write".
    name: String,
    /// Tool arguments (validated by the safety layer before execution).
    #[serde(default)]
    arguments: serde_json::Value,
    /// Task the call belongs to. Required and must equal the connection's
    /// job id (MR !1506 review CQ-7): pooled sockets are reused across jobs,
    /// so every call must bind itself to the job it claims to serve.
    #[serde(default)]
    task_id: Option<String>,
}

/// Daemon → worker `tool_catalog` reply payload for `tool_list` requests:
/// the definitions of exactly the tools this worker's allowlist permits.
#[derive(Debug, Serialize)]
struct ToolCatalogEntry {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

/// Per-task binding for the proxied tool path (WI#324): the shared proxy
/// (registry/safety/hooks) plus the worker's sanitized allowlist. Bundled so
/// `run_external_task` does not grow a parameter per new proxy concern.
#[derive(Clone)]
struct ToolProxyBinding {
    proxy: Option<ToolProxy>,
    allowlist: Arc<[String]>,
}

// ── External job handle ─────────────────────────────────────────────

/// Handle to a running external worker task.
pub struct ExternalJobHandle {
    pub job_id: Uuid,
    pub worker_name: String,
    cancel_tx: watch::Sender<bool>,
    started: bool,
}

impl ExternalJobHandle {
    pub fn cancel(&self) {
        self.cancel_tx.send_replace(true);
    }
}

// ── Manager ─────────────────────────────────────────────────────────

/// Manages connections to external worker endpoints.
pub struct ExternalWorkerManager {
    workers: HashMap<String, ExternalWorkerConfig>,
    load_balancers: HashMap<String, LoadBalancer>,
    pool: Arc<WorkerConnectionPool>,
    job_event_tx: Option<broadcast::Sender<(Uuid, String, SseEvent)>>,
    context_manager: Option<Arc<ContextManager>>,
    store: Option<Arc<dyn Database>>,
    /// Proxied tool path (WI#324). `None` = feature not wired; `tool_call`
    /// frames get an explicit error reply rather than being dropped.
    tool_proxy: Option<ToolProxy>,
    active_handles: Arc<RwLock<HashMap<Uuid, ExternalJobHandle>>>,
}

impl ExternalWorkerManager {
    pub fn new(configs: Vec<ExternalWorkerConfig>) -> Self {
        let mut load_balancers = HashMap::new();
        let mut workers = HashMap::new();
        for mut config in configs {
            let cleartext_remote = config
                .endpoints()
                .iter()
                .any(|endpoint| is_cleartext_non_loopback(&endpoint.url));
            if cleartext_remote {
                for endpoint in config.endpoints() {
                    if is_cleartext_non_loopback(&endpoint.url) {
                        tracing::warn!(
                            "External worker '{}' endpoint uses cleartext ws:// for non-loopback host '{}'. \
                             Credentials and task data will be sent unencrypted. Use wss:// for remote workers.",
                            config.name,
                            endpoint_host_display(&endpoint.url)
                        );
                    }
                }
            }
            config.tool_allowlist =
                sanitize_tool_allowlist(&config.name, &config.tool_allowlist, cleartext_remote);
            let endpoints = config.endpoints();
            load_balancers.insert(
                config.name.clone(),
                LoadBalancer::new(endpoints, config.load_balance.clone()),
            );
            workers.insert(config.name.clone(), config);
        }

        if !workers.is_empty() {
            tracing::info!(
                "External workers configured: {}",
                workers.keys().cloned().collect::<Vec<_>>().join(", ")
            );
        }

        Self {
            workers,
            load_balancers,
            pool: Arc::new(WorkerConnectionPool::new(2, Duration::from_secs(300))),
            job_event_tx: None,
            context_manager: None,
            store: None,
            tool_proxy: None,
            active_handles: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn with_event_deps(
        mut self,
        event_tx: broadcast::Sender<(Uuid, String, SseEvent)>,
        context_manager: Arc<ContextManager>,
    ) -> Self {
        self.job_event_tx = Some(event_tx);
        self.context_manager = Some(context_manager);
        self
    }

    pub fn with_store(mut self, store: Arc<dyn Database>) -> Self {
        self.store = Some(store);
        self
    }

    /// Wire the proxied tool path (WI#324): workers may then invoke
    /// allowlisted in-process tools via `tool_call` frames on the task WS.
    pub fn with_tool_proxy(mut self, proxy: ToolProxy) -> Self {
        self.tool_proxy = Some(proxy);
        self
    }

    pub fn get_worker(&self, name: &str) -> Option<&ExternalWorkerConfig> {
        self.workers.get(name)
    }

    pub fn worker_names(&self) -> Vec<&str> {
        self.workers.keys().map(|s| s.as_str()).collect()
    }

    pub fn max_timeout_ms(&self) -> Option<u64> {
        self.workers.values().map(|worker| worker.timeout_ms).max()
    }

    pub fn is_empty(&self) -> bool {
        self.workers.is_empty()
    }

    /// Reserve a job ID before exposing its context or DB record to cancellation callers.
    pub async fn reserve_task(
        &self,
        job_id: Uuid,
        worker_name: &str,
    ) -> Result<(), OrchestratorError> {
        if !self.workers.contains_key(worker_name) {
            return Err(OrchestratorError::ExternalWorkerNotFound {
                worker_name: worker_name.to_string(),
            });
        }

        let mut handles = self.active_handles.write().await;
        if handles.contains_key(&job_id) {
            return Err(OrchestratorError::ExternalWorkerTaskAlreadyActive {
                worker_name: worker_name.to_string(),
                job_id,
            });
        }
        let (cancel_tx, _cancel_rx) = watch::channel(false);
        handles.insert(
            job_id,
            ExternalJobHandle {
                job_id,
                worker_name: worker_name.to_string(),
                cancel_tx,
                started: false,
            },
        );
        Ok(())
    }

    /// Release a reservation when setup fails before task execution starts.
    pub async fn release_task_reservation(&self, job_id: Uuid) {
        let mut handles = self.active_handles.write().await;
        if handles.get(&job_id).is_some_and(|handle| !handle.started) {
            handles.remove(&job_id);
        }
    }

    /// Execute a task on a named external worker.
    ///
    /// Connects via WebSocket, sends the task, streams progress as job events,
    /// and returns the final result. If `wait` is true, blocks until completion.
    /// If false, spawns a background task and returns immediately.
    pub async fn execute_task(
        &self,
        job_id: Uuid,
        worker_name: &str,
        task: &str,
        timeout_ms: Option<u64>,
        wait: bool,
        context: TaskContext,
    ) -> Result<Option<ExternalTaskResult>, OrchestratorError> {
        self.pool.evict_stale().await;

        let config = self.workers.get(worker_name).ok_or_else(|| {
            OrchestratorError::ExternalWorkerNotFound {
                worker_name: worker_name.to_string(),
            }
        })?;

        let timeout = timeout_ms.unwrap_or(config.timeout_ms);

        // Use load balancer to select an endpoint and acquire an active-connection
        // lease. The lease is held for the task's lifetime (moved into the spawned
        // task on the fire-and-forget path) so LeastConnections reflects real load.
        // With no LB configured (single legacy endpoint) there is nothing to
        // balance and no active count to track.
        let lease = self.load_balancers.get(worker_name).map(|lb| lb.acquire());
        let url = lease
            .as_ref()
            .map(|l| l.url.clone())
            .unwrap_or_else(|| config.url.clone());
        let auth_token = lease
            .as_ref()
            .map(|l| l.auth_token.clone())
            .unwrap_or_else(|| config.auth_token.clone());
        let pool_key = format!("{worker_name}:{url}");
        let worker_name_owned = worker_name.to_string();
        let task_owned = task.to_string();
        let event_tx = self.job_event_tx.clone();
        let context_manager = self.context_manager.clone();
        let store = self.store.clone();
        let tool_binding = ToolProxyBinding {
            proxy: self.tool_proxy.clone(),
            allowlist: Arc::from(config.tool_allowlist.as_slice()),
        };
        let active_handles = Arc::clone(&self.active_handles);
        let pool = Arc::clone(&self.pool);

        let cancel_rx = {
            let mut handles = active_handles.write().await;
            if let Some(handle) = handles.get_mut(&job_id) {
                if handle.started {
                    return Err(OrchestratorError::ExternalWorkerTaskAlreadyActive {
                        worker_name: worker_name_owned,
                        job_id,
                    });
                }
                handle.started = true;
                handle.cancel_tx.subscribe()
            } else {
                let (cancel_tx, cancel_rx) = watch::channel(false);
                handles.insert(
                    job_id,
                    ExternalJobHandle {
                        job_id,
                        worker_name: worker_name_owned.clone(),
                        cancel_tx,
                        started: true,
                    },
                );
                cancel_rx
            }
        };

        if *cancel_rx.borrow() {
            let result = ExternalTaskResult {
                status: ExternalTaskStatus::Cancelled,
                output: String::new(),
                error: Some("Cancelled before external worker dispatch".to_string()),
                duration_ms: 0,
            };
            record_external_task_cancelled(
                job_id,
                &context.user_id,
                event_tx.as_ref(),
                context_manager.as_ref(),
                store.as_ref(),
                result.error.as_deref().unwrap_or("Cancelled by user"),
            )
            .await;
            active_handles.write().await.remove(&job_id);
            return Ok(wait.then_some(result));
        }

        if wait {
            let lb = self.load_balancers.get(worker_name);
            let max_attempts = if lb.is_some_and(|l| l.endpoint_count() > 1) {
                lb.unwrap().endpoint_count()
            } else {
                1
            };

            let mut failed_urls: Vec<String> = Vec::new();

            for attempt in 0..max_attempts {
                let attempt_lease = lb.map(|lb| {
                    if failed_urls.is_empty() {
                        lb.acquire()
                    } else {
                        lb.acquire_excluding(&failed_urls)
                    }
                });
                let attempt_url = attempt_lease
                    .as_ref()
                    .map(|l| l.url.clone())
                    .unwrap_or_else(|| config.url.clone());
                let attempt_auth_token = attempt_lease
                    .as_ref()
                    .map(|l| l.auth_token.clone())
                    .unwrap_or_else(|| config.auth_token.clone());
                let attempt_pool_key = format!("{worker_name_owned}:{attempt_url}");

                let result = run_external_task(
                    job_id,
                    &attempt_url,
                    attempt_auth_token.as_ref().map(|s| s.expose_secret()),
                    &task_owned,
                    timeout,
                    &worker_name_owned,
                    event_tx.as_ref(),
                    context_manager.as_ref(),
                    store.as_ref(),
                    cancel_rx.clone(),
                    context.clone(),
                    tool_binding.clone(),
                    &pool,
                    &attempt_pool_key,
                )
                .await;

                let is_connection_failure = matches!(
                    &result,
                    Err(OrchestratorError::ExternalWorkerConnectionFailed { .. })
                );

                if is_connection_failure && attempt + 1 < max_attempts {
                    failed_urls.push(attempt_url.clone());
                    if let Err(ref e) = result {
                        tracing::warn!(
                            "External worker '{}' endpoint {} unreachable ({e}), retrying {}/{}",
                            worker_name_owned,
                            attempt_url,
                            attempt + 2,
                            max_attempts
                        );
                    }
                    continue;
                }

                if let Err(error) = &result {
                    record_external_task_error(
                        job_id,
                        error,
                        event_tx.as_ref(),
                        context_manager.as_ref(),
                        store.as_ref(),
                    )
                    .await;
                }

                active_handles.write().await.remove(&job_id);
                return result.map(Some);
            }

            active_handles.write().await.remove(&job_id);
            unreachable!("loop always returns when max_attempts > 0");
        } else {
            tokio::spawn(async move {
                // Hold the endpoint lease for the task's lifetime so the active
                // count (LeastConnections accounting) stays accurate until the
                // task finishes; it decrements when _lease drops at block end.
                let _lease = lease;
                let result = run_external_task(
                    job_id,
                    &url,
                    auth_token.as_ref().map(|s| s.expose_secret()),
                    &task_owned,
                    timeout,
                    &worker_name_owned,
                    event_tx.as_ref(),
                    context_manager.as_ref(),
                    store.as_ref(),
                    cancel_rx,
                    context,
                    tool_binding,
                    &pool,
                    &pool_key,
                )
                .await;

                active_handles.write().await.remove(&job_id);

                if let Err(error) = &result {
                    record_external_task_error(
                        job_id,
                        error,
                        event_tx.as_ref(),
                        context_manager.as_ref(),
                        store.as_ref(),
                    )
                    .await;
                }

                match &result {
                    Ok(r) => {
                        tracing::info!(
                            "External worker '{}' job {} completed: status={}",
                            worker_name_owned,
                            job_id,
                            r.status
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            "External worker '{}' job {} failed: {}",
                            worker_name_owned,
                            job_id,
                            e
                        );
                    }
                }
            });

            Ok(None)
        }
    }

    /// Cancel an active external worker task.
    pub async fn cancel_task(&self, job_id: Uuid) -> bool {
        if let Some(handle) = self.active_handles.read().await.get(&job_id) {
            handle.cancel();
            return true;
        }
        false
    }

    /// Spawn a background task that periodically evicts stale idle connections.
    /// Cancelled when the shutdown receiver fires.
    pub fn spawn_eviction_task(&self, mut shutdown_rx: broadcast::Receiver<()>) {
        let pool = Arc::clone(&self.pool);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            interval.tick().await; // skip immediate first tick
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        pool.evict_stale().await;
                    }
                    _ = shutdown_rx.recv() => {
                        tracing::debug!("pool eviction task shutting down");
                        break;
                    }
                }
            }
        });
    }

    /// Gracefully drain the pool (send WS Close frames, bounded per-connection).
    ///
    /// WI#149: in-flight tasks get a bounded cancel handoff first. Every active
    /// handle is signalled, then we wait (bounded) for the task runners to
    /// finish — each runner now returns only after the worker's terminal frame,
    /// socket close, or the ack deadline — before idle pool connections are
    /// closed. Without this, daemon shutdown could tear sockets down while a
    /// mid-task cancel was still racing the worker's ack.
    pub async fn drain_pool(&self) {
        self.cancel_active_tasks_for_shutdown().await;
        self.pool.drain().await;
    }

    /// Signal cancel on every active handle and wait bounded for the task
    /// runners to complete their cancel handoff. Runners remove their handle
    /// after `run_external_task` returns, so an empty map means every in-flight
    /// task finished its handoff (ack observed, socket closed, or deadline).
    async fn cancel_active_tasks_for_shutdown(&self) {
        {
            let handles = self.active_handles.read().await;
            if handles.is_empty() {
                return;
            }
            tracing::info!(
                count = handles.len(),
                "Shutdown: signalling cancel to in-flight external worker tasks"
            );
            for handle in handles.values() {
                handle.cancel();
            }
        }

        // One full handoff window (bounded cancel send + ack wait) plus slack
        // for runner bookkeeping.
        let handoff = CANCEL_SEND_TIMEOUT + CANCEL_ACK_TIMEOUT + Duration::from_secs(1);
        let drained = tokio::time::timeout(handoff, async {
            while !self.active_handles.read().await.is_empty() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .is_ok();

        if drained {
            tracing::debug!("Shutdown: external worker cancel handoff complete");
        } else {
            tracing::warn!(
                remaining = self.active_handles.read().await.len(),
                "Shutdown: external worker cancel handoff window expired with task(s) still in flight; proceeding"
            );
        }
    }
}

/// Status of an external worker task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ExternalTaskStatus {
    Success,
    Failed,
    Cancelled,
    #[serde(rename = "timed_out")]
    TimedOut,
    Partial(String),
}

impl std::fmt::Display for ExternalTaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Success => write!(f, "success"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::TimedOut => write!(f, "timed_out"),
            Self::Partial(msg) => write!(f, "partial: {}", msg),
        }
    }
}

/// Result of an external worker task.
#[derive(Debug, Clone)]
pub struct ExternalTaskResult {
    pub status: ExternalTaskStatus,
    pub output: String,
    pub error: Option<String>,
    pub duration_ms: u64,
}

// ── Load balancer ──────────────────────────────────────────────────

use crate::config::WorkerEndpoint;

pub struct LoadBalancer {
    inner: Arc<LbInner>,
    strategy: LoadBalanceStrategy,
    current_index: AtomicUsize,
}

struct LbInner {
    endpoints: Vec<WorkerEndpoint>,
    // In-flight task count per endpoint, parallel to `endpoints`. Bumped by
    // `acquire()` and decremented when the returned `EndpointLease` drops, so
    // LeastConnections can prefer the least-loaded endpoint.
    active: Vec<AtomicUsize>,
}

/// A selected endpoint plus its active-connection accounting. Derefs to the
/// chosen [`WorkerEndpoint`]; dropping the lease decrements that endpoint's
/// in-flight count, so callers must hold it for the task's lifetime.
pub struct EndpointLease {
    index: usize,
    inner: Arc<LbInner>,
}

impl std::ops::Deref for EndpointLease {
    type Target = WorkerEndpoint;
    fn deref(&self) -> &WorkerEndpoint {
        &self.inner.endpoints[self.index]
    }
}

impl Drop for EndpointLease {
    fn drop(&mut self) {
        self.inner.active[self.index].fetch_sub(1, Ordering::Relaxed);
    }
}

impl LoadBalancer {
    pub fn new(endpoints: Vec<WorkerEndpoint>, strategy: LoadBalanceStrategy) -> Self {
        assert!(
            !endpoints.is_empty(),
            "LoadBalancer requires at least one endpoint"
        );
        let active = (0..endpoints.len())
            .map(|_| AtomicUsize::new(0))
            .collect::<Vec<_>>();
        Self {
            inner: Arc::new(LbInner { endpoints, active }),
            strategy,
            current_index: AtomicUsize::new(0),
        }
    }

    /// Select the next endpoint and acquire an active-connection lease.
    ///
    /// - `RoundRobin`: cycles endpoints lock-free.
    /// - `LeastConnections`: picks the endpoint with the fewest in-flight tasks
    ///   (ties resolve to the lowest index).
    ///
    /// The returned lease must be held for the task's lifetime so the count
    /// reflects actual load; it decrements on drop. For failover retries that
    /// should skip a just-failed endpoint, use [`acquire_excluding`](Self::acquire_excluding).
    pub fn acquire(&self) -> EndpointLease {
        let len = self.inner.endpoints.len();
        let idx = match self.strategy {
            LoadBalanceStrategy::RoundRobin => {
                self.current_index.fetch_add(1, Ordering::Relaxed) % len
            }
            LoadBalanceStrategy::LeastConnections => {
                let mut best = 0usize;
                let mut best_count = usize::MAX;
                for (i, a) in self.inner.active.iter().enumerate() {
                    let c = a.load(Ordering::Relaxed);
                    if c < best_count {
                        best_count = c;
                        best = i;
                    }
                }
                best
            }
        };
        self.inner.active[idx].fetch_add(1, Ordering::Relaxed);
        EndpointLease {
            index: idx,
            inner: Arc::clone(&self.inner),
        }
    }

    /// Select an endpoint, skipping any whose URL is in `excluded_urls`. Used by
    /// the failover retry loop to avoid re-trying an endpoint that just failed
    /// (M9 circuit-breaker). If all endpoints are excluded, falls back to the
    /// normal selection so we never deadlock.
    pub fn acquire_excluding(&self, excluded_urls: &[String]) -> EndpointLease {
        let len = self.inner.endpoints.len();
        let idx = match self.strategy {
            LoadBalanceStrategy::RoundRobin => {
                self.current_index.fetch_add(1, Ordering::Relaxed) % len
            }
            LoadBalanceStrategy::LeastConnections => {
                let mut best = 0usize;
                let mut best_count = usize::MAX;
                let mut found = false;
                for (i, a) in self.inner.active.iter().enumerate() {
                    if excluded_urls
                        .iter()
                        .any(|u| u == &self.inner.endpoints[i].url)
                    {
                        continue;
                    }
                    let c = a.load(Ordering::Relaxed);
                    if c < best_count {
                        best_count = c;
                        best = i;
                        found = true;
                    }
                }
                if found {
                    best
                } else {
                    self.current_index.fetch_add(1, Ordering::Relaxed) % len
                }
            }
        };
        self.inner.active[idx].fetch_add(1, Ordering::Relaxed);
        EndpointLease {
            index: idx,
            inner: Arc::clone(&self.inner),
        }
    }

    /// In-flight task count for the endpoint currently at `index` (debug/observe).
    pub fn active_for(&self, index: usize) -> usize {
        self.inner
            .active
            .get(index)
            .map(|a| a.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    pub fn endpoint_count(&self) -> usize {
        self.inner.endpoints.len()
    }

    pub fn strategy(&self) -> LoadBalanceStrategy {
        self.strategy.clone()
    }
}

// ── Connection pool ────────────────────────────────────────────────

use std::time::Instant;
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsWrite = futures::stream::SplitSink<WsStream, tokio_tungstenite::tungstenite::Message>;
type WsRead = futures::stream::SplitStream<WsStream>;

const CANCEL_SEND_TIMEOUT: Duration = Duration::from_secs(2);

/// How long the daemon waits for a worker's terminal frame (`task_result` for
/// the cancelled job) or socket close after sending a `cancel` envelope before
/// tearing the connection down (WI#149). Workers that received the WI#101
/// read-loop fix ack promptly; pre-fix workers just hit this deadline.
const CANCEL_ACK_TIMEOUT: Duration = Duration::from_secs(3);

/// Scheduling margin subtracted when capping the user-cancel ack wait to the
/// remaining task budget, so the inner deadline reliably fires before the
/// outer task timeout drops the future (a cancel racing the last seconds of
/// the task window must still resolve as `Cancelled`, not `TimedOut`).
const CANCEL_ACK_TASK_MARGIN: Duration = Duration::from_millis(50);

pub struct PooledConnection {
    pub stream: WsStream,
    pub worker_id: String,
    last_used: Instant,
}

pub struct WorkerConnectionPool {
    connections: Mutex<HashMap<String, Vec<PooledConnection>>>,
    max_idle_per_endpoint: usize,
    idle_timeout: Duration,
}

const POOLED_CONNECTION_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

impl WorkerConnectionPool {
    pub fn new(max_idle_per_endpoint: usize, idle_timeout: Duration) -> Self {
        Self {
            connections: Mutex::new(HashMap::new()),
            max_idle_per_endpoint,
            idle_timeout,
        }
    }

    pub async fn try_acquire(&self, key: &str) -> Option<PooledConnection> {
        let mut conns = self.connections.lock().await;
        let pool = conns.get_mut(key)?;
        pool.pop()
    }

    pub async fn release(&self, key: String, mut conn: PooledConnection) {
        conn.last_used = Instant::now();
        let mut conns = self.connections.lock().await;
        let pool = conns.entry(key).or_default();
        if pool.len() < self.max_idle_per_endpoint {
            pool.push(conn);
        }
    }

    pub async fn evict_stale(&self) {
        let mut conns = self.connections.lock().await;
        let cutoff = self.idle_timeout;
        conns.retain(|_, pool| {
            pool.retain(|c| c.last_used.elapsed() < cutoff);
            !pool.is_empty()
        });
    }

    pub async fn drain(&self) {
        let mut conns = self.connections.lock().await;
        for pool in conns.values_mut() {
            for conn in pool.drain(..) {
                let _ = tokio::time::timeout(Duration::from_secs(2), async {
                    let (mut sink, _read) = conn.stream.split();
                    let _ = sink
                        .send(tokio_tungstenite::tungstenite::Message::Close(None))
                        .await;
                })
                .await;
            }
        }
        conns.clear();
    }

    pub async fn pool_size(&self) -> usize {
        let conns = self.connections.lock().await;
        conns.values().map(|v| v.len()).sum()
    }
}

// ── WebSocket task runner ───────────────────────────────────────────

async fn probe_pooled_connection(
    mut conn: PooledConnection,
    worker_name: &str,
) -> Option<PooledConnection> {
    use tokio_tungstenite::tungstenite;

    let ping = Envelope::new("ping", serde_json::json!({}));
    let payload = match serde_json::to_string(&ping) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!("Failed to serialize pool probe for '{worker_name}': {error}");
            return None;
        }
    };

    if let Err(error) = conn
        .stream
        .send(tungstenite::Message::Text(payload.into()))
        .await
    {
        tracing::debug!("Discarding closed pooled connection for '{worker_name}': {error}");
        return None;
    }

    let healthy = tokio::time::timeout(POOLED_CONNECTION_PROBE_TIMEOUT, async {
        loop {
            let message = match conn.stream.next().await {
                Some(Ok(message)) => message,
                Some(Err(error)) => {
                    tracing::debug!(
                        "Discarding broken pooled connection for '{worker_name}': {error}"
                    );
                    return false;
                }
                None => return false,
            };

            if message.is_close() {
                return false;
            }
            if message.is_ping() || message.is_pong() {
                continue;
            }

            let Ok(text) = message.to_text() else {
                return false;
            };
            let Ok(envelope) = serde_json::from_str::<Envelope>(text) else {
                return false;
            };
            return envelope.msg_type == "pong";
        }
    })
    .await
    .unwrap_or(false);

    if healthy {
        Some(conn)
    } else {
        tracing::debug!("Pooled connection probe failed for '{worker_name}'; reconnecting");
        None
    }
}

async fn connect_and_handshake(
    url: &str,
    auth_token: Option<&str>,
    worker_name: &str,
) -> Result<(WsStream, String), OrchestratorError> {
    use tokio_tungstenite::tungstenite;

    let uri = url.parse::<http::Uri>().map_err(|e| {
        OrchestratorError::ExternalWorkerConnectionFailed {
            worker_name: worker_name.to_string(),
            reason: format!("invalid URL: {e}"),
        }
    })?;

    let mut req_builder = http::Request::builder()
        .uri(&uri)
        .header("Sec-WebSocket-Protocol", SUBPROTOCOL);

    if let Some(token) = auth_token {
        req_builder = req_builder.header("Authorization", format!("Bearer {token}"));
    }

    let host = uri.host().unwrap_or("localhost");
    let port_suffix = uri.port_u16().map(|p| format!(":{p}")).unwrap_or_default();
    req_builder = req_builder
        .header("Host", format!("{host}{port_suffix}"))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header(
            "Sec-WebSocket-Key",
            tungstenite::handshake::client::generate_key(),
        )
        .header("Sec-WebSocket-Version", "13");

    let ws_request =
        req_builder
            .body(())
            .map_err(|e| OrchestratorError::ExternalWorkerConnectionFailed {
                worker_name: worker_name.to_string(),
                reason: format!("failed to build request: {e}"),
            })?;

    let connect_timeout = Duration::from_secs(15);
    let (ws_stream, _response) = tokio::time::timeout(
        connect_timeout,
        tokio_tungstenite::connect_async(ws_request),
    )
    .await
    .map_err(|_| OrchestratorError::ExternalWorkerConnectionFailed {
        worker_name: worker_name.to_string(),
        reason: "connection timed out (15s)".to_string(),
    })?
    .map_err(|e| OrchestratorError::ExternalWorkerConnectionFailed {
        worker_name: worker_name.to_string(),
        reason: describe_connect_failure(&e),
    })?;

    let (write_half, mut read) = ws_stream.split();

    let ready_timeout = Duration::from_secs(10);
    let ready_msg = tokio::time::timeout(ready_timeout, read.next())
        .await
        .map_err(|_| OrchestratorError::ExternalWorkerProtocolError {
            worker_name: worker_name.to_string(),
            reason: "no ready message within 10s".to_string(),
        })?
        .ok_or_else(|| OrchestratorError::ExternalWorkerProtocolError {
            worker_name: worker_name.to_string(),
            reason: "connection closed before ready".to_string(),
        })?
        .map_err(|e| OrchestratorError::ExternalWorkerProtocolError {
            worker_name: worker_name.to_string(),
            reason: format!("WebSocket error: {e}"),
        })?;

    let ready_text =
        ready_msg
            .to_text()
            .map_err(|e| OrchestratorError::ExternalWorkerProtocolError {
                worker_name: worker_name.to_string(),
                reason: format!("ready message not text: {e}"),
            })?;

    let ready_env: Envelope = serde_json::from_str(ready_text).map_err(|e| {
        OrchestratorError::ExternalWorkerProtocolError {
            worker_name: worker_name.to_string(),
            reason: format!("invalid ready envelope: {e}"),
        }
    })?;

    if ready_env.msg_type != "ready" {
        return Err(OrchestratorError::ExternalWorkerProtocolError {
            worker_name: worker_name.to_string(),
            reason: format!("expected 'ready', got '{}'", ready_env.msg_type),
        });
    }

    let ready: ReadyPayload = serde_json::from_value(ready_env.payload).map_err(|e| {
        OrchestratorError::ExternalWorkerProtocolError {
            worker_name: worker_name.to_string(),
            reason: format!("invalid ready payload: {e}"),
        }
    })?;

    let stream =
        write_half
            .reunite(read)
            .map_err(|_| OrchestratorError::ExternalWorkerProtocolError {
                worker_name: worker_name.to_string(),
                reason: "failed to reunite WebSocket stream halves".to_string(),
            })?;

    Ok((stream, ready.worker_id))
}

#[allow(clippy::too_many_arguments)]
async fn run_external_task(
    job_id: Uuid,
    url: &str,
    auth_token: Option<&str>,
    task: &str,
    timeout_ms: u64,
    worker_name: &str,
    event_tx: Option<&broadcast::Sender<(Uuid, String, SseEvent)>>,
    context_manager: Option<&Arc<ContextManager>>,
    store: Option<&Arc<dyn Database>>,
    cancel_rx: watch::Receiver<bool>,
    context: TaskContext,
    tool_binding: ToolProxyBinding,
    pool: &WorkerConnectionPool,
    pool_key: &str,
) -> Result<ExternalTaskResult, OrchestratorError> {
    use tokio_tungstenite::tungstenite;

    let event_user_id = context.user_id.clone();

    // Probe pooled connections before dispatch so a worker restart cannot leave
    // the next task attempting to use a dead WebSocket.
    let pooled = match pool.try_acquire(pool_key).await {
        Some(conn) => probe_pooled_connection(conn, worker_name).await,
        None => None,
    };

    let (mut write, mut read, worker_id, from_pool) = if let Some(pooled) = pooled {
        tracing::debug!("Reusing pooled connection for '{worker_name}'");
        let wid = pooled.worker_id.clone();
        let (w, r) = pooled.stream.split();
        (w, r, wid, true)
    } else {
        let (stream, wid) = connect_and_handshake(url, auth_token, worker_name).await?;
        let (w, r) = stream.split();
        (w, r, wid, false)
    };

    let source = if from_pool { "pooled" } else { "new" };
    tracing::info!(
        "External worker '{}' ready (worker_id={}, connection={})",
        worker_name,
        worker_id,
        source
    );

    // Emit job_started event
    emit_event(
        event_tx,
        job_id,
        &event_user_id,
        SseEvent::JobStatus {
            job_id: job_id.to_string(),
            message: format!("Connected to external worker '{worker_name}'"),
        },
    );

    // Send task_request. `timeout_ms` here is the already-resolved config
    // default (a concrete u64, never Option) — it serializes as an integer.
    // `TaskContext` omits None fields (project_dir) because the TS worker
    // bridges reject explicit JSON nulls.
    let task_request = Envelope::new(
        "task_request",
        serde_json::json!({
            "task_id": job_id.to_string(),
            "prompt": task,
            "context": context,
            "timeout_ms": timeout_ms,
        }),
    );

    let msg = tungstenite::Message::Text(
        serde_json::to_string(&task_request)
            .map_err(|e| OrchestratorError::ExternalWorkerProtocolError {
                worker_name: worker_name.to_string(),
                reason: format!("failed to serialize task_request: {e}"),
            })?
            .into(),
    );

    write
        .send(msg)
        .await
        .map_err(|e| OrchestratorError::ExternalWorkerProtocolError {
            worker_name: worker_name.to_string(),
            reason: format!("failed to send task_request: {e}"),
        })?;

    // Read messages until task_result or timeout
    let task_timeout = Duration::from_millis(timeout_ms);
    let mut accumulated_output = String::new();
    let mut cancel_rx = cancel_rx;
    let task_start = tokio::time::Instant::now();
    // MR !1506 review (CQ-3), tightened per !1521 DD-5/DD-10: bound the
    // number of proxied control frames (tool_call AND tool_list) a single
    // job may issue, independent of the per-tool rate limiter. Every
    // tool_call-typed frame counts — accepted, rejected, or malformed — so
    // a rejected-frame flood is capped too.
    let mut tool_calls_used: u32 = 0;

    let result = tokio::time::timeout(task_timeout, async {
        loop {
            tokio::select! {
                msg = read.next() => {
                    let Some(msg_result) = msg else {
                        return Err(OrchestratorError::ExternalWorkerProtocolError {
                            worker_name: worker_name.to_string(),
                            reason: "connection closed unexpectedly".to_string(),
                        });
                    };

                    let ws_msg = msg_result.map_err(|e| {
                        OrchestratorError::ExternalWorkerProtocolError {
                            worker_name: worker_name.to_string(),
                            reason: format!("WebSocket error: {e}"),
                        }
                    })?;

                    if ws_msg.is_close() {
                        return Err(OrchestratorError::ExternalWorkerProtocolError {
                            worker_name: worker_name.to_string(),
                            reason: "connection closed by worker".to_string(),
                        });
                    }

                    if ws_msg.is_ping() || ws_msg.is_pong() {
                        continue;
                    }

                    let text = match ws_msg.to_text() {
                        Ok(t) => t,
                        Err(_) => continue,
                    };

                    // MR !1521 DD-5: an oversized `tool_call` frame is
                    // rejected BEFORE its payload is parsed into a heap
                    // `Value` (the probe borrows from `text`; no
                    // allocation), and every tool_call-typed frame —
                    // accepted or rejected — counts toward the per-job cap
                    // so rejected-frame floods are bounded too. Rejection
                    // rows stop at the cap so a flood cannot spam
                    // job_events. Oversized frames of other types (a
                    // legitimate large task_result/progress payload) are
                    // unaffected.
                    if text.len() > MAX_TOOL_CALL_FRAME_BYTES
                        && matches!(
                            serde_json::from_str::<TypeProbe<'_>>(text),
                            Ok(probe) if probe.msg_type == "tool_call"
                        )
                    {
                        tool_calls_used += 1;
                        if tool_calls_used <= MAX_TOOL_CALLS_PER_JOB {
                            record_tool_result(
                                store,
                                job_id,
                                "unknown",
                                false,
                                "tool_call frame rejected: too large",
                            )
                            .await;
                        }
                        let reply = tool_result_envelope(
                            "",
                            "error",
                            String::new(),
                            Some(format!(
                                "tool_call frame exceeds {MAX_TOOL_CALL_FRAME_BYTES} bytes"
                            )),
                        );
                        send_reply_envelope(&mut write, &reply, worker_name).await;
                        continue;
                    }

                    let env: Envelope = match serde_json::from_str(text) {
                        Ok(e) => e,
                        Err(e) => {
                            tracing::warn!("Ignoring unparseable message from '{}': {}", worker_name, e);
                            continue;
                        }
                    };

                    match env.msg_type.as_str() {
                        "task_progress" => {
                            if let Ok(progress) = serde_json::from_value::<TaskProgressPayload>(env.payload) {
                                // WI#263: a streamed delta is proof the worker
                                // is alive — unless it is a synthetic worker
                                // heartbeat (the kimi executor's "[worker]
                                // Still waiting…" keep-alive). A wedged CLI
                                // emits nothing at all while its heartbeat
                                // claims the task is active, so heartbeats
                                // must not refresh the idle baseline used by
                                // stuck detection.
                                if is_genuine_progress(&progress.delta)
                                    && let Some(cm) = context_manager
                                {
                                    let _ = cm.update_context(job_id, |ctx| ctx.note_activity()).await;
                                }

                                // WI#267: heartbeats are not evidence. They
                                // must stay out of the recovery buffer or a
                                // heartbeat-only timeout (a wedged CLI with a
                                // healthy executor) is later presented as a
                                // preserved "partial investigation" that never
                                // existed. Only the keep-alive *sentence
                                // shape* is excluded (!1237 finding 1): the
                                // idle gate above intentionally stays on the
                                // broad prefix, but genuine `[worker] `
                                // diagnostics such as the opencode executors'
                                // "Transient provider failure; retrying…"
                                // retry notices are real evidence of what the
                                // worker was doing and must survive here.
                                if !is_heartbeat_only_frame(&progress.delta) {
                                    accumulated_output.push_str(&progress.delta);
                                }

                                emit_event(
                                    event_tx,
                                    job_id,
                                    &event_user_id,
                                    SseEvent::JobMessage {
                                        job_id: job_id.to_string(),
                                        content: progress.delta.clone(),
                                        role: "assistant".to_string(),
                                    },
                                );

                                // Persist as job event
                                persist_event(store, job_id, "message", &progress.delta).await;
                            }
                        }
                        "task_result" => {
                            let result: TaskResultPayload =
                                serde_json::from_value(env.payload).map_err(|e| {
                                    OrchestratorError::ExternalWorkerProtocolError {
                                        worker_name: worker_name.to_string(),
                                        reason: format!("invalid task_result: {e}"),
                                    }
                                })?;

                            let status = match result.status.as_str() {
                                "success" => ExternalTaskStatus::Success,
                                "cancelled" => ExternalTaskStatus::Cancelled,
                                "timed_out" => ExternalTaskStatus::TimedOut,
                                _ => ExternalTaskStatus::Failed,
                            };

                            // WI#267: a worker-reported *timeout* can carry
                            // synthetic heartbeats (an executor older than the
                            // daemon, or one that folds its keep-alives into
                            // the final output). Strip them before the buffer
                            // is used as timeout evidence so a heartbeat-only
                            // run is never presented as a partial
                            // investigation. Any other status keeps the
                            // worker-supplied output verbatim — a successful
                            // result is not "evidence" and must not have its
                            // whitespace or genuine `[worker] …` content
                            // rewritten (!1235 finding 2).
                            //
                            // !1237 finding 2: when a timed-out worker result
                            // strips down to nothing (or was empty all along),
                            // the fallback to the daemon's own genuine-progress
                            // buffer must happen HERE — before the result is
                            // constructed — so `res.output` and the persisted
                            // `timed_out_result_message` keep the real streamed
                            // work instead of collapsing to an error-only
                            // string.
                            let final_output = match status {
                                ExternalTaskStatus::TimedOut => {
                                    let sanitized = strip_worker_heartbeats(&result.output);
                                    if sanitized.is_empty() {
                                        accumulated_output.clone()
                                    } else {
                                        sanitized
                                    }
                                }
                                _ if result.output.is_empty() => accumulated_output.clone(),
                                _ => result.output.clone(),
                            };

                            return Ok(ExternalTaskResult {
                                status,
                                output: final_output,
                                error: result.error,
                                duration_ms: result.duration_ms,
                            });
                        }
                        // WI#324: proxied tool path. The worker invokes an
                        // allowlisted in-process tool; the reply rides the
                        // same WS control channel. Handled inline: the call
                        // carries its own per-tool timeout plus a cancel
                        // race, and the outer task deadline still bounds the
                        // whole loop.
                        "tool_call" => {
                            // MR !1521 DD-5: every tool_call-typed frame
                            // counts toward the per-job cap, accepted or not;
                            // rejection rows stop once the cap is crossed (one
                            // marker row is kept). Oversized frames were
                            // already rejected pre-parse above.
                            tool_calls_used += 1;
                            let reply = match serde_json::from_value::<ToolCallPayload>(env.payload)
                            {
                                Ok(call) if tool_calls_used > MAX_TOOL_CALLS_PER_JOB => {
                                    if tool_calls_used == MAX_TOOL_CALLS_PER_JOB + 1 {
                                        record_tool_result(
                                            store,
                                            job_id,
                                            &call.name,
                                            false,
                                            "tool_call denied: per-job call cap reached",
                                        )
                                        .await;
                                    }
                                    tool_result_envelope(
                                        &call.call_id,
                                        "denied",
                                        String::new(),
                                        Some(format!(
                                            "per-job proxied tool call cap ({MAX_TOOL_CALLS_PER_JOB}) reached"
                                        )),
                                    )
                                }
                                Ok(call) => {
                                    let call_ctx = ToolCallCtx {
                                        proxy: tool_binding.proxy.as_ref(),
                                        allowlist: &tool_binding.allowlist,
                                        context_manager,
                                        store,
                                        job_id,
                                        worker_name,
                                        user_id: &event_user_id,
                                    };
                                    handle_tool_call(&call_ctx, call, &cancel_rx).await
                                }
                                Err(_) if tool_calls_used > MAX_TOOL_CALLS_PER_JOB => {
                                    tool_result_envelope(
                                        "",
                                        "denied",
                                        String::new(),
                                        Some(format!(
                                            "per-job proxied tool call cap ({MAX_TOOL_CALLS_PER_JOB}) reached"
                                        )),
                                    )
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Malformed tool_call from '{}': {}",
                                        worker_name,
                                        log_safe(&e.to_string(), 200)
                                    );
                                    record_tool_result(
                                        store,
                                        job_id,
                                        "unknown",
                                        false,
                                        "malformed tool_call frame",
                                    )
                                    .await;
                                    tool_result_envelope(
                                        "",
                                        "error",
                                        String::new(),
                                        Some(format!("malformed tool_call frame: {e}")),
                                    )
                                }
                            };
                            send_reply_envelope(&mut write, &reply, worker_name).await;
                        }
                        // MR !1521 DD-10: `tool_list` is a control frame from
                        // the same untrusted peer — it counts toward the
                        // per-job frame budget, and a payload `task_id`, when
                        // present, must match this connection's job.
                        "tool_list" => {
                            tool_calls_used += 1;
                            let reply = if tool_calls_used > MAX_TOOL_CALLS_PER_JOB {
                                tool_catalog_error(&format!(
                                    "per-job proxied tool call cap ({MAX_TOOL_CALLS_PER_JOB}) reached"
                                ))
                            } else {
                                let task_id_ok =
                                    serde_json::from_value::<ToolListPayload>(env.payload)
                                        .ok()
                                        .and_then(|p| p.task_id)
                                        .is_none_or(|tid| tid == job_id.to_string());
                                if task_id_ok {
                                    build_tool_catalog(
                                        tool_binding.proxy.as_ref(),
                                        &tool_binding.allowlist,
                                        &event_user_id,
                                    )
                                    .await
                                } else {
                                    tool_catalog_error(&format!(
                                        "task_id must equal this connection's job id ({job_id})"
                                    ))
                                }
                            };
                            send_reply_envelope(&mut write, &reply, worker_name).await;
                        }
                        "pong" => {}
                        other => {
                            tracing::debug!("Unknown message type '{}' from '{}'", other, worker_name);
                        }
                    }
                }
                _ = wait_for_cancellation(&mut cancel_rx) => {
                    send_cancel_envelope(&mut write, job_id).await;
                    // Bounded handoff (WI#149): wait for the worker's terminal
                    // frame (a `task_result` for this job) or socket close
                    // before returning, so the cancel actually reaches a live
                    // read loop instead of racing connection teardown. The
                    // worker's ack, when observed, is authoritative for the
                    // cancel bookkeeping; the timeout expiry is the fallback
                    // for pre-WI#101 workers that never ack.
                    //
                    // The wait is capped to the task's remaining budget so a
                    // cancel racing the last seconds of the task window still
                    // resolves as Cancelled — the outer deadline must not drop
                    // this future mid-handoff and misreport TimedOut.
                    let elapsed = tokio::time::Instant::now()
                        .saturating_duration_since(task_start)
                        + CANCEL_ACK_TASK_MARGIN;
                    let ack_budget = task_timeout
                        .saturating_sub(elapsed)
                        .min(CANCEL_ACK_TIMEOUT);
                    if let Some(result) =
                        wait_for_cancel_ack(
                            &mut read,
                            &mut write,
                            job_id,
                            worker_name,
                            ack_budget,
                            store,
                        )
                        .await
                    {
                        return Ok(result);
                    }
                    return Ok(ExternalTaskResult {
                        status: ExternalTaskStatus::Cancelled,
                        output: accumulated_output.clone(),
                        error: Some("Cancelled by user".to_string()),
                        duration_ms: 0,
                    });
                }
            }
        }
    })
    .await;

    let task_result = match result {
        Ok(r) => r?,
        Err(_) => {
            // Tell the worker to abort its underlying provider/session work before
            // dropping the socket. Its own deadline is defense in depth, not the
            // daemon's only cancellation mechanism.
            send_cancel_envelope(&mut write, job_id).await;
            // Bounded handoff on the timeout path too (WI#149): give the worker
            // the same window to acknowledge before the connection is torn
            // down, so its cancel processing isn't racing socket cleanup.
            let _ = wait_for_cancel_ack(
                &mut read,
                &mut write,
                job_id,
                worker_name,
                CANCEL_ACK_TIMEOUT,
                store,
            )
            .await;
            // WI#263: the streamed investigation is real work. Persist a
            // bounded evidence snapshot and a fallback-deliverable marker so
            // the partial result survives the timeout instead of vanishing
            // with the connection.
            preserve_timeout_evidence(
                job_id,
                worker_name,
                &accumulated_output,
                context_manager,
                store,
            )
            .await;
            return Err(OrchestratorError::ExternalWorkerTimeout {
                worker_name: worker_name.to_string(),
                job_id,
            });
        }
    };

    let (success, final_state, status_str, final_msg) = match &task_result.status {
        ExternalTaskStatus::Success => (
            true,
            JobState::Completed,
            "completed",
            format!("Completed: {}", task_result.output),
        ),
        ExternalTaskStatus::Cancelled => (
            false,
            JobState::Cancelled,
            "cancelled",
            "Cancelled by user".to_string(),
        ),
        ExternalTaskStatus::TimedOut => {
            // WI#263: a worker-reported timeout (e.g. the kimi silence
            // watchdog resolving a wedged CLI) carries the streamed
            // investigation in `output`. The deliverable evidence must reach
            // the persisted result — an error string alone loses the work.
            (
                false,
                JobState::Failed,
                "timed_out",
                timed_out_result_message(task_result.error.as_deref(), &task_result.output),
            )
        }
        ExternalTaskStatus::Failed | ExternalTaskStatus::Partial(_) => (
            false,
            JobState::Failed,
            "failed",
            format!(
                "Failed: {}",
                task_result.error.as_deref().unwrap_or("unknown error")
            ),
        ),
    };

    // WI#263: preserve a bounded evidence snapshot whenever the task resolves
    // as timed out — whether the daemon's own deadline fired (handled at the
    // timeout return above) or the *worker* reported TimedOut (e.g. the kimi
    // silence watchdog). Covers both wait=true and wait=false callers: the
    // snapshot lives in job metadata + a job event, not the caller's return
    // value.
    if task_result.status == ExternalTaskStatus::TimedOut {
        // WI#267: by this point `task_result.output` is already the sanitized
        // worker output with the genuine-progress fallback applied (see the
        // task_result frame handler), and `accumulated_output` excludes
        // keep-alive frames (the read loop gates on the heartbeat shape). A
        // heartbeat-only timeout therefore resolves with empty evidence and
        // preserves nothing — which is the correct outcome: nothing genuine
        // streamed. `preserve_timeout_evidence` itself is idempotent w.r.t.
        // heartbeats (it drops whitespace-only buffers), so this is defense
        // in depth rather than a second stripping site.
        let evidence = if task_result.output.is_empty() {
            accumulated_output.clone()
        } else {
            task_result.output.clone()
        };
        preserve_timeout_evidence(job_id, worker_name, &evidence, context_manager, store).await;
    }

    if let Some(cm) = context_manager {
        let transition = cm
            .update_context(job_id, |ctx| {
                if ctx.state == final_state {
                    return Ok(());
                }
                ctx.transition_to(final_state, task_result.error.clone())
            })
            .await;
        match transition {
            Ok(Ok(())) => {}
            Ok(Err(reason)) => {
                // A rejected transition (e.g. terminal state already reached)
                // must not be silently discarded (WI#263): log it so the lost
                // state update is observable, including the recovered output
                // size so operators can see evidence was received.
                tracing::warn!(
                    job_id = %job_id,
                    worker = worker_name,
                    from_state_result = %status_str,
                    output_bytes = final_msg.len(),
                    "Could not apply external worker final state: {reason}"
                );
            }
            Err(error) => {
                tracing::warn!(
                    job_id = %job_id,
                    "Failed to load job context for final state: {error}"
                );
            }
        }
    }

    // Persist final event
    persist_event(store, job_id, "result", &final_msg).await;

    // Emit final SSE event
    emit_event(
        event_tx,
        job_id,
        &event_user_id,
        SseEvent::JobResult {
            job_id: job_id.to_string(),
            status: status_str.to_string(),
            session_id: None,
            fallback_deliverable: None,
        },
    );

    // Update DB job record
    if let Some(db) = store {
        let _ = db
            .update_sandbox_job_status(
                job_id,
                status_str,
                Some(success),
                task_result.error.as_deref(),
                None,
                Some(Utc::now()),
            )
            .await;
    }

    // Return the connection to the pool for reuse on successful tasks.
    // On failure/cancel/timeout paths the connection may be in an
    // indeterminate state, so we drop it instead of risking corruption.
    if success {
        match write.reunite(read) {
            Ok(stream) => {
                tracing::debug!(
                    "Returning connection to pool for '{worker_name}' (key={pool_key})"
                );
                pool.release(
                    pool_key.to_string(),
                    PooledConnection {
                        stream,
                        worker_id,
                        last_used: std::time::Instant::now(),
                    },
                )
                .await;
            }
            Err(e) => {
                tracing::warn!("Failed to reunite WebSocket halves for '{worker_name}': {e}");
            }
        }
    }

    Ok(task_result)
}

// ── Helpers ─────────────────────────────────────────────────────────

async fn wait_for_cancellation(cancel_rx: &mut watch::Receiver<bool>) {
    loop {
        if *cancel_rx.borrow() {
            return;
        }
        if cancel_rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

async fn send_cancel_envelope(write: &mut WsWrite, job_id: Uuid) {
    let cancel_env = Envelope::new(
        "cancel",
        serde_json::json!({ "task_id": job_id.to_string() }),
    );
    let Ok(json) = serde_json::to_string(&cancel_env) else {
        return;
    };
    if tokio::time::timeout(
        CANCEL_SEND_TIMEOUT,
        write.send(tokio_tungstenite::tungstenite::Message::Text(json.into())),
    )
    .await
    .is_err()
    {
        tracing::warn!(%job_id, "Timed out sending external worker cancellation");
    }
}

/// Wait bounded for the worker's terminal frame after a cancel envelope
/// (WI#149). Consumes frames until a `task_result` envelope for `job_id`
/// arrives (returned to the caller so the worker's own cancel status/output
/// wins), the socket closes (worker done; handoff complete), or the deadline
/// expires (pre-WI#101 worker that never acks — proceed with teardown).
///
/// Frame parsing is best-effort by design: progress/pong frames are skipped,
/// and malformed frames don't abort the wait, since all we need is either the
/// terminal frame or the socket's end. `tool_call` frames arriving during the
/// handoff are answered with an `error` reply (MR !1506 review CQ-8): the
/// daemon always answers, and a cancelled job must not start new tool work.
/// Each answered call is also persisted as a canonical `tool_result` row
/// (MR !1521 CQ-4 follow-up), bounded by [`MAX_HANDOFF_TOOL_RESULT_ROWS`] so
/// a cancel-window flood cannot spam job_events.
async fn wait_for_cancel_ack(
    read: &mut WsRead,
    write: &mut WsWrite,
    job_id: Uuid,
    worker_name: &str,
    budget: Duration,
    store: Option<&Arc<dyn Database>>,
) -> Option<ExternalTaskResult> {
    let mut handoff_tool_calls: u32 = 0;
    let deadline = tokio::time::timeout(budget, async {
        loop {
            let msg = read.next().await?;
            let ws_msg = match msg {
                Ok(m) => m,
                Err(_) => return None,
            };
            if ws_msg.is_close() || ws_msg.is_ping() || ws_msg.is_pong() {
                continue;
            }
            let Ok(text) = ws_msg.to_text() else {
                continue;
            };
            let Ok(env) = serde_json::from_str::<Envelope>(text) else {
                continue;
            };
            if env.msg_type == "tool_call" {
                let call_id = env
                    .payload
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if handoff_tool_calls < MAX_HANDOFF_TOOL_RESULT_ROWS {
                    handoff_tool_calls += 1;
                    record_tool_result(
                        store,
                        job_id,
                        "unknown",
                        false,
                        "tool_call rejected during cancel handoff",
                    )
                    .await;
                }
                let reply = tool_result_envelope(
                    &call_id,
                    "error",
                    String::new(),
                    Some("job is being cancelled; tool call rejected".to_string()),
                );
                send_reply_envelope(write, &reply, worker_name).await;
                continue;
            }
            if env.msg_type != "task_result" {
                continue;
            }
            let payload: TaskResultPayload = match serde_json::from_value(env.payload) {
                Ok(p) => p,
                Err(_) => continue,
            };
            if payload.task_id != job_id.to_string() {
                continue;
            }
            let status = match payload.status.as_str() {
                "success" => ExternalTaskStatus::Success,
                "cancelled" => ExternalTaskStatus::Cancelled,
                "timed_out" => ExternalTaskStatus::TimedOut,
                _ => ExternalTaskStatus::Failed,
            };
            return Some(ExternalTaskResult {
                status,
                output: payload.output,
                error: payload.error,
                duration_ms: payload.duration_ms,
            });
        }
    })
    .await;

    match deadline {
        // Inner loop returned: either the terminal frame (Some) or socket
        // close/error (None). Both complete the handoff.
        Ok(inner) => inner,
        // Deadline expired without a terminal frame: handoff window exhausted,
        // fall back to daemon-side cancel bookkeeping.
        Err(_) => {
            tracing::debug!(
                %job_id,
                worker = worker_name,
                budget_ms = budget.as_millis() as u64,
                "External worker did not ack cancel within the handoff window; proceeding"
            );
            None
        }
    }
}

fn emit_event(
    tx: Option<&broadcast::Sender<(Uuid, String, SseEvent)>>,
    job_id: Uuid,
    user_id: &str,
    event: SseEvent,
) {
    if let Some(tx) = tx {
        let _ = tx.send((job_id, user_id.to_string(), event));
    }
}

async fn persist_event(
    store: Option<&Arc<dyn Database>>,
    job_id: Uuid,
    event_type: &str,
    data: &str,
) {
    if let Some(db) = store {
        let value = serde_json::json!({ "content": data });
        if let Err(error) = db.save_job_event(job_id, event_type, &value).await {
            tracing::warn!(
                %job_id,
                event_type,
                "Failed to persist external worker event: {error}"
            );
        }
    }
}

// ── Proxied tool path helpers (WI#324) ──────────────────────────────

// Bounds for the proxied tool path (MR !1506 review CQ-3): the worker is an
// untrusted container, so every worker-controlled field is validated and
// capped before it is parsed, persisted, or logged.
/// Maximum serialized size of one `tool_call` WS frame.
const MAX_TOOL_CALL_FRAME_BYTES: usize = 256 * 1024;
/// Maximum proxied tool calls a single job may issue.
const MAX_TOOL_CALLS_PER_JOB: u32 = 100;
/// Tool names are validated as bounded identifiers before use.
const MAX_TOOL_NAME_BYTES: usize = 128;
/// Worker-chosen correlation ids get the same treatment.
const MAX_CALL_ID_BYTES: usize = 128;
/// Maximum `tool_result` rows persisted for `tool_call` frames answered
/// during a cancel handoff (MR !1521 CQ-4 follow-up: auditable, but bounded).
const MAX_HANDOFF_TOOL_RESULT_ROWS: u32 = 5;

/// Proxy-level hard denylist (MR !1521 DD-4): tools that must NEVER be
/// reachable from an untrusted worker container, enforced both at allowlist
/// load and at dispatch time. These names are NOT on the engine's
/// `AUTONOMOUS_TOOL_DENYLIST` (which targets jobs/routines), so without this
/// list an operator entry like `tool_allowlist = ["shell"]` would pass every
/// gate and hand the container command execution on the daemon host (direct
/// exec when the Docker sandbox is unavailable). The proxied path is
/// phase-1 scoped to the WI#324 capability set (`memory_*`, `knowledge_*`,
/// `gitlab_tool`); host-execution and cross-plane tools stay out of it.
const PROXY_TOOL_DENYLIST: &[&str] = &[
    // Host command/file execution on the daemon.
    "shell",
    "write_file",
    "apply_patch",
    // Outbound HTTP with daemon-side credentials.
    "http",
    // Cross-channel messaging and job control planes.
    "message",
    "cancel_job",
];

/// Whether a tool name is hard-denied for the proxied path (MR !1521 DD-4).
/// Alias-aware callers pass the resolved name; the check is deliberately
/// independent of the autonomous gates so it cannot be unlocked by an
/// allowlist grant.
fn is_proxy_denied_tool(name: &str) -> bool {
    PROXY_TOOL_DENYLIST.contains(&name)
}

/// Whether `name` is permitted by a worker's tool allowlist. Deny-by-default:
/// an empty list rejects everything; exact name match only. There is
/// deliberately no `"*"` wildcard (MR !1506 review CQ-1): a wildcard on an
/// untrusted container is a full tenant-admin grant.
fn tool_name_allowed(allowlist: &[String], name: &str) -> bool {
    allowlist.iter().any(|entry| entry == name)
}

/// Sanitize a worker's configured `tool_allowlist` when the manager takes the
/// config (MR !1506 review CQ-1/CQ-7, MR !1521 DD-4). Dropped entries are
/// config errors worth surfacing, so each is logged:
/// - `"*"` is not honored — it was the rejected MR's full-admin escape hatch;
/// - names on the autonomous denylist can never pass the execution gates;
/// - names on the proxy denylist (`PROXY_TOOL_DENYLIST`: host-execution and
///   cross-plane tools) must never be reachable from an untrusted container;
/// - entries that are not valid tool identifiers are useless;
/// - a cleartext `ws://` endpoint to a non-loopback host strips the whole
///   allowlist: tool authority must never ride a channel an on-path attacker
///   can inject frames into.
fn sanitize_tool_allowlist(
    worker_name: &str,
    allowlist: &[String],
    cleartext_remote: bool,
) -> Vec<String> {
    if cleartext_remote && !allowlist.is_empty() {
        tracing::error!(
            worker = worker_name,
            "tool_allowlist stripped: cleartext ws:// to a non-loopback host may not carry \
             proxied tool authority; use wss:// or a loopback endpoint"
        );
        return Vec::new();
    }
    let mut sanitized = Vec::with_capacity(allowlist.len());
    for entry in allowlist {
        if entry == "*" {
            tracing::warn!(
                worker = worker_name,
                "tool_allowlist entry \"*\" is not supported and was dropped; list tools by name"
            );
            continue;
        }
        if !is_valid_frame_ident(entry, MAX_TOOL_NAME_BYTES) {
            tracing::warn!(
                worker = worker_name,
                entry = %log_safe(entry, 64),
                "tool_allowlist entry is not a valid tool identifier and was dropped"
            );
            continue;
        }
        if is_autonomous_tool_denylisted(entry) {
            tracing::warn!(
                worker = worker_name,
                entry = %entry.as_str(),
                "tool_allowlist entry is on the autonomous denylist and was dropped"
            );
            continue;
        }
        if is_proxy_denied_tool(entry) {
            tracing::warn!(
                worker = worker_name,
                entry = %entry.as_str(),
                "tool_allowlist entry is on the proxy denylist (host-execution/cross-plane) and was dropped"
            );
            continue;
        }
        sanitized.push(entry.clone());
    }
    sanitized
}

/// Whether a worker-supplied frame field is a bounded identifier
/// (`[A-Za-z0-9_-]`, 1..=max bytes). Applied before the value is persisted
/// or logged so newlines/control characters cannot forge log lines or
/// poison job_events rows (MR !1506 review CQ-3).
fn is_valid_frame_ident(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Render an untrusted string safe for a single log line: control characters
/// become spaces and the result is truncated.
fn log_safe(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max_chars)
        .collect()
}

/// Truncate a string for the canonical job_events preview shape (the
/// in-process worker path previews tool input/output at 500 chars).
fn preview(value: &str, max_chars: usize) -> String {
    let mut out: String = value.chars().take(max_chars).collect();
    if value.chars().count() > max_chars {
        out.push('…');
    }
    out
}

/// Build the daemon → worker reply for a `tool_call`. `status` is
/// "success" | "error" | "denied" — a denied call is a hard reject, never a
/// silent empty result.
fn tool_result_envelope(
    call_id: &str,
    status: &str,
    output: String,
    error: Option<String>,
) -> Envelope {
    Envelope::new(
        "tool_result",
        serde_json::json!({
            "call_id": call_id,
            "status": status,
            "output": output,
            "error": error,
        }),
    )
}

/// Send one reply envelope on the task socket. Send failures are logged, not
/// propagated: a broken socket fails the next read anyway, and a tool reply
/// must never abort the surrounding task loop.
async fn send_reply_envelope(write: &mut WsWrite, env: &Envelope, worker_name: &str) {
    let Ok(json) = serde_json::to_string(env) else {
        tracing::warn!(
            "Failed to serialize {} reply for '{worker_name}'",
            env.msg_type
        );
        return;
    };
    if let Err(e) = write
        .send(tokio_tungstenite::tungstenite::Message::Text(json.into()))
        .await
    {
        tracing::warn!(
            "Failed to send {} reply to '{worker_name}': {e}",
            env.msg_type
        );
    }
}

/// Persist a structured job event. Unlike `persist_event`, which wraps a
/// free-form string as `{content}`, this stores the value verbatim so
/// `tool_use`/`tool_result` rows match the schema the web renderer and the
/// orchestrator API already understand (MR !1506 review CQ-4).
async fn persist_event_value(
    store: Option<&Arc<dyn Database>>,
    job_id: Uuid,
    event_type: &str,
    value: serde_json::Value,
) {
    if let Some(db) = store
        && let Err(error) = db.save_job_event(job_id, event_type, &value).await
    {
        tracing::warn!(
            %job_id,
            event_type,
            "Failed to persist external worker event: {error}"
        );
    }
}

/// Record a proxied call in the canonical `tool_use {tool_name,input}` shape
/// (same schema as `worker/job.rs`). The input preview is scrubbed first
/// (MR !1521 DD-7): redacted with the resolved tool's sensitive params and
/// passed through the sanitizer's leak scan, so worker-supplied arguments
/// can never land a secret in job_events. (The tool is resolved here only
/// for its `sensitive_params`; the gates re-resolve authoritatively.)
async fn record_tool_use(ctx: &ToolCallCtx<'_>, call: &ToolCallPayload) {
    let input = match ctx.proxy {
        Some(proxy) => {
            // The resolved Arc must outlive the `sensitive_params` borrow.
            let resolved = proxy
                .tools
                .get_resolved_for_user(&call.name, ctx.user_id)
                .await;
            let sensitive: &[&str] = match &resolved {
                Some((_, tool)) => tool.sensitive_params(),
                None => &[],
            };
            let redacted = redact_params(&call.arguments, sensitive);
            drop(resolved);
            preview(
                &proxy
                    .safety
                    .sanitize_tool_output(&call.name, &redacted.to_string())
                    .content,
                500,
            )
        }
        None => preview(&call.arguments.to_string(), 500),
    };
    persist_event_value(
        ctx.store,
        ctx.job_id,
        "tool_use",
        serde_json::json!({
            "tool_name": call.name,
            "input": input,
        }),
    )
    .await;
}

/// Record the outcome in the canonical `tool_result {tool_name,success,
/// output}` shape. `success: false` is load-bearing: the web renderer
/// computes `data.success !== false`, so denied/failed proxied calls must
/// carry it explicitly or they render as green checkmarks (CQ-4).
async fn record_tool_result(
    store: Option<&Arc<dyn Database>>,
    job_id: Uuid,
    tool_name: &str,
    success: bool,
    output: &str,
) {
    persist_event_value(
        store,
        job_id,
        "tool_result",
        serde_json::json!({
            "tool_name": tool_name,
            "success": success,
            "output": preview(output, 500),
        }),
    )
    .await;
}

/// Everything dispatching one `tool_call` needs that the frame itself must
/// never supply: scoping comes from the connection's job, never from
/// worker-controlled frame fields.
struct ToolCallCtx<'a> {
    proxy: Option<&'a ToolProxy>,
    allowlist: &'a [String],
    context_manager: Option<&'a Arc<ContextManager>>,
    store: Option<&'a Arc<dyn Database>>,
    job_id: Uuid,
    worker_name: &'a str,
    user_id: &'a str,
}

/// Handle one worker `tool_call` frame: job binding → identifier checks →
/// capability gate → tenant-scoped dispatch with the repo's
/// autonomous-execution hard gates → sanitized reply. Every outcome
/// (success, denial, error, malformed) is persisted in the canonical
/// `tool_use`/`tool_result` job_events shape (MR !1506 review CQ-4). The
/// per-job call cap is enforced by the read loop before dispatch (MR !1521
/// DD-5): every tool_call-typed frame counts there, accepted or not.
async fn handle_tool_call(
    ctx: &ToolCallCtx<'_>,
    call: ToolCallPayload,
    cancel_rx: &watch::Receiver<bool>,
) -> Envelope {
    if let Some(reply) = frame_binding_rejection(ctx, &call).await {
        return reply;
    }

    // MR !1521 DD-6: a proxied call is job activity. A tool-only worker that
    // never updates `last_activity_at` looks wedged to stuck-job detection
    // (and to a heartbeat watchdog on the same signal) after the idle
    // threshold, so every accepted call refreshes it — mirroring what a
    // genuine `task_progress` delta does in the read loop.
    if let Some(cm) = ctx.context_manager {
        let _ = cm.update_context(ctx.job_id, |c| c.note_activity()).await;
    }

    // CQ-4: canonical audit trail — the call itself first (input scrubbed
    // per DD-7), then exactly one result row for every outcome below.
    record_tool_use(ctx, &call).await;

    // Deny-by-default capability gate (exact names only — no wildcard).
    if !tool_name_allowed(ctx.allowlist, &call.name) {
        let reason = format!(
            "tool '{}' is not in worker '{}' tool_allowlist (deny-by-default)",
            call.name, ctx.worker_name
        );
        return deny_tool_call(ctx, &call, reason).await;
    }

    let Some(proxy) = ctx.proxy else {
        return fail_tool_call(
            ctx,
            &call,
            "tool proxy is not configured on this daemon".to_string(),
        )
        .await;
    };

    match preflight_gates(ctx, proxy, &call).await {
        Ok(prepared) => execute_proxied_call(ctx, proxy, &call, prepared, cancel_rx).await,
        Err(reply) => reply,
    }
}

/// Frame-level rejections, before the call enters the audit trail as a
/// `tool_use`: job binding (CQ-7) and identifier validation (CQ-3). Each
/// rejected frame still gets exactly one canonical `tool_result` event row
/// (CQ-4). Returns `None` when the frame may proceed. The per-job call cap
/// is NOT here — it is enforced by the read loop before dispatch so that
/// rejected frames count too (MR !1521 DD-5).
async fn frame_binding_rejection(
    ctx: &ToolCallCtx<'_>,
    call: &ToolCallPayload,
) -> Option<Envelope> {
    // CQ-7: bind the frame to this connection's job. `task_id` is required
    // and must match — a leftover worker process on a pooled socket must not
    // run calls in the next job's scope.
    let job_id_str = ctx.job_id.to_string();
    if call.task_id.as_deref() != Some(job_id_str.as_str()) {
        record_tool_result(
            ctx.store,
            ctx.job_id,
            "unknown",
            false,
            "tool_call rejected: task_id missing or mismatched",
        )
        .await;
        return Some(tool_result_envelope(
            &call.call_id,
            "error",
            String::new(),
            Some(format!(
                "task_id must equal this connection's job id ({job_id_str})"
            )),
        ));
    }

    // CQ-3: validate worker-controlled identifiers before they are persisted
    // or logged (raw newlines would forge log lines / poison event rows).
    if !is_valid_frame_ident(&call.call_id, MAX_CALL_ID_BYTES) {
        record_tool_result(
            ctx.store,
            ctx.job_id,
            "unknown",
            false,
            "tool_call rejected: invalid call_id",
        )
        .await;
        return Some(tool_result_envelope(
            "",
            "error",
            String::new(),
            Some("invalid call_id".to_string()),
        ));
    }
    if !is_valid_frame_ident(&call.name, MAX_TOOL_NAME_BYTES) {
        record_tool_result(
            ctx.store,
            ctx.job_id,
            "unknown",
            false,
            "tool_call rejected: invalid tool name",
        )
        .await;
        return Some(tool_result_envelope(
            &call.call_id,
            "error",
            String::new(),
            Some("invalid tool name".to_string()),
        ));
    }

    None
}

/// MR !1521 DD-7: denial/failure reasons can embed hook-supplied text or
/// subsystem errors (context-manager or store error strings, hook rejection
/// reasons). They pass through the same `sanitize_tool_output` pipeline as
/// tool output (size cap + LeakDetector redact/block + policy) before
/// reaching the worker or job_events. Without a wired proxy there is no
/// sanitizer; the text is still rendered control-safe for logs.
fn sanitize_proxy_text(ctx: &ToolCallCtx<'_>, tool_name: &str, text: String) -> String {
    match ctx.proxy {
        Some(proxy) => proxy.safety.sanitize_tool_output(tool_name, &text).content,
        None => log_safe(&text, 500),
    }
}

/// Record a denied call and build its reply (status "denied": policy forbids
/// the tool for this worker, regardless of the invocation's validity).
async fn deny_tool_call(ctx: &ToolCallCtx<'_>, call: &ToolCallPayload, reason: String) -> Envelope {
    let reason = sanitize_proxy_text(ctx, &call.name, reason);
    tracing::warn!(
        worker = ctx.worker_name,
        tool = %call.name,
        %reason,
        "Denied proxied tool call"
    );
    record_tool_result(ctx.store, ctx.job_id, &call.name, false, &reason).await;
    tool_result_envelope(&call.call_id, "denied", String::new(), Some(reason))
}

/// Record a failed call and build its reply (status "error": the invocation
/// itself could not be carried out).
async fn fail_tool_call(ctx: &ToolCallCtx<'_>, call: &ToolCallPayload, reason: String) -> Envelope {
    let reason = sanitize_proxy_text(ctx, &call.name, reason);
    record_tool_result(ctx.store, ctx.job_id, &call.name, false, &reason).await;
    tool_result_envelope(&call.call_id, "error", String::new(), Some(reason))
}

/// A call that passed every preflight gate: the real job context, the
/// resolved (alias-aware) tool name, the vetted tool instance itself, and
/// the normalized, possibly hook-modified parameters.
struct PreparedCall {
    job_ctx: JobContext,
    resolved_name: String,
    tool: Arc<dyn Tool>,
    params: serde_json::Value,
}

/// Run every preflight gate the repo's other autonomous paths enforce
/// (MR !1506 review CQ-1): real job context, user-scoped tool resolution,
/// autonomous hard gates, persisted `Disabled` permission, the WI#322
/// mission-path guard, per-tool rate limit, and `BeforeToolCall` hooks.
/// Returns the prepared call or the reply to send back.
async fn preflight_gates(
    ctx: &ToolCallCtx<'_>,
    proxy: &ToolProxy,
    call: &ToolCallPayload,
) -> Result<PreparedCall, Envelope> {
    // Real job context (CQ-6): a fabricated `InProgress` context would run
    // calls for cancelled/terminal jobs.
    let job_ctx = match load_job_context(ctx, call).await {
        Ok(c) => c,
        Err(reason) => return Err(fail_tool_call(ctx, call, reason).await),
    };
    if job_ctx.state.is_terminal() {
        return Err(fail_tool_call(
            ctx,
            call,
            format!(
                "job {} is {:?}; no new tool calls",
                ctx.job_id, job_ctx.state
            ),
        )
        .await);
    }

    // User-scoped resolution (CQ-6): alias handling plus visibility —
    // another user's MCP/extension tools are NotFound, not leaked.
    let Some((resolved_name, tool)) = proxy
        .tools
        .get_resolved_for_user(&call.name, &job_ctx.user_id)
        .await
    else {
        return Err(
            fail_tool_call(ctx, call, format!("tool '{}' is not registered", call.name)).await,
        );
    };

    let params = prepare_tool_params(tool.as_ref(), &call.arguments);

    // Proxy denylist (MR !1521 DD-4): host-execution and cross-plane tools
    // (shell, write_file, apply_patch, http, message, cancel_job) must never
    // execute for an untrusted container. Enforced at runtime as well as at
    // allowlist load so a registry-side alias or a later-registered tool
    // under a denied name cannot slip through.
    if is_proxy_denied_tool(&resolved_name) {
        return Err(deny_tool_call(
            ctx,
            call,
            format!(
                "tool '{resolved_name}' is on the proxied-path denylist (host-execution/cross-plane tools are never worker-callable)"
            ),
        )
        .await);
    }

    // Autonomous hard gates (CQ-1): the allowlist acts as the approval
    // grant, but the hard exclusions still hold — per-tool autonomous
    // opt-outs and the AUTONOMOUS_TOOL_DENYLIST (routine_*, event_emit,
    // create_job, job_prompt, restart, tool_*, skill_*, secret_list,
    // secret_delete, ssh, ssh_git). Note: the seeded `AskEachTime`
    // permission default is NOT an approval prompt on this path (a worker
    // cannot answer prompts) — the operator's allowlist entry IS the grant;
    // only a persisted `Disabled` still blocks (checked below).
    let requirement = tool.requires_approval(&params);
    let approval = Some(ApprovalContext::autonomous_with_tools(
        ctx.allowlist.iter().cloned(),
    ));
    if !tool.allows_autonomous_execution()
        || ApprovalContext::is_blocked_or_default(&approval, &resolved_name, requirement)
    {
        return Err(deny_tool_call(
            ctx,
            call,
            crate::tools::autonomous_unavailable_message(&resolved_name, &job_ctx.user_id),
        )
        .await);
    }

    // Persisted `Disabled` permission (CQ-1): a user who disabled a tool
    // must not have it executed by a worker. Fails closed on read errors.
    if let Some(reason) =
        persisted_disabled_reason(ctx.store, &proxy.tools, &resolved_name, &job_ctx.user_id).await
    {
        return Err(deny_tool_call(ctx, call, reason).await);
    }

    // WI#322 (open): memory_write can plant engine-internal state (mission
    // records, project/knowledge docs the engine reloads into its trusted
    // store at boot). Until that WI lands, the proxied path refuses writes
    // into the whole `engine/**` tree.
    if let Some(reason) = engine_path_guard_reason(&resolved_name, &params) {
        return Err(deny_tool_call(ctx, call, reason).await);
    }

    // Per-tool rate limit keyed on the job's user (CQ-3).
    if let Some(reason) =
        rate_limit_reason(proxy, &resolved_name, tool.as_ref(), &job_ctx.user_id).await
    {
        return Err(fail_tool_call(ctx, call, reason).await);
    }

    // BeforeToolCall hooks (CQ-1): operator policy may reject or rewrite.
    match run_before_tool_hooks(proxy, &resolved_name, tool.as_ref(), params, &job_ctx).await {
        Ok(params) => Ok(PreparedCall {
            job_ctx,
            resolved_name,
            tool,
            params,
        }),
        Err(reason) => Err(deny_tool_call(ctx, call, reason).await),
    }
}

/// Load the real job context (MR !1506 review CQ-1/CQ-6). With a context
/// manager wired this is authoritative — a missing context fails closed.
/// Harnesses without a context manager fall back to the connection's scope
/// (user_id from the host-side `TaskContext`, state `InProgress`).
async fn load_job_context(
    ctx: &ToolCallCtx<'_>,
    call: &ToolCallPayload,
) -> Result<JobContext, String> {
    match ctx.context_manager {
        Some(cm) => cm
            .get_context(ctx.job_id)
            .await
            .map_err(|e| format!("job context unavailable: {e}")),
        None => {
            let mut job_ctx = JobContext::with_user(
                ctx.user_id,
                format!("external worker '{}' proxied tool call", ctx.worker_name),
                format!("proxied '{}' call (call_id={})", call.name, call.call_id),
            );
            job_ctx.job_id = ctx.job_id;
            job_ctx.state = JobState::InProgress;
            Ok(job_ctx)
        }
    }
}

/// Persisted `Disabled` check against the settings store (MR !1506 review
/// CQ-1), mirroring the engine v2 gate. `None` store = no persisted state to
/// check (harness). Returns the denial reason when the tool may not run.
async fn persisted_disabled_reason(
    store: Option<&Arc<dyn Database>>,
    tools: &Arc<ToolRegistry>,
    name: &str,
    user_id: &str,
) -> Option<String> {
    let store = store?;
    let alias_conflict = matches!(name, "gitlab_tool" | "gitlab-tool")
        && gitlab_permission_alias_conflict(tools).await;
    let mut keys = if alias_conflict {
        vec![format!("tool_permissions.{name}")]
    } else {
        permission_setting_keys(name)
    };
    if let Some(alias) = crate::extensions::naming::legacy_extension_alias(name)
        && !matches!(name, "gitlab_tool" | "gitlab-tool")
    {
        keys.push(format!("tool_permissions.{alias}"));
    }
    for key in keys {
        match store.get_setting(user_id, &key).await {
            Ok(Some(value)) => {
                let state: Option<PermissionState> = serde_json::from_value(value).ok();
                return (state == Some(PermissionState::Disabled)).then(|| {
                    format!("tool '{name}' is disabled for this user in tool permissions")
                });
            }
            Ok(None) => {}
            Err(error) => {
                // Fail closed: an unreadable store may be hiding a persisted
                // `disabled` (mirrors the WI#308 mission gate).
                return Some(format!("tool permissions could not be read: {error}"));
            }
        }
    }
    None
}

/// WI#322 guard (open WI; MR !1506 review CQ-1 scenario C, widened per
/// !1521 DD-3): until the memory tools reserve the engine tree themselves,
/// the proxied path refuses `memory_write` into **any** engine-internal path
/// (`engine/**` — not just the missions family). The engine reloads
/// `engine/projects/**` and `engine/knowledge/**` from the owner workspace at
/// boot, so a worker writable there could plant project/knowledge documents
/// into the trusted in-process engine's store and prompts, not only forge
/// mission records. `memory_write` carries the destination in its `target`
/// parameter (named targets like "memory" resolve outside the engine tree
/// and pass).
fn engine_path_guard_reason(name: &str, params: &serde_json::Value) -> Option<String> {
    if name != "memory_write" {
        return None;
    }
    let target = params.get("target").and_then(|v| v.as_str())?;
    is_engine_internal_path(&normalize_path(target)).then(|| {
        "proxied memory_write may not write engine-internal paths (engine/**) (WI#322)".to_string()
    })
}

/// Per-tool rate limit, keyed on the job's user (MR !1506 review CQ-3): the
/// limiter the in-process paths consult applies to proxied calls too.
async fn rate_limit_reason(
    proxy: &ToolProxy,
    name: &str,
    tool: &dyn Tool,
    user_id: &str,
) -> Option<String> {
    let config = tool.rate_limit_config()?;
    match proxy
        .tools
        .rate_limiter()
        .check_and_record(user_id, name, &config)
        .await
    {
        RateLimitResult::Limited { retry_after, .. } => Some(format!(
            "tool '{name}' rate limit exceeded; retry after {retry_after:?}"
        )),
        RateLimitResult::Allowed { .. } => None,
    }
}

/// Run `BeforeToolCall` hooks with redacted params (CQ-1): operator hook
/// policy applies to proxied calls exactly as to job/routine calls. Hook
/// modifications are re-normalized before use.
async fn run_before_tool_hooks(
    proxy: &ToolProxy,
    name: &str,
    tool: &dyn Tool,
    params: serde_json::Value,
    job_ctx: &JobContext,
) -> Result<serde_json::Value, String> {
    let hook_params = redact_params(&params, tool.sensitive_params());
    let event = HookEvent::ToolCall {
        tool_name: name.to_string(),
        parameters: hook_params,
        user_id: job_ctx.user_id.clone(),
        context: format!("job:{}", job_ctx.job_id),
    };
    match proxy.hooks.run(&event).await {
        Err(HookError::Rejected { reason }) => Err(format!(
            "Blocked by hook: {reason}. This call cannot succeed unchanged; choose an allowed tool or parameters, or ask the user/operator to change policy."
        )),
        Err(err) => Err(format!(
            "Blocked by hook failure mode: {err}. This call cannot succeed unchanged; choose an allowed tool or parameters, or ask the user/operator to repair the hook policy."
        )),
        Ok(HookOutcome::Continue {
            modified: Some(new_params),
        }) => match serde_json::from_str(&new_params) {
            Ok(parsed) => Ok(prepare_tool_params(tool, &parsed)),
            Err(e) => {
                tracing::warn!(
                    tool = %name,
                    "Hook returned non-JSON modification for ToolCall, ignoring: {}",
                    log_safe(&e.to_string(), 200)
                );
                Ok(params)
            }
        },
        _ => Ok(params),
    }
}

/// Execute a prepared call and build the sanitized reply (MR !1506 review
/// CQ-2): output and error text pass through `sanitize_tool_output` (size
/// cap + LeakDetector + policy) before they are sent to the worker or
/// persisted. Execution runs the exact `Arc<dyn Tool>` instance the
/// preflight gates vetted (MR !1521 DD-8) — there is no second registry
/// lookup between the gates and execution, so a registry swap in that window
/// cannot substitute an unvetted instance — and the call races the job's
/// cancel watch (CQ-8) so a cancel is never queued behind a long tool
/// timeout.
async fn execute_proxied_call(
    ctx: &ToolCallCtx<'_>,
    proxy: &ToolProxy,
    call: &ToolCallPayload,
    prepared: PreparedCall,
    cancel_rx: &watch::Receiver<bool>,
) -> Envelope {
    // Same validator the shared pipeline runs, against the hook-final
    // parameters.
    let validation = proxy
        .safety
        .validator()
        .validate_tool_params(&prepared.params);
    if !validation.is_valid {
        let details = validation
            .errors
            .iter()
            .map(|e| format!("{}: {}", e.field, e.message))
            .collect::<Vec<_>>()
            .join("; ");
        return fail_tool_call(ctx, call, format!("Invalid tool parameters: {details}")).await;
    }

    let started = tokio::time::Instant::now();
    let tool_timeout = prepared.tool.execution_timeout();
    let mut cancel_watch = cancel_rx.clone();
    let execution = crate::tools::execute::execute_timed_tool(
        &proxy.tools,
        prepared.tool.as_ref(),
        prepared.params,
        &prepared.job_ctx,
        tool_timeout,
    );
    tokio::pin!(execution);
    let result = tokio::select! {
        res = &mut execution => Ok(res),
        _ = wait_for_cancellation(&mut cancel_watch) => Err("job cancelled".to_string()),
    };
    let elapsed_ms = started.elapsed().as_millis() as u64;

    let (status, output, error) = match result {
        Ok((Ok(Ok(tool_output)), _)) => {
            let serialized = match serde_json::to_string_pretty(&tool_output.result) {
                Ok(s) => s,
                Err(e) => format!("\"<result serialization failed: {e}>\""),
            };
            let sanitized = proxy
                .safety
                .sanitize_tool_output(&prepared.resolved_name, &serialized);
            ("success", sanitized.content, None)
        }
        Ok((Ok(Err(e)), _)) => {
            let sanitized = proxy
                .safety
                .sanitize_tool_output(&prepared.resolved_name, &format!("Error: {e}"));
            ("error", String::new(), Some(sanitized.content))
        }
        Ok((Err(_elapsed), _)) => (
            "error",
            String::new(),
            Some(format!(
                "tool '{}' timed out after {}s",
                prepared.resolved_name,
                tool_timeout.as_secs()
            )),
        ),
        Err(reason) => ("error", String::new(), Some(reason)),
    };
    tracing::info!(
        worker = ctx.worker_name,
        tool = %prepared.resolved_name,
        status,
        elapsed_ms,
        output_bytes = output.len(),
        "Proxied tool call completed"
    );
    record_tool_result(
        ctx.store,
        ctx.job_id,
        &prepared.resolved_name,
        status == "success",
        error.as_deref().unwrap_or(&output),
    )
    .await;

    tool_result_envelope(&call.call_id, status, output, error)
}

/// A `tool_catalog` reply carrying an error instead of a tool list (the
/// reply type stays `tool_catalog` so the worker's read loop is not stuck
/// waiting on a shape it does not know).
fn tool_catalog_error(message: &str) -> Envelope {
    Envelope::new(
        "tool_catalog",
        serde_json::json!({ "tools": [], "error": message }),
    )
}

/// Build the `tool_catalog` reply for a worker `tool_list` request: the
/// definitions of exactly the tools this worker could actually call —
/// allowlisted, visible to the job's user, and not excluded by the
/// autonomous hard gates (MR !1506 review CQ-6) — so a worker-side MCP
/// bridge exposes a truthful `tools/list`.
async fn build_tool_catalog(
    proxy: Option<&ToolProxy>,
    allowlist: &[String],
    user_id: &str,
) -> Envelope {
    let mut tools: Vec<ToolCatalogEntry> = Vec::new();
    if let Some(p) = proxy {
        let approval = Some(ApprovalContext::autonomous_with_tools(
            allowlist.iter().cloned(),
        ));
        let mut seen = std::collections::HashSet::new();
        for name in allowlist {
            let Some((resolved, tool)) = p.tools.get_resolved_for_user(name, user_id).await else {
                continue;
            };
            if !seen.insert(resolved.clone()) {
                continue;
            }
            let requirement = tool.requires_approval(&serde_json::Value::Null);
            if tool.allows_autonomous_execution()
                && !ApprovalContext::is_blocked_or_default(&approval, &resolved, requirement)
            {
                tools.push(ToolCatalogEntry {
                    name: resolved,
                    description: tool.description().to_string(),
                    parameters: tool.parameters_schema(),
                });
            }
        }
        tools.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    }
    Envelope::new("tool_catalog", serde_json::json!({ "tools": tools }))
}

/// Marker prefix workers use for synthetic keep-alive progress (WI#263).
///
/// The kimi executor emits `"[worker] Still waiting …"` heartbeats on a timer
/// while the underlying CLI runs. Those frames prove the *executor* is alive,
/// not that the CLI is producing output, so they must not refresh the
/// idle baseline used by stuck detection.
const WORKER_HEARTBEAT_PREFIX: &str = "[worker] ";

/// Whether a streamed progress delta is genuine worker output rather than a
/// synthetic heartbeat (WI#263).
fn is_genuine_progress(delta: &str) -> bool {
    !delta.trim_start().starts_with(WORKER_HEARTBEAT_PREFIX)
}

/// Structural shape of the synthetic keep-alive sentence every in-tree
/// executor emits on its progress timer — kimi says "… for Kimi Code response
/// …", opencode/opencode2 say "… for OpenCode/model response …", all sharing
/// the same `"[worker] Still waiting for …; task remains active."` frame.
/// Matching the shape (rather than the bare `[worker] ` prefix) keeps genuine
/// worker log lines such as `[worker] initialized` and the opencode
/// executors' `[worker] Transient provider failure; retrying …` diagnostics
/// intact (WI#267 / !1235 / !1237 finding 1).
const WORKER_HEARTBEAT_LINE_PREFIX: &str = "[worker] Still waiting for ";
const WORKER_HEARTBEAT_LINE_SUFFIX: &str = "; task remains active.";

/// Whether a single output line is a synthetic executor keep-alive (WI#267).
///
/// The bare `[worker] ` prefix is too broad for evidence cleanup — workers
/// legitimately emit `[worker] initialized`-style logs and transient-failure
/// retry notices — so the full known heartbeat shape is matched instead.
fn is_heartbeat_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with(WORKER_HEARTBEAT_LINE_PREFIX)
        && trimmed.ends_with(WORKER_HEARTBEAT_LINE_SUFFIX)
}

/// Whether a streamed progress *frame* carries only synthetic keep-alive
/// content (WI#267 / !1237 finding 1 / !1239 finding 1).
///
/// The read loop's evidence gate needs a frame-level predicate: a delta that
/// is entirely heartbeat lines contributes nothing genuine to
/// `accumulated_output`, while a delta mixing genuine content with keep-alive
/// lines keeps its genuine part. This is deliberately narrower than
/// [`is_genuine_progress`]: that broad-prefix predicate drives the idle
/// baseline (any `[worker] ` frame may not prove CLI liveness), but it must
/// not gate the evidence buffer or it would also discard the opencode
/// executors' `[worker] Transient provider failure; retrying …` diagnostics,
/// which are real evidence of what the worker was doing when it timed out.
///
/// At least one actual heartbeat line is required before a frame counts as
/// heartbeat-only (!1239 finding 1): the opencode2 executor forwards raw,
/// untrimmed `session.text.delta` strings, so whitespace-only frames (`" "`,
/// `"\n"`) are genuine stream separators between real content. Treating them
/// as heartbeat-only collapsed `["alpha", " ", "beta", "\n", "gamma"]` into
/// `"alphabetagamma"`, corrupting timeout evidence instead of preserving it.
fn is_heartbeat_only_frame(delta: &str) -> bool {
    let mut saw_heartbeat = false;
    delta.lines().all(|line| {
        let trimmed = line.trim();
        if is_heartbeat_line(trimmed) {
            saw_heartbeat = true;
            true
        } else {
            // Whitespace-only lines are neutral — they carry no keep-alive
            // signal and must not, on their own, mark the frame heartbeat-only.
            trimmed.is_empty()
        }
    }) && saw_heartbeat
}

/// Remove synthetic worker-heartbeat lines from a worker-supplied output
/// buffer (WI#267).
///
/// The daemon's own read loop already keeps heartbeats out of the streamed
/// evidence buffer, but a `task_result.output` comes from the worker and may
/// fold its keep-alives in (e.g. an executor that concatenates every emitted
/// progress frame). Timeout evidence must only ever contain genuine output,
/// so heartbeat lines are dropped here before the buffer is treated as
/// recovered work.
fn strip_worker_heartbeats(output: &str) -> String {
    let mut stripped = String::with_capacity(output.len());
    for line in output.split_inclusive('\n') {
        if is_heartbeat_line(line) {
            continue;
        }
        stripped.push_str(line);
    }
    stripped.trim().to_string()
}

/// Build the persisted result message for a worker-reported timeout (WI#263).
///
/// The worker's `task_result.output` carries whatever the CLI streamed before
/// wedging (e.g. the kimi silence watchdog). Appending it to the persisted
/// result keeps the investigation readable from `job_events` / the job record
/// even though the job resolves as `timed_out`; an error string alone would
/// discard the work.
fn timed_out_result_message(error: Option<&str>, output: &str) -> String {
    let reason = error.unwrap_or("External worker timed out");
    if output.trim().is_empty() {
        reason.to_string()
    } else {
        format!("{reason}\n\nPartial output:\n{output}")
    }
}

/// Cap on the streamed-output snapshot persisted when an external worker task
/// times out (WI#263). Evidence beyond this is already persisted event-by-event
/// via `persist_event`; the snapshot is the tail an operator reads first.
const TIMEOUT_EVIDENCE_SNAPSHOT_BYTES: usize = 16_384;

/// Preserve a partial-evidence snapshot when an external worker task times out
/// (WI#263).
///
/// Real investigations routinely exceed the task deadline at the final
/// report-composition step; until now the streamed evidence was dropped with
/// the connection and the report was unrecoverable. Store the output tail in
/// `JobContext.metadata["partial_output"]` (bounded) and persist an explicit
/// event so the work is visible from `job_events` / `job_status` even though
/// the job resolves as TimedOut. Applies to both the daemon-deadline path and
/// worker-reported timeouts (the kimi silence watchdog), for wait=true and
/// wait=false callers alike.
///
/// Any **nonempty** genuine output is preserved (WI#263 review finding): a
/// short streamed investigation is still real evidence, and dropping it under
/// an arbitrary size floor left `job_status.partial_output` null exactly when
/// the recovery path was needed most.
async fn preserve_timeout_evidence(
    job_id: Uuid,
    worker_name: &str,
    accumulated_output: &str,
    context_manager: Option<&Arc<ContextManager>>,
    store: Option<&Arc<dyn Database>>,
) {
    let trimmed = accumulated_output.trim();
    if trimmed.is_empty() {
        // Nothing genuine streamed — nothing to preserve.
        return;
    }

    let snapshot: String = if trimmed.len() <= TIMEOUT_EVIDENCE_SNAPSHOT_BYTES {
        trimmed.to_string()
    } else {
        let start = floor_char_boundary(trimmed, trimmed.len() - TIMEOUT_EVIDENCE_SNAPSHOT_BYTES);
        trimmed[start..].to_string()
    };

    if let Some(cm) = context_manager {
        let stored = cm
            .update_context(job_id, |ctx| {
                if !ctx.metadata.is_object() {
                    ctx.metadata = serde_json::json!({});
                }
                ctx.metadata["partial_output"] = serde_json::Value::String(snapshot.clone());
            })
            .await;
        if stored.is_err() {
            tracing::warn!(%job_id, "Failed to store partial output in job context");
        }
    }

    persist_event(
        store,
        job_id,
        "partial_output",
        &format!(
            "[worker {worker_name} timed out after streaming {total} bytes; tail preserved]\n{snapshot}",
            total = accumulated_output.len(),
        ),
    )
    .await;
}

/// UTF-8-safe floor of a char boundary, mirroring `str::floor_char_boundary`
/// (still unstable on our MSRV).
fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

async fn record_external_task_error(
    job_id: Uuid,
    error: &OrchestratorError,
    event_tx: Option<&broadcast::Sender<(Uuid, String, SseEvent)>>,
    context_manager: Option<&Arc<ContextManager>>,
    store: Option<&Arc<dyn Database>>,
) {
    let reason = error.to_string();
    let status = if matches!(error, OrchestratorError::ExternalWorkerTimeout { .. }) {
        "timed_out"
    } else {
        "failed"
    };
    let mut user_id = String::new();

    if let Some(cm) = context_manager {
        if let Ok(context) = cm.get_context(job_id).await {
            user_id = context.user_id;
        }
        let _ = cm
            .update_context(job_id, |ctx| {
                ctx.transition_to(JobState::Failed, Some(reason.clone()))
            })
            .await;
    }

    persist_event(store, job_id, "result", &format!("{status}: {reason}")).await;
    emit_event(
        event_tx,
        job_id,
        &user_id,
        SseEvent::JobResult {
            job_id: job_id.to_string(),
            status: status.to_string(),
            session_id: None,
            fallback_deliverable: None,
        },
    );

    if let Some(db) = store {
        let _ = db
            .update_sandbox_job_status(
                job_id,
                status,
                Some(false),
                Some(&reason),
                None,
                Some(Utc::now()),
            )
            .await;
    }
}

async fn record_external_task_cancelled(
    job_id: Uuid,
    user_id: &str,
    event_tx: Option<&broadcast::Sender<(Uuid, String, SseEvent)>>,
    context_manager: Option<&Arc<ContextManager>>,
    store: Option<&Arc<dyn Database>>,
    reason: &str,
) {
    if let Some(context_manager) = context_manager {
        let _ = context_manager
            .update_context(job_id, |context| {
                if context.state == JobState::Cancelled {
                    Ok(())
                } else {
                    context.transition_to(JobState::Cancelled, Some(reason.to_string()))
                }
            })
            .await;
    }
    persist_event(store, job_id, "result", reason).await;
    emit_event(
        event_tx,
        job_id,
        user_id,
        SseEvent::JobResult {
            job_id: job_id.to_string(),
            status: "cancelled".to_string(),
            session_id: None,
            fallback_deliverable: None,
        },
    );
    if let Some(store) = store {
        let _ = store
            .update_sandbox_job_status(
                job_id,
                "cancelled",
                Some(false),
                Some(reason),
                None,
                Some(Utc::now()),
            )
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LoadBalanceStrategy;
    use lunarwing_common::PromptContextRole;

    #[test]
    fn tool_allowlist_gate_is_deny_by_default() {
        // WI#324 + MR !1506 review (CQ-1): empty allowlist rejects
        // everything; exact names opt in; non-matching names stay rejected.
        // There is no "*" wildcard — it was a full tenant-admin grant.
        let empty: &[String] = &[];
        assert!(!tool_name_allowed(empty, "memory_read"));

        let named: Vec<String> = vec!["memory_read".to_string(), "knowledge_search".to_string()];
        assert!(tool_name_allowed(&named, "memory_read"));
        assert!(tool_name_allowed(&named, "knowledge_search"));
        assert!(!tool_name_allowed(&named, "memory_write"));
        assert!(!tool_name_allowed(&named, "shell"));

        let wildcard: Vec<String> = vec!["*".to_string()];
        assert!(!tool_name_allowed(&wildcard, "shell"));
    }

    #[test]
    fn allowlist_sanitizer_drops_wildcard_denylisted_and_invalid_entries() {
        // MR !1506 review (CQ-1): denylisted names can never pass the
        // execution gates, wildcards are not honored, and non-identifier
        // entries are useless — all are dropped at manager load.
        let raw: Vec<String> = vec![
            "memory_read".to_string(),
            "*".to_string(),
            "restart".to_string(),
            "create_job".to_string(),
            "secret_list".to_string(),
            "not a tool".to_string(),
            "line\nbreak".to_string(),
        ];
        let sanitized = sanitize_tool_allowlist("w", &raw, false);
        assert_eq!(sanitized, vec!["memory_read".to_string()]);
    }

    #[test]
    fn allowlist_sanitizer_strips_everything_on_cleartext_remote() {
        // MR !1506 review (CQ-7): cleartext ws:// to a non-loopback host may
        // not carry tool authority — an on-path attacker could inject
        // tool_call frames. Loopback cleartext keeps its allowlist.
        let raw: Vec<String> = vec!["memory_read".to_string()];
        assert!(sanitize_tool_allowlist("w", &raw, true).is_empty());
        assert_eq!(
            sanitize_tool_allowlist("w", &raw, false),
            vec!["memory_read".to_string()]
        );
    }

    #[test]
    fn frame_ident_validation_rejects_control_and_oversized() {
        assert!(is_valid_frame_ident("memory_read", MAX_TOOL_NAME_BYTES));
        assert!(is_valid_frame_ident("gitlab-tool", MAX_TOOL_NAME_BYTES));
        assert!(!is_valid_frame_ident("", MAX_TOOL_NAME_BYTES));
        assert!(!is_valid_frame_ident("has space", MAX_TOOL_NAME_BYTES));
        assert!(!is_valid_frame_ident("line\nbreak", MAX_TOOL_NAME_BYTES));
        assert!(!is_valid_frame_ident("dot.name", MAX_TOOL_NAME_BYTES));
        let oversized = "a".repeat(MAX_TOOL_NAME_BYTES + 1);
        assert!(!is_valid_frame_ident(&oversized, MAX_TOOL_NAME_BYTES));
        assert_eq!(log_safe("a\nb\rc", 16), "a b c");
    }

    #[test]
    fn tool_result_envelope_shape() {
        // The worker correlates replies by call_id; status is one of
        // success/error/denied. Serialize the envelope and inspect the wire
        // shape a worker-side bridge would parse.
        let env = tool_result_envelope("call-7", "denied", String::new(), Some("nope".to_string()));
        let json = serde_json::to_string(&env).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed["type"], "tool_result");
        assert_eq!(parsed["payload"]["call_id"], "call-7");
        assert_eq!(parsed["payload"]["status"], "denied");
        assert_eq!(parsed["payload"]["error"], "nope");
    }

    #[test]
    fn tool_call_payload_deserializes_with_defaults() {
        // `arguments` and `task_id` are optional at the serde layer; the
        // handler still requires `task_id` to equal the connection's job id.
        let payload: ToolCallPayload =
            serde_json::from_str(r#"{"call_id":"c1","name":"memory_read"}"#)
                .expect("minimal payload");
        assert_eq!(payload.call_id, "c1");
        assert_eq!(payload.name, "memory_read");
        assert!(payload.arguments.is_null());
        assert!(payload.task_id.is_none());
    }

    #[test]
    fn floor_char_boundary_is_utf8_safe() {
        // ASCII passes through unchanged.
        assert_eq!(floor_char_boundary("hello", 3), 3);
        // Index beyond the end clamps to the length.
        assert_eq!(floor_char_boundary("hello", 99), 5);
        // "é" occupies bytes 3..5 in "café": an index landing mid-codepoint
        // backs up to the preceding boundary instead of splitting it.
        let s = "café";
        assert_eq!(floor_char_boundary(s, 3), 3);
        assert_eq!(floor_char_boundary(s, 4), 3);
        // The tail slice must itself be valid UTF-8 (no panic on slicing).
        let long = format!("{}{}", "é".repeat(100), "x".repeat(50));
        let start = floor_char_boundary(&long, long.len() - 10);
        assert!(long.is_char_boundary(start));
        let _tail = &long[start..];
    }

    #[test]
    fn heartbeats_are_not_genuine_progress() {
        // WI#263: the kimi executor's synthetic keep-alive must not refresh
        // the idle baseline — a wedged CLI emits nothing while the heartbeat
        // claims the task is active.
        assert!(!is_genuine_progress(
            "[worker] Still waiting for Kimi Code response; task remains active."
        ));
        assert!(!is_genuine_progress("  [worker] Still waiting…"));
        // Real CLI output (including output that merely mentions the worker)
        // still counts.
        assert!(is_genuine_progress(
            "Investigation step: evidence gathered."
        ));
        assert!(is_genuine_progress("[assistant] partial report"));
        assert!(is_genuine_progress(""));
    }

    #[test]
    fn timed_out_result_message_keeps_partial_output() {
        // WI#263: a worker-reported timeout carrying streamed output must
        // persist that output alongside the error — the investigation is the
        // deliverable, and it is unrecoverable from the error string alone.
        let msg = timed_out_result_message(
            Some("kimi produced no stream output for 900000ms"),
            "Investigation step: evidence gathered.",
        );
        assert!(msg.contains("no stream output for 900000ms"));
        assert!(msg.contains("Partial output:"));
        assert!(msg.contains("Investigation step: evidence gathered."));

        // Without output (or whitespace-only), the reason stands alone.
        assert_eq!(
            timed_out_result_message(Some("External worker timed out"), "   "),
            "External worker timed out"
        );
        assert_eq!(
            timed_out_result_message(None, ""),
            "External worker timed out"
        );
    }

    #[tokio::test]
    async fn preserve_timeout_evidence_stores_bounded_tail() {
        let cm = std::sync::Arc::new(crate::context::ContextManager::new(10));
        let job_id = Uuid::new_v4();
        cm.register_sandbox_job(job_id, "user-1", "task", "desc")
            .await
            .unwrap();

        let streamed = "evidence line\n".repeat(4_000); // ~56 KB, over the cap
        preserve_timeout_evidence(job_id, "kimi", &streamed, Some(&cm), None).await;

        let ctx = cm.get_context(job_id).await.unwrap();
        let partial = ctx
            .metadata
            .get("partial_output")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(!partial.is_empty());
        assert!(
            partial.len() <= TIMEOUT_EVIDENCE_SNAPSHOT_BYTES + 16,
            "snapshot must be bounded, got {}",
            partial.len()
        );
        // Bounded tail keeps the END of the stream (the report-composition
        // step), not the beginning.
        assert!(partial.ends_with("evidence line"));
    }

    #[tokio::test]
    async fn preserve_timeout_evidence_skips_trivial_output() {
        let cm = std::sync::Arc::new(crate::context::ContextManager::new(10));
        let job_id = Uuid::new_v4();
        cm.register_sandbox_job(job_id, "user-1", "task", "desc")
            .await
            .unwrap();

        preserve_timeout_evidence(job_id, "kimi", "   ", Some(&cm), None).await;

        let ctx = cm.get_context(job_id).await.unwrap();
        assert!(ctx.metadata.get("partial_output").is_none());
    }

    /// WI#263 review finding: short genuine evidence must be preserved too.
    /// A streamed investigation of only a few bytes (e.g. the silence
    /// watchdog firing shortly after the CLI's first — and only — progress
    /// event) is still real evidence; the old 512-byte floor dropped it and
    /// left `job_status.partial_output` null.
    #[tokio::test]
    async fn preserve_timeout_evidence_keeps_short_genuine_output() {
        let cm = std::sync::Arc::new(crate::context::ContextManager::new(10));
        let job_id = Uuid::new_v4();
        cm.register_sandbox_job(job_id, "user-1", "task", "desc")
            .await
            .unwrap();

        let short = "Investigation step: evidence gathered.";
        preserve_timeout_evidence(job_id, "kimi", short, Some(&cm), None).await;

        let ctx = cm.get_context(job_id).await.unwrap();
        assert_eq!(
            ctx.metadata.get("partial_output").and_then(|v| v.as_str()),
            Some(short),
            "short genuine output must be preserved verbatim"
        );
    }

    #[tokio::test]
    async fn heartbeat_only_streams_do_not_refresh_idle_baseline() {
        // WI#263 end-to-end at the context level: only genuine deltas may
        // note activity, so a wedged worker whose executor keeps sending
        // heartbeats still crosses the idle threshold.
        let cm = std::sync::Arc::new(crate::context::ContextManager::new(10));
        let job_id = Uuid::new_v4();
        cm.register_sandbox_job(job_id, "user-1", "task", "desc")
            .await
            .unwrap();

        // Backdate the start and record one genuine activity long ago…
        cm.update_context(job_id, |ctx| -> Result<(), crate::error::JobError> {
            ctx.started_at = Some(chrono::Utc::now() - chrono::Duration::seconds(3600));
            ctx.last_activity_at = Some(chrono::Utc::now() - chrono::Duration::seconds(600));
            Ok(())
        })
        .await
        .unwrap()
        .unwrap();

        // …then simulate the orchestrator's progress handler on heartbeat
        // frames only (mirrors the is_genuine_progress gate).
        for _ in 0..3 {
            let delta = "[worker] Still waiting for Kimi Code response; task remains active.";
            if is_genuine_progress(delta) {
                cm.update_context(job_id, |ctx| ctx.note_activity())
                    .await
                    .unwrap();
            }
        }

        let stuck = cm
            .find_stuck_jobs_with_threshold(Some(std::time::Duration::from_secs(300)))
            .await;
        assert_eq!(
            stuck.len(),
            1,
            "heartbeats must not keep a wedged job alive"
        );
        assert_eq!(stuck[0], job_id);
    }

    #[test]
    fn strip_worker_heartbeats_removes_only_keepalive_lines() {
        // WI#267: a worker-supplied buffer that folded its synthetic
        // keep-alives in must lose them (and only them) before it can be
        // treated as timeout evidence.
        let heartbeat = "[worker] Still waiting for Kimi Code response; task remains active.";
        let mixed = format!(
            "{heartbeat}\nInvestigation step: evidence gathered.\n{heartbeat}\n[tool result] ok\n  {heartbeat}\n"
        );
        assert_eq!(
            strip_worker_heartbeats(&mixed),
            "Investigation step: evidence gathered.\n[tool result] ok"
        );

        // A heartbeat-only buffer strips to empty — nothing genuine streamed.
        assert_eq!(
            strip_worker_heartbeats(&format!("{heartbeat}\n{heartbeat}")),
            ""
        );

        // Genuine-only buffers survive verbatim (modulo the tail trim).
        let genuine = "line one\nline two";
        assert_eq!(strip_worker_heartbeats(genuine), genuine);
        assert_eq!(strip_worker_heartbeats(""), "");

        // The opencode executors emit the same shape with a different subject
        // — it is the same synthetic keep-alive and must go too.
        assert_eq!(
            strip_worker_heartbeats(
                "[worker] Still waiting for OpenCode/model response; task remains active."
            ),
            ""
        );

        // A genuine worker log line sharing the `[worker] ` prefix is NOT a
        // heartbeat and must survive (!1235 finding 2) — including the
        // opencode executors' transient-failure retry diagnostics, which are
        // real evidence of what the worker was doing (!1237 finding 1).
        assert_eq!(
            strip_worker_heartbeats("[worker] initialized session"),
            "[worker] initialized session"
        );
        assert_eq!(
            strip_worker_heartbeats(
                "[worker] Transient provider failure; retrying same OpenCode session (1/3)."
            ),
            "[worker] Transient provider failure; retrying same OpenCode session (1/3)."
        );
        // Nor is a near-miss of the heartbeat sentence.
        assert_eq!(
            strip_worker_heartbeats("[worker] Still waiting for Kimi Code response (retrying)"),
            "[worker] Still waiting for Kimi Code response (retrying)"
        );
    }

    #[test]
    fn heartbeat_only_frames_are_genuine_progress_diagnostics_survive() {
        // !1237 finding 1: the evidence-buffer gate is the narrow
        // keep-alive-shape predicate, NOT the broad idle-baseline prefix.
        // Synthetic keep-alives must be excluded from the buffer…
        assert!(is_heartbeat_only_frame(
            "[worker] Still waiting for Kimi Code response; task remains active."
        ));
        assert!(is_heartbeat_only_frame(
            "[worker] Still waiting for OpenCode/model response; task remains active.\n"
        ));
        assert!(is_heartbeat_only_frame(
            "  [worker] Still waiting for Kimi Code response; task remains active.  \n\n"
        ));
        // !1239 finding 1: a frame must carry an actual heartbeat line to be
        // heartbeat-only. Whitespace-only frames are genuine stream
        // separators (the opencode2 executor forwards raw, untrimmed
        // `session.text.delta` strings) and empty deltas carry nothing —
        // neither may be dropped from the evidence buffer.
        assert!(!is_heartbeat_only_frame(""));
        assert!(!is_heartbeat_only_frame(" "));
        assert!(!is_heartbeat_only_frame("\n"));
        assert!(!is_heartbeat_only_frame(" \n \n  "));

        // …while genuine `[worker] `-prefixed diagnostics stay in the buffer.
        let transient =
            "[worker] Transient provider failure; retrying same OpenCode session (2/5).";
        assert!(!is_heartbeat_only_frame(transient));
        assert!(!is_heartbeat_only_frame("[worker] initialized session"));
        // The two predicates intentionally disagree on the transient notice:
        // it may not refresh the idle baseline (any `[worker] ` frame is
        // executor-authored, not CLI-authored)…
        assert!(!is_genuine_progress(transient));
        // …but it IS recoverable evidence.
        let mut buffer = String::new();
        if !is_heartbeat_only_frame(transient) {
            buffer.push_str(transient);
        }
        assert_eq!(buffer, transient);
    }

    /// !1239 finding 1 regression: the evidence gate must preserve
    /// whitespace-only progress frames. The opencode2 executor forwards raw,
    /// untrimmed `session.text.delta` strings, so separators arrive as their
    /// own `" "` / `"\n"` frames between genuine deltas; dropping them
    /// concatenated the surrounding words and corrupted the recovered
    /// evidence (`"alpha beta\ngamma"` became `"alphabetagamma"`).
    #[test]
    fn whitespace_only_frames_are_preserved_in_the_evidence_buffer() {
        // Simulate the read loop's progress handling on a delta stream with
        // whitespace separators as standalone frames — exact opencode2 shape.
        let frames = ["alpha", " ", "beta", "\n", "gamma"];
        let mut accumulated_output = String::new();
        for delta in frames {
            if !is_heartbeat_only_frame(delta) {
                accumulated_output.push_str(delta);
            }
        }
        assert_eq!(accumulated_output, "alpha beta\ngamma");

        // A heartbeat interleaved with genuine separators stays excluded
        // without eating the separators around it.
        let heartbeat = "[worker] Still waiting for OpenCode/model response; task remains active.";
        let mut mixed = String::new();
        for delta in ["one\n", heartbeat, "\ntwo"] {
            if !is_heartbeat_only_frame(delta) {
                mixed.push_str(delta);
            }
        }
        assert_eq!(mixed, "one\n\ntwo");
    }

    /// WI#267: the companion case to the short-genuine-output finding. The
    /// read loop now keeps synthetic heartbeats out of the evidence buffer,
    /// so a heartbeat-only timeout (a wedged CLI behind a healthy executor)
    /// must preserve NOTHING — the streamed buffer is empty of genuine work
    /// and must not be presented as a recovered investigation.
    #[tokio::test]
    async fn heartbeat_only_timeout_preserves_no_evidence() {
        let cm = std::sync::Arc::new(crate::context::ContextManager::new(10));
        let job_id = Uuid::new_v4();
        cm.register_sandbox_job(job_id, "user-1", "task", "desc")
            .await
            .unwrap();

        // Simulate the read loop's progress handling on heartbeat frames
        // only — exactly what a wedged CLI with a healthy executor streams
        // until the 15-minute silence watchdog fires.
        let mut accumulated_output = String::new();
        for _ in 0..5 {
            let delta = "[worker] Still waiting for Kimi Code response; task remains active.";
            if !is_heartbeat_only_frame(delta) {
                accumulated_output.push_str(delta);
            }
        }

        // The worker's timeout result carries no genuine output either (the
        // kimi executor only accumulates real CLI stdout), so the daemon's
        // fallback for an empty result output is the heartbeat-free buffer.
        let evidence = if accumulated_output.is_empty() {
            String::new()
        } else {
            accumulated_output
        };
        preserve_timeout_evidence(job_id, "kimi", &evidence, Some(&cm), None).await;

        let ctx = cm.get_context(job_id).await.unwrap();
        assert!(
            ctx.metadata.get("partial_output").is_none(),
            "a heartbeat-only timeout must not be preserved as evidence"
        );
    }

    #[test]
    fn describe_connect_failure_enriches_subprotocol_rejection() {
        use tokio_tungstenite::tungstenite::http::Response;

        let rejected = tokio_tungstenite::tungstenite::Error::Http(
            Response::builder()
                .status(400)
                .body(Some(Vec::new()))
                .expect("valid http response"),
        );
        let reason = describe_connect_failure(&rejected);
        assert!(
            reason.contains("lunarwing-agent-v1"),
            "hint should name the only offered subprotocol: {reason}"
        );
        assert!(
            reason.contains("ironclaw-agent-v1"),
            "hint should name the removed alias: {reason}"
        );
    }

    #[test]
    fn describe_connect_failure_keeps_other_errors_plain() {
        use tokio_tungstenite::tungstenite::Error;

        let io_error = Error::Io(std::io::Error::other("refused"));
        assert_eq!(
            describe_connect_failure(&io_error),
            io_error.to_string(),
            "non-HTTP failures must keep their plain display form"
        );
    }

    #[test]
    fn describe_connect_failure_keeps_auth_rejection_plain() {
        // Regression test for the !893 follow-up review: an auth rejection
        // (401, no echoed subprotocol) must NOT carry the rolling-upgrade
        // hint — the operator's real problem is a mismatched AGENT_AUTH_TOKEN,
        // not a stale worker container.
        use tokio_tungstenite::tungstenite::http::Response;

        let rejected = tokio_tungstenite::tungstenite::Error::Http(
            Response::builder()
                .status(401)
                .body(Some(Vec::new()))
                .expect("valid http response"),
        );
        let reason = describe_connect_failure(&rejected);
        assert!(
            !reason.contains("ironclaw-agent-v1"),
            "auth rejection must not carry the worker-upgrade hint: {reason}"
        );
    }

    #[test]
    fn describe_connect_failure_enriches_upgrade_required_rejection() {
        // 426 Upgrade Required is the other protocol-mismatch status the
        // heuristic recognizes; it must still get the operator hint.
        use tokio_tungstenite::tungstenite::http::Response;

        let rejected = tokio_tungstenite::tungstenite::Error::Http(
            Response::builder()
                .status(426)
                .body(Some(Vec::new()))
                .expect("valid http response"),
        );
        let reason = describe_connect_failure(&rejected);
        assert!(
            reason.contains("ironclaw-agent-v1"),
            "426 rejection should carry the worker-upgrade hint: {reason}"
        );
    }

    #[test]
    fn envelope_serialization() {
        let env = Envelope::new("task_request", serde_json::json!({"prompt": "hello"}));
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"type\":\"task_request\""));
        assert!(json.contains("\"prompt\":\"hello\""));
    }

    #[test]
    fn envelope_deserialization() {
        let json = r#"{"id":"abc","type":"ready","timestamp":"2026-01-01T00:00:00Z","payload":{"worker_id":"w1","version":"1.0","mode":"websocket"}}"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        assert_eq!(env.msg_type, "ready");
        let ready: ReadyPayload = serde_json::from_value(env.payload).unwrap();
        assert_eq!(ready.worker_id, "w1");
    }

    #[test]
    fn task_result_deserialization() {
        let json = r#"{"task_id":"abc","status":"success","output":"done","error":null,"duration_ms":1234}"#;
        let result: TaskResultPayload = serde_json::from_str(json).unwrap();
        assert_eq!(result.status, "success");
        assert_eq!(result.output, "done");
        assert!(result.error.is_none());
        assert_eq!(result.duration_ms, 1234);
    }

    #[test]
    fn manager_new_empty() {
        let mgr = ExternalWorkerManager::new(vec![]);
        assert!(mgr.is_empty());
        assert!(mgr.get_worker("nanocode").is_none());
    }

    #[test]
    fn manager_new_with_workers() {
        let mgr = ExternalWorkerManager::new(vec![
            ExternalWorkerConfig {
                name: "nanocode".to_string(),
                url: "ws://localhost:9090/ws/agent".to_string(),
                auth_token: Some(secrecy::SecretString::from("tok")),
                timeout_ms: 300_000,
                ..Default::default()
            },
            ExternalWorkerConfig {
                name: "pebble".to_string(),
                url: "ws://localhost:8443".to_string(),
                timeout_ms: 600_000,
                ..Default::default()
            },
        ]);
        assert!(!mgr.is_empty());
        assert!(mgr.get_worker("nanocode").is_some());
        assert!(mgr.get_worker("pebble").is_some());
        assert!(mgr.get_worker("unknown").is_none());
        assert_eq!(mgr.worker_names().len(), 2);
    }

    #[test]
    fn job_mode_db_roundtrip() {
        use crate::orchestrator::job_manager::JobMode;
        let ext = JobMode::External("nanocode".to_string());
        assert_eq!(ext.db_value(), "external:nanocode");
        assert_eq!(JobMode::from_db_value("external:nanocode"), ext);

        let worker = JobMode::Worker;
        assert_eq!(worker.db_value(), "worker");
        assert_eq!(JobMode::from_db_value("worker"), worker);
    }

    #[test]
    fn task_context_full_roundtrip() {
        let ctx = TaskContext {
            project_dir: Some("/workspace/myproject".to_string()),
            conversation_history: vec![
                ConversationMessage {
                    role: PromptContextRole::User,
                    content: "fix the bug".to_string(),
                },
                ConversationMessage {
                    role: PromptContextRole::Assistant,
                    content: "working on it".to_string(),
                },
            ],
            environment: [("API_KEY".to_string(), "secret123".to_string())]
                .into_iter()
                .collect(),
            user_id: "user-42".to_string(),
            metadata: [("priority".to_string(), "high".to_string())]
                .into_iter()
                .collect(),
        };

        let json = serde_json::to_string(&ctx).unwrap();
        let deserialized: TaskContext = serde_json::from_str(&json).unwrap();

        assert_eq!(
            deserialized.project_dir,
            Some("/workspace/myproject".to_string())
        );
        assert_eq!(deserialized.conversation_history.len(), 2);
        assert_eq!(
            deserialized.conversation_history[0].role,
            PromptContextRole::User
        );
        assert_eq!(deserialized.conversation_history[0].content, "fix the bug");
        assert_eq!(
            deserialized.environment.get("API_KEY"),
            Some(&"secret123".to_string())
        );
        assert_eq!(deserialized.user_id, "user-42");
        assert_eq!(
            deserialized.metadata.get("priority"),
            Some(&"high".to_string())
        );
    }

    #[test]
    fn task_context_backward_compat() {
        let json = r#"{}"#;
        let ctx: TaskContext = serde_json::from_str(json).unwrap();

        assert_eq!(ctx.project_dir, None);
        assert!(ctx.conversation_history.is_empty());
        assert!(ctx.environment.is_empty());
        assert_eq!(ctx.user_id, "");

        assert!(ctx.metadata.is_empty());
    }

    #[test]
    fn task_context_omits_none_fields_on_wire() {
        // The TS worker bridges (kimi/opencode) distinguish an absent JSON key
        // from an explicit `null`: kimi's payload validator rejects
        // `project_dir: null` ("context.project_dir must be a string") but
        // accepts an absent key. Regression: serializing a default TaskContext
        // must not emit `"project_dir": null`.
        let ctx = TaskContext::default();
        let json = serde_json::to_string(&ctx).unwrap();
        assert!(
            !json.contains("project_dir"),
            "project_dir must be omitted when None, got: {json}"
        );
        assert!(
            !json.contains("null"),
            "no field may serialize as JSON null, got: {json}"
        );

        // Round-trip stays lossless.
        let deserialized: TaskContext = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.project_dir, None);
    }

    #[test]
    fn task_status_enum_serde() {
        assert_eq!(
            serde_json::to_string(&ExternalTaskStatus::Success).unwrap(),
            "\"success\""
        );
        assert_eq!(
            serde_json::to_string(&ExternalTaskStatus::Failed).unwrap(),
            "\"failed\""
        );
        assert_eq!(
            serde_json::to_string(&ExternalTaskStatus::Cancelled).unwrap(),
            "\"cancelled\""
        );
        assert_eq!(
            serde_json::to_string(&ExternalTaskStatus::TimedOut).unwrap(),
            "\"timed_out\""
        );
        let partial_json =
            serde_json::to_string(&ExternalTaskStatus::Partial("wip".to_string())).unwrap();
        assert!(partial_json.contains("partial"));
        assert!(partial_json.contains("wip"));

        let success: ExternalTaskStatus = serde_json::from_str("\"success\"").unwrap();
        assert_eq!(success, ExternalTaskStatus::Success);

        let failed: ExternalTaskStatus = serde_json::from_str("\"failed\"").unwrap();
        assert_eq!(failed, ExternalTaskStatus::Failed);
        let cancelled: ExternalTaskStatus = serde_json::from_str("\"cancelled\"").unwrap();
        assert_eq!(cancelled, ExternalTaskStatus::Cancelled);
        let timed_out: ExternalTaskStatus = serde_json::from_str("\"timed_out\"").unwrap();
        assert_eq!(timed_out, ExternalTaskStatus::TimedOut);
    }

    #[test]
    fn task_status_enum_matching() {
        let success = ExternalTaskStatus::Success;
        let failed = ExternalTaskStatus::Failed;
        let cancelled = ExternalTaskStatus::Cancelled;

        assert!(matches!(success, ExternalTaskStatus::Success));
        assert!(!matches!(failed, ExternalTaskStatus::Success));
        assert!(!matches!(cancelled, ExternalTaskStatus::Success));
    }

    #[test]
    fn load_balancer_round_robin() {
        use crate::config::WorkerEndpoint;

        let endpoints = vec![
            WorkerEndpoint {
                url: "ws://a:9090".to_string(),
                auth_token: None,
                weight: None,
            },
            WorkerEndpoint {
                url: "ws://b:9090".to_string(),
                auth_token: None,
                weight: None,
            },
            WorkerEndpoint {
                url: "ws://c:9090".to_string(),
                auth_token: None,
                weight: None,
            },
        ];
        let lb = LoadBalancer::new(endpoints, LoadBalanceStrategy::RoundRobin);

        assert_eq!(lb.acquire().url, "ws://a:9090");
        assert_eq!(lb.acquire().url, "ws://b:9090");
        assert_eq!(lb.acquire().url, "ws://c:9090");
        assert_eq!(lb.acquire().url, "ws://a:9090");
        assert_eq!(lb.acquire().url, "ws://b:9090");
        assert_eq!(lb.acquire().url, "ws://c:9090");
    }

    #[test]
    fn load_balancer_single_endpoint() {
        use crate::config::WorkerEndpoint;

        let endpoints = vec![WorkerEndpoint {
            url: "ws://only:9090".to_string(),
            auth_token: None,
            weight: None,
        }];
        let lb = LoadBalancer::new(endpoints, LoadBalanceStrategy::RoundRobin);

        for _ in 0..10 {
            assert_eq!(lb.acquire().url, "ws://only:9090");
        }
    }

    #[test]
    fn load_balancer_least_connections_picks_least_loaded() {
        // Regression for H3: LeastConnections must select by in-flight count,
        // not silently round-robin.
        // Noted in audit document for Jun 23 2026
        use crate::config::WorkerEndpoint;

        let endpoints = vec![
            WorkerEndpoint {
                url: "ws://a".to_string(),
                auth_token: None,
                weight: None,
            },
            WorkerEndpoint {
                url: "ws://b".to_string(),
                auth_token: None,
                weight: None,
            },
            WorkerEndpoint {
                url: "ws://c".to_string(),
                auth_token: None,
                weight: None,
            },
        ];
        let lb = LoadBalancer::new(endpoints, LoadBalanceStrategy::LeastConnections);

        // All zero: tie resolves to lowest index (a).
        let l0 = lb.acquire();
        assert_eq!(l0.url, "ws://a");
        assert_eq!(lb.active_for(0), 1);

        // a=1, b=0, c=0 -> pick b (lowest of the zeros).
        let l1 = lb.acquire();
        assert_eq!(l1.url, "ws://b");
        assert_eq!(lb.active_for(1), 1);

        // a=1, b=1, c=0 -> pick c.
        let l2 = lb.acquire();
        assert_eq!(l2.url, "ws://c");
        assert_eq!(lb.active_for(2), 1);

        // a=1, b=1, c=1 -> tie, lowest index a.
        let l3 = lb.acquire();
        assert_eq!(l3.url, "ws://a");
        assert_eq!(lb.active_for(0), 2);
        assert_eq!(lb.active_for(1), 1);
        assert_eq!(lb.active_for(2), 1);
    }

    #[test]
    fn load_balancer_lease_release_on_drop() {
        // Holding a lease bumps the count; dropping it restores the count so the
        // next LeastConnections pick returns to the released endpoint.
        use crate::config::WorkerEndpoint;

        let endpoints = vec![
            WorkerEndpoint {
                url: "ws://a".to_string(),
                auth_token: None,
                weight: None,
            },
            WorkerEndpoint {
                url: "ws://b".to_string(),
                auth_token: None,
                weight: None,
            },
        ];
        let lb = LoadBalancer::new(endpoints, LoadBalanceStrategy::LeastConnections);

        // Pin a load on endpoint a.
        let held = lb.acquire();
        assert_eq!(held.url, "ws://a");
        assert_eq!(lb.active_for(0), 1);

        // While a is loaded, b is preferred.
        let _b = lb.acquire();
        assert_eq!(_b.url, "ws://b");
        assert_eq!(lb.active_for(1), 1);

        // Dropping the a-lease makes a least-loaded again.
        drop(held);
        assert_eq!(lb.active_for(0), 0);

        let next = lb.acquire();
        assert_eq!(next.url, "ws://a");
        assert_eq!(lb.active_for(0), 1);
    }

    #[test]
    fn load_balancer_strategies_diverge() {
        // Concrete demonstration that H3 is fixed: the same acquire sequence
        // yields different endpoints under the two strategies.
        use crate::config::WorkerEndpoint;

        let mk = || {
            vec![
                WorkerEndpoint {
                    url: "ws://a".to_string(),
                    auth_token: None,
                    weight: None,
                },
                WorkerEndpoint {
                    url: "ws://b".to_string(),
                    auth_token: None,
                    weight: None,
                },
            ]
        };

        // RoundRobin: a, b, a, b ...
        let rr = LoadBalancer::new(mk(), LoadBalanceStrategy::RoundRobin);
        assert_eq!(rr.acquire().url, "ws://a");
        assert_eq!(rr.acquire().url, "ws://b");
        assert_eq!(rr.acquire().url, "ws://a");

        // LeastConnections with leases held: a, b, b, b ... (a stays loaded, so
        // every subsequent pick prefers the least-loaded b).
        let lc = LoadBalancer::new(mk(), LoadBalanceStrategy::LeastConnections);
        let _hold_a = lc.acquire();
        assert_eq!(_hold_a.url, "ws://a");
        assert_eq!(lc.acquire().url, "ws://b");
        assert_eq!(lc.acquire().url, "ws://b");
    }

    #[test]
    fn endpoint_lease_is_send_sync() {
        // EndpointLease must be Send+Sync so it can move into tokio::spawn on
        // the fire-and-forget path and decrement across threads on drop.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<EndpointLease>();
    }

    #[test]
    fn build_task_context_populates_fields() {
        let env: HashMap<String, String> = [("API_KEY".to_string(), "secret".to_string())]
            .into_iter()
            .collect();
        let history = vec![ConversationMessage {
            role: PromptContextRole::User,
            content: "do the thing".to_string(),
        }];
        let meta: HashMap<String, String> = [("priority".to_string(), "high".to_string())]
            .into_iter()
            .collect();

        let ctx = build_task_context("user-1", Some("/workspace"), env, history, meta);

        assert_eq!(ctx.user_id, "user-1");
        assert_eq!(ctx.project_dir.as_deref(), Some("/workspace"));
        assert_eq!(ctx.environment.get("API_KEY").unwrap(), "secret");
        assert_eq!(ctx.conversation_history.len(), 1);
        assert_eq!(ctx.conversation_history[0].content, "do the thing");
        assert_eq!(ctx.metadata.get("priority").unwrap(), "high");
    }

    #[test]
    fn build_task_context_defaults() {
        let ctx = build_task_context("u", None, HashMap::new(), vec![], HashMap::new());

        assert_eq!(ctx.user_id, "u");
        assert!(ctx.project_dir.is_none());
        assert!(ctx.environment.is_empty());
        assert!(ctx.conversation_history.is_empty());
        assert!(ctx.metadata.is_empty());
    }

    #[tokio::test]
    async fn pool_try_acquire_empty_returns_none() {
        let pool = WorkerConnectionPool::new(2, Duration::from_secs(60));
        assert!(
            pool.try_acquire("nanocode:ws://localhost:9090")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn pool_evict_stale_removes_old() {
        let pool = WorkerConnectionPool::new(2, Duration::from_millis(1));
        // Pool is empty, evict should be a no-op
        pool.evict_stale().await;
        assert_eq!(pool.pool_size().await, 0);
    }

    #[tokio::test]
    async fn pool_drain_empties_all() {
        let pool = WorkerConnectionPool::new(2, Duration::from_secs(300));
        pool.drain().await;
        assert_eq!(pool.pool_size().await, 0);
    }

    #[test]
    fn manager_initializes_load_balancers() {
        use crate::config::WorkerEndpoint;

        let mgr = ExternalWorkerManager::new(vec![ExternalWorkerConfig {
            name: "multi".to_string(),
            url: "ws://fallback:9090".to_string(),
            timeout_ms: 300_000,
            endpoints: vec![
                WorkerEndpoint {
                    url: "ws://a:9090".to_string(),
                    auth_token: None,
                    weight: None,
                },
                WorkerEndpoint {
                    url: "ws://b:9090".to_string(),
                    auth_token: None,
                    weight: None,
                },
            ],
            ..Default::default()
        }]);

        let lb = mgr.load_balancers.get("multi").unwrap();
        assert_eq!(lb.endpoint_count(), 2);
        assert_eq!(lb.acquire().url, "ws://a:9090");
        assert_eq!(lb.acquire().url, "ws://b:9090");
        assert_eq!(lb.acquire().url, "ws://a:9090");
    }

    #[test]
    fn cleartext_classification_is_userinfo_safe_and_fails_closed() {
        // Loopback cleartext stays allowed (local worker containers).
        assert!(!is_cleartext_non_loopback("ws://127.0.0.1:9090/ws/agent"));
        assert!(!is_cleartext_non_loopback("ws://localhost/path"));
        assert!(!is_cleartext_non_loopback("ws://[::1]:9000/kimi"));
        // TLS is never "cleartext".
        assert!(!is_cleartext_non_loopback(
            "wss://worker.example.com:443/ws"
        ));
        // Remote cleartext strips the allowlist.
        assert!(is_cleartext_non_loopback("ws://worker.example.com/ws"));
        assert!(is_cleartext_non_loopback("ws://192.168.1.100:9000"));
        // MR !1521 DD-9: userinfo must not spoof a loopback classification —
        // this URL connects to agent-hub.example, not localhost.
        assert!(is_cleartext_non_loopback(
            "ws://localhost:9000@agent-hub.example/kimi"
        ));
        // Fail closed: unparseable or authority-less cleartext URLs count as
        // non-loopback rather than silently keeping tool authority.
        assert!(is_cleartext_non_loopback("ws://"));
        assert!(is_cleartext_non_loopback("ws://[::1"));
    }

    #[test]
    fn is_loopback_detects_local_hosts() {
        assert!(is_loopback("127.0.0.1"));
        assert!(is_loopback("localhost"));
        assert!(is_loopback("::1"));
        assert!(is_loopback("[::1]"));
        assert!(!is_loopback("worker.example.com"));
        assert!(!is_loopback("192.168.1.100"));
    }
}
