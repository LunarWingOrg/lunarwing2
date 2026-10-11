//! Internal HTTP API for worker-to-orchestrator communication.
//!
//! This runs on a separate port (default 50051) from the web gateway.
//! All endpoints are authenticated via per-job bearer tokens.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, broadcast};
use uuid::Uuid;

use crate::channels::web::types::SseEvent;
use crate::db::Database;
use crate::llm::{CompletionRequest, LlmProvider, ToolCompletionRequest};
use crate::orchestrator::auth::{TokenStore, worker_auth_middleware};
use crate::orchestrator::job_manager::ContainerJobManager;
use crate::secrets::SecretsStore;
use crate::worker::api::JobEventPayload;
use crate::worker::api::{
    CompletionReport, CredentialResponse, JobDescription, McpCredentialResponse,
    ProxyCompletionRequest, ProxyCompletionResponse, ProxyToolCompletionRequest,
    ProxyToolCompletionResponse, StatusUpdate,
};

/// A follow-up prompt queued for a Claude Code bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingPrompt {
    pub content: String,
    pub done: bool,
}

/// Shared state for the orchestrator API.
#[derive(Clone)]
pub struct OrchestratorState {
    pub llm: Arc<dyn LlmProvider>,
    pub job_manager: Arc<ContainerJobManager>,
    pub token_store: TokenStore,
    /// Broadcast channel for job events (consumed by the web gateway SSE).
    /// Tuple: (job_id, user_id, event).
    pub job_event_tx: Option<broadcast::Sender<(Uuid, String, SseEvent)>>,
    /// Buffered follow-up prompts for sandbox jobs, keyed by job_id.
    pub prompt_queue: Arc<Mutex<HashMap<Uuid, VecDeque<PendingPrompt>>>>,
    /// Database handle for persisting job events.
    pub store: Option<Arc<dyn Database>>,
    /// Encrypted secrets store for credential injection into containers.
    pub secrets_store: Option<Arc<dyn SecretsStore + Send + Sync>>,
    /// Legacy fallback owner for completion event routing.
    pub user_id: String,
    /// In-memory cache of job_id → user_id for SSE scoping. Populated when
    /// sandbox jobs are created, avoiding a DB round-trip on every job event.
    pub job_owner_cache: Arc<std::sync::RwLock<HashMap<Uuid, String>>>,
    /// Direct handle to the in-memory job state tracker. Used by
    /// `report_complete` to guarantee the job transitions out of
    /// `InProgress` even if the broadcast is missed by the monitor.
    pub context_manager: Option<Arc<crate::context::ContextManager>>,
}

/// The orchestrator's internal API server.
pub struct OrchestratorApi;

impl OrchestratorApi {
    /// Build the axum router for the internal API.
    pub fn router(state: OrchestratorState) -> Router {
        Router::new()
            // Worker routes: authenticated via route_layer middleware.
            .route("/worker/{job_id}/job", get(get_job))
            .route("/worker/{job_id}/llm/complete", post(llm_complete))
            .route(
                "/worker/{job_id}/llm/complete_with_tools",
                post(llm_complete_with_tools),
            )
            .route("/worker/{job_id}/status", post(report_status))
            .route("/worker/{job_id}/complete", post(report_complete))
            .route("/worker/{job_id}/event", post(job_event_handler))
            .route("/worker/{job_id}/prompt", get(get_prompt_handler))
            .route("/worker/{job_id}/credentials", get(get_credentials_handler))
            .route(
                "/worker/{job_id}/harness-secrets",
                get(get_harness_secrets_handler),
            )
            .route(
                "/worker/{job_id}/mcp/credentials",
                get(get_mcp_credentials_handler),
            )
            .route_layer(axum::middleware::from_fn_with_state(
                state.token_store.clone(),
                worker_auth_middleware,
            ))
            // Unauthenticated routes (added after the layer).
            .route("/health", get(health_check))
            .with_state(state)
    }

    /// Start the internal API server on the given port.
    ///
    /// On macOS/Windows (Docker Desktop), binds to loopback only because
    /// Docker Desktop routes `host.docker.internal` through its VM to the
    /// host's `127.0.0.1`.
    ///
    /// On Linux, containers reach the host via the docker bridge gateway
    /// (`172.17.0.1`), which is NOT loopback. Binding to `127.0.0.1`
    /// would reject container traffic. We bind to all interfaces instead
    /// and rely on `worker_auth_middleware` (applied as a route_layer on
    /// every `/worker/` endpoint) to reject unauthenticated requests.
    pub async fn start(
        state: OrchestratorState,
        port: u16,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let router = Self::router(state);
        let addr = if cfg!(target_os = "linux") {
            std::net::SocketAddr::from(([0, 0, 0, 0], port))
        } else {
            std::net::SocketAddr::from(([127, 0, 0, 1], port))
        };

        tracing::info!("Orchestrator internal API listening on {}", addr);

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, router).await?;

        Ok(())
    }
}

// -- Handlers --
//
// All /worker/ handlers below are behind the worker_auth_middleware route_layer,
// so they don't need to validate tokens themselves.

async fn health_check() -> &'static str {
    "ok"
}

async fn get_job(
    State(state): State<OrchestratorState>,
    Path(job_id): Path<Uuid>,
) -> Result<Json<JobDescription>, StatusCode> {
    let handle = state
        .job_manager
        .get_handle(job_id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(JobDescription {
        title: format!("Job {}", job_id),
        description: handle.task_description,
        project_dir: handle.project_dir.map(|p| p.display().to_string()),
        owner_id: handle.owner_id,
        mcp_servers: handle.mcp_servers,
    }))
}

async fn llm_complete(
    State(state): State<OrchestratorState>,
    Path(job_id): Path<Uuid>,
    Json(req): Json<ProxyCompletionRequest>,
) -> Result<Json<ProxyCompletionResponse>, StatusCode> {
    let completion_req = CompletionRequest {
        messages: req.messages,
        model: req.model,
        max_tokens: req.max_tokens,
        temperature: req.temperature,
        stop_sequences: req.stop_sequences,
        metadata: std::collections::HashMap::new(),
    };

    let resp = state.llm.complete(completion_req).await.map_err(|e| {
        tracing::error!("LLM completion failed for job {}: {}", job_id, e);
        StatusCode::BAD_GATEWAY
    })?;

    Ok(Json(ProxyCompletionResponse {
        content: resp.content,
        input_tokens: resp.input_tokens,
        output_tokens: resp.output_tokens,
        finish_reason: format_finish_reason(resp.finish_reason),
        cache_read_input_tokens: resp.cache_read_input_tokens,
        cache_creation_input_tokens: resp.cache_creation_input_tokens,
    }))
}

async fn llm_complete_with_tools(
    State(state): State<OrchestratorState>,
    Path(job_id): Path<Uuid>,
    Json(req): Json<ProxyToolCompletionRequest>,
) -> Result<Json<ProxyToolCompletionResponse>, StatusCode> {
    let tool_req = ToolCompletionRequest {
        messages: req.messages,
        tools: req.tools,
        model: req.model,
        max_tokens: req.max_tokens,
        temperature: req.temperature,
        stop_sequences: req.stop_sequences,
        tool_choice: req.tool_choice,
        metadata: std::collections::HashMap::new(),
    };

    let resp = state.llm.complete_with_tools(tool_req).await.map_err(|e| {
        tracing::error!("LLM tool completion failed for job {}: {}", job_id, e);
        StatusCode::BAD_GATEWAY
    })?;

    Ok(Json(ProxyToolCompletionResponse {
        content: resp.content,
        tool_calls: resp.tool_calls,
        input_tokens: resp.input_tokens,
        output_tokens: resp.output_tokens,
        finish_reason: format_finish_reason(resp.finish_reason),
        cache_read_input_tokens: resp.cache_read_input_tokens,
        cache_creation_input_tokens: resp.cache_creation_input_tokens,
    }))
}

async fn report_status(
    State(state): State<OrchestratorState>,
    Path(job_id): Path<Uuid>,
    Json(update): Json<StatusUpdate>,
) -> Result<StatusCode, StatusCode> {
    tracing::debug!(
        job_id = %job_id,
        state = %update.state,
        iteration = update.iteration,
        "Worker status update"
    );

    state
        .job_manager
        .update_worker_status(job_id, update.message, update.iteration)
        .await;

    Ok(StatusCode::OK)
}

async fn report_complete(
    State(state): State<OrchestratorState>,
    Path(job_id): Path<Uuid>,
    Json(report): Json<CompletionReport>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let owner_id = state
        .job_manager
        .get_handle(job_id)
        .await
        .map(|handle| handle.owner_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    if report.success {
        tracing::info!(
            job_id = %job_id,
            "Worker reported job complete"
        );
    } else {
        tracing::warn!(
            job_id = %job_id,
            message = ?report.message,
            "Worker reported job failure"
        );
    }

    // Store the result and clean up the container
    let result = crate::orchestrator::job_manager::CompletionResult {
        success: report.success,
        message: report.message.clone(),
    };
    if let Err(e) = state.job_manager.complete_job(job_id, result).await {
        tracing::error!(job_id = %job_id, "Failed to complete job cleanup: {}", e);
    }
    let status = completion_report_status(&report);
    if let Some(ref store) = state.store
        && let Err(e) = store
            .update_sandbox_job_status(
                job_id,
                status,
                Some(report.success),
                report.message.as_deref(),
                None,
                Some(Utc::now()),
            )
            .await
    {
        tracing::warn!(
            job_id = %job_id,
            status = status,
            "Failed to persist sandbox job completion status: {}",
            e
        );
    }

    // Broadcast an SSE JobResult so the job monitor can transition the
    // in-memory ContextManager state. Without this, monitors waiting on
    // event_rx.recv() would block indefinitely when the worker reports
    // completion via /complete instead of /event.
    if let Some(ref tx) = state.job_event_tx {
        let _ = tx.send((
            job_id,
            owner_id,
            SseEvent::JobResult {
                job_id: job_id.to_string(),
                status: status.to_string(),
                session_id: None,
                fallback_deliverable: None,
            },
        ));
    }

    // Directly update ContextManager so the job transitions out of
    // InProgress even if the broadcast is missed by the monitor.
    if let Some(ref cm) = state.context_manager {
        let (target, reason) = match status {
            "completed" => (crate::context::JobState::Completed, None),
            "stuck" => (
                crate::context::JobState::Stuck,
                Some("Container is awaiting approval or manual attention".to_string()),
            ),
            other => (
                crate::context::JobState::Failed,
                Some(format!("Container finished: {}", other)),
            ),
        };
        let _ = cm
            .update_context(job_id, |ctx| {
                let _ = ctx.transition_to(target, reason);
            })
            .await;
    }

    Ok(Json(serde_json::json!({"status": "ok"})))
}

fn completion_report_status(report: &CompletionReport) -> &'static str {
    if report.success {
        "completed"
    } else if completion_requires_attention(report.message.as_deref()) {
        "stuck"
    } else {
        "failed"
    }
}

fn completion_requires_attention(message: Option<&str>) -> bool {
    let Some(message) = message else {
        return false;
    };

    let message = message.to_ascii_lowercase();
    message.contains("awaiting approval")
        || message.contains("requires approval")
        || message.contains("not available for autonomous execution")
        || message.contains("not currently available for owner")
}

fn job_result_status(data: &serde_json::Value) -> String {
    if let Some(status) = data.get("status").and_then(|v| v.as_str()) {
        status.to_string()
    } else if let Some(success) = data.get("success").and_then(|v| v.as_bool()) {
        if success {
            "completed".to_string()
        } else if completion_requires_attention(data.get("message").and_then(|v| v.as_str())) {
            "stuck".to_string()
        } else {
            "error".to_string()
        }
    } else {
        "unknown".to_string()
    }
}

// -- Sandbox job event handlers --

/// Receive a job event from a worker or Claude Code bridge and broadcast + persist it.
async fn job_event_handler(
    State(state): State<OrchestratorState>,
    Path(job_id): Path<Uuid>,
    Json(payload): Json<JobEventPayload>,
) -> Result<StatusCode, StatusCode> {
    tracing::debug!(
        job_id = %job_id,
        event_type = %payload.event_type,
        "Job event received"
    );

    // CV-7 (WI#326): value-level redaction. A job can always `cat` its own
    // mounted secret files; the exact/base64/URL-encoded values are scrubbed
    // here at ingest so they never reach persisted events, SSE, or the
    // parent agent's context. (The worker scrubs before sending too; this is
    // the daemon-side backstop.)
    let mut payload = payload;
    if let Some(redactor) = state.token_store.harness_secret_redactor(job_id).await {
        redactor.scrub_json(&mut payload.data);
    }

    // Persist to DB (fire-and-forget)
    if let Some(ref store) = state.store {
        let store = Arc::clone(store);
        let event_type = payload.event_type.clone();
        let data = payload.data.clone();
        tokio::spawn(async move {
            if let Err(e) = store.save_job_event(job_id, &event_type, &data).await {
                tracing::warn!(job_id = %job_id, "Failed to persist job event: {}", e);
            }
        });
    }

    // Convert to SSE event and broadcast
    let job_id_str = job_id.to_string();
    let sse_event = match payload.event_type.as_str() {
        "message" => SseEvent::JobMessage {
            job_id: job_id_str,
            role: payload
                .data
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("assistant")
                .to_string(),
            content: payload
                .data
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        },
        "tool_use" => SseEvent::JobToolUse {
            job_id: job_id_str,
            tool_name: payload
                .data
                .get("tool_name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
            input: payload
                .data
                .get("input")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        },
        "tool_result" => SseEvent::JobToolResult {
            job_id: job_id_str,
            tool_name: payload
                .data
                .get("tool_name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
            output: payload
                .data
                .get("output")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        },
        "result" => SseEvent::JobResult {
            job_id: job_id_str,
            status: job_result_status(&payload.data),
            session_id: payload
                .data
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            // NOTE: `fallback_deliverable` is currently always None in SSE events.
            // In-memory jobs store fallback data in JobContext.metadata (accessed via job_status tool).
            // Sandbox containers don't yet emit fallback data in their event payloads.
            // This field is forward-compatible infrastructure for when container workers
            // gain context/memory tracking capabilities.
            fallback_deliverable: payload.data.get("fallback_deliverable").cloned(),
        },
        _ => SseEvent::JobStatus {
            job_id: job_id_str,
            message: payload
                .data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        },
    };

    // Broadcast via the channel (if configured).
    // Look up the job owner from the in-memory cache (populated at job creation).
    if let Some(ref tx) = state.job_event_tx {
        let cached_uid = state
            .job_owner_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&job_id)
            .cloned();

        let user_id = match cached_uid {
            Some(uid) => uid,
            None => {
                // Cache miss: fall back to DB lookup and populate cache.
                let uid = match state.store.as_ref() {
                    Some(store) => store
                        .get_sandbox_job(job_id)
                        .await
                        .ok()
                        .flatten()
                        .map(|j| j.user_id),
                    None => None,
                };
                if let Some(ref uid) = uid {
                    state
                        .job_owner_cache
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(job_id, uid.clone());
                }
                uid.unwrap_or_default()
            }
        };

        if user_id.is_empty() {
            let _ = tx.send((job_id, String::new(), sse_event));
        } else {
            let _ = tx.send((job_id, user_id, sse_event));
        }
    }

    Ok(StatusCode::OK)
}

/// Return the next queued follow-up prompt for a Claude Code bridge.
/// Returns 204 No Content if no prompt is available.
async fn get_prompt_handler(
    State(state): State<OrchestratorState>,
    Path(job_id): Path<Uuid>,
) -> Result<(StatusCode, Json<serde_json::Value>), StatusCode> {
    let mut queue = state.prompt_queue.lock().await;
    if let Some(prompts) = queue.get_mut(&job_id)
        && let Some(prompt) = prompts.pop_front()
    {
        return Ok((
            StatusCode::OK,
            Json(serde_json::json!({
                "content": prompt.content,
                "done": prompt.done,
            })),
        ));
    }

    // Return 204 with an empty body. The Json wrapper requires some value
    // but the status code signals "nothing here".
    Ok((StatusCode::NO_CONTENT, Json(serde_json::Value::Null)))
}

/// Serve decrypted credentials for a job's granted secrets.
///
/// Returns 204 if no grants exist, 503 if no secrets store is configured,
/// or a JSON array of `{ env_var, value }` pairs.
async fn get_credentials_handler(
    State(state): State<OrchestratorState>,
    Path(job_id): Path<Uuid>,
) -> Result<(StatusCode, Json<serde_json::Value>), StatusCode> {
    let handle = state
        .job_manager
        .get_handle(job_id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    let grants = match state.token_store.get_grants(job_id).await {
        Some(g) if !g.is_empty() => g,
        _ => return Ok((StatusCode::NO_CONTENT, Json(serde_json::Value::Null))),
    };

    let secrets = state.secrets_store.as_ref().ok_or_else(|| {
        tracing::error!("Credentials requested but no secrets store configured");
        StatusCode::SERVICE_UNAVAILABLE
    })?;

    let mut credentials: Vec<CredentialResponse> = Vec::with_capacity(grants.len());

    for grant in &grants {
        let decrypted = secrets
            .get_decrypted(&handle.owner_id, &grant.secret_name)
            .await
            .map_err(|_| {
                tracing::error!(
                    job_id = %job_id,
                    "Failed to decrypt credential grant"
                );
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

        // Record usage for audit trail
        if let Ok(secret) = secrets.get(&handle.owner_id, &grant.secret_name).await
            && let Err(e) = secrets.record_usage(secret.id).await
        {
            tracing::warn!(
                job_id = %job_id,
                "Failed to record credential usage: {}", e
            );
        }

        tracing::debug!(
            job_id = %job_id,
            env_var = %grant.env_var,
            "Serving credential to container"
        );

        credentials.push(CredentialResponse {
            env_var: grant.env_var.clone(),
            value: decrypted.expose().to_string(),
        });
    }

    Ok((
        StatusCode::OK,
        Json(serde_json::to_value(&credentials).unwrap_or(serde_json::Value::Null)),
    ))
}

/// Serve harness-mode secret values for a job (WI#326 CV-2 worker pull).
///
/// Same per-job bearer posture as `/credentials` (route_layer middleware).
/// Values were resolved host-side at dispatch and are held in the token
/// store, so no secrets-store access happens per request; the worker writes
/// them into the tmpfs mount itself and fails closed on a set mismatch.
/// Returns 204 when the job mounts no harness secrets.
async fn get_harness_secrets_handler(
    State(state): State<OrchestratorState>,
    Path(job_id): Path<Uuid>,
) -> Result<(StatusCode, Json<serde_json::Value>), StatusCode> {
    // The job handle must exist (same as /credentials).
    state
        .job_manager
        .get_handle(job_id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    let secrets = match state.token_store.harness_secrets(job_id).await {
        Some(secrets) if !secrets.is_empty() => secrets,
        _ => return Ok((StatusCode::NO_CONTENT, Json(serde_json::Value::Null))),
    };
    let files: Vec<serde_json::Value> = secrets
        .iter()
        .map(|(file_name, value)| {
            serde_json::json!({
                "file_name": file_name,
                "value": secrecy::ExposeSecret::expose_secret(value),
            })
        })
        .collect();
    tracing::debug!(
        job_id = %job_id,
        count = files.len(),
        "Serving harness secret files to container"
    );
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({ "secrets": files })),
    ))
}

/// Serve owner-scoped MCP credentials without exposing them to generic workers.
async fn get_mcp_credentials_handler(
    State(state): State<OrchestratorState>,
    Path(job_id): Path<Uuid>,
) -> Result<(StatusCode, Json<serde_json::Value>), StatusCode> {
    let handle = state
        .job_manager
        .get_handle(job_id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    let grants = match state.token_store.get_mcp_grants(job_id).await {
        Some(grants) if !grants.is_empty() => grants,
        _ => return Ok((StatusCode::NO_CONTENT, Json(serde_json::Value::Null))),
    };
    let secrets = state.secrets_store.as_ref().ok_or_else(|| {
        tracing::error!(job_id = %job_id, "MCP credentials requested without secrets store");
        StatusCode::SERVICE_UNAVAILABLE
    })?;
    let mut credentials = Vec::with_capacity(grants.len());
    for grant in grants {
        let decrypted = secrets
            .get_decrypted(&handle.owner_id, &grant.secret_name)
            .await
            .map_err(|_| {
                tracing::error!(job_id = %job_id, "Failed to decrypt MCP credential grant");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        if let Ok(secret) = secrets.get(&handle.owner_id, &grant.secret_name).await
            && let Err(error) = secrets.record_usage(secret.id).await
        {
            tracing::warn!(job_id = %job_id, "Failed to record MCP credential usage: {error}");
        }
        credentials.push(McpCredentialResponse {
            server_name: grant.server_name,
            env_var: grant.env_var,
            value: decrypted.expose().to_string(),
        });
    }
    Ok((
        StatusCode::OK,
        Json(serde_json::to_value(credentials).unwrap_or(serde_json::Value::Null)),
    ))
}

/// One finish-reason spelling everywhere: the canonical protocol strings
/// shared with streamed terminal metadata (`LlmStreamChunk::Done`).
fn format_finish_reason(reason: crate::llm::FinishReason) -> String {
    reason.as_str().to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::orchestrator::auth::TokenStore;
    use crate::orchestrator::job_manager::{ContainerJobConfig, ContainerJobManager};
    use crate::testing::StubLlm;

    use super::*;

    #[test]
    fn format_finish_reason_uses_canonical_stream_spellings() {
        // The proxy must spell finish reasons exactly like streamed terminal
        // metadata, so workers and streaming consumers parse one vocabulary.
        assert_eq!(format_finish_reason(crate::llm::FinishReason::Stop), "stop");
        assert_eq!(
            format_finish_reason(crate::llm::FinishReason::Length),
            "length"
        );
        assert_eq!(
            format_finish_reason(crate::llm::FinishReason::ToolUse),
            "tool_calls"
        );
        assert_eq!(
            format_finish_reason(crate::llm::FinishReason::ContentFilter),
            "content_filter"
        );
        assert_eq!(
            format_finish_reason(crate::llm::FinishReason::Unknown),
            "unknown"
        );
    }

    fn test_state() -> OrchestratorState {
        let token_store = TokenStore::new();
        let jm = ContainerJobManager::new(ContainerJobConfig::default(), token_store.clone());
        OrchestratorState {
            llm: Arc::new(StubLlm::default()),
            job_manager: Arc::new(jm),
            token_store,
            job_event_tx: None,
            prompt_queue: Arc::new(Mutex::new(HashMap::new())),
            store: None,
            secrets_store: None,
            user_id: "default".to_string(),
            job_owner_cache: Arc::new(std::sync::RwLock::new(HashMap::new())),
            context_manager: None,
        }
    }

    async fn insert_test_handle(state: &OrchestratorState, job_id: Uuid, owner_id: &str) {
        state.job_manager.containers.write().await.insert(
            job_id,
            crate::orchestrator::job_manager::ContainerHandle {
                job_id,
                container_id: String::new(),
                state: crate::orchestrator::job_manager::ContainerState::Running,
                mode: crate::orchestrator::job_manager::JobMode::Worker,
                owner_id: owner_id.to_string(),
                mcp_servers: Vec::new(),
                created_at: chrono::Utc::now(),
                project_dir: None,
                task_description: "test".to_string(),
                last_worker_status: None,
                worker_iteration: 0,
                completion_result: None,
                harness: None,
            },
        );
    }

    #[tokio::test]
    async fn health_requires_no_auth() {
        let state = test_state();
        let router = OrchestratorApi::router(state);

        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn worker_route_rejects_missing_token() {
        let state = test_state();
        let router = OrchestratorApi::router(state);

        let job_id = Uuid::new_v4();
        let req = Request::builder()
            .uri(format!("/worker/{}/job", job_id))
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn worker_route_rejects_wrong_token() {
        let state = test_state();
        let router = OrchestratorApi::router(state);

        let job_id = Uuid::new_v4();
        let req = Request::builder()
            .uri(format!("/worker/{}/job", job_id))
            .header("Authorization", "Bearer totally-bogus")
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn worker_route_accepts_valid_token() {
        let state = test_state();
        let job_id = Uuid::new_v4();
        let token = state.token_store.create_token(job_id).await;

        let router = OrchestratorApi::router(state);

        let req = Request::builder()
            .uri(format!("/worker/{}/job", job_id))
            .header("Authorization", format!("Bearer {}", token))
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        // 404 because no container exists for this job_id, but NOT 401.
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn token_for_job_a_rejected_on_job_b() {
        let state = test_state();
        let job_a = Uuid::new_v4();
        let job_b = Uuid::new_v4();
        let token_a = state.token_store.create_token(job_a).await;

        let router = OrchestratorApi::router(state);

        // Use job_a's token to hit job_b's endpoint
        let req = Request::builder()
            .uri(format!("/worker/{}/job", job_b))
            .header("Authorization", format!("Bearer {}", token_a))
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn harness_secrets_endpoint_serves_stored_values_to_token_holder() {
        let state = test_state();
        let job_id = Uuid::new_v4();
        let token = state.token_store.create_token(job_id).await;
        insert_test_handle(&state, job_id, "owner").await;
        // TokenStore is Arc-backed: storing through the clone is visible to
        // the router built from the moved state.
        let token_store = state.token_store.clone();

        let router = OrchestratorApi::router(state);
        // 204 before any secrets are stored.
        let req = Request::builder()
            .uri(format!("/worker/{job_id}/harness-secrets"))
            .header("Authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        token_store
            .store_harness_secrets(
                job_id,
                vec![(
                    "API_KEY".to_string(),
                    secrecy::SecretString::from("super-secret-value".to_string()),
                )],
            )
            .await;

        let req = Request::builder()
            .uri(format!("/worker/{job_id}/harness-secrets"))
            .header("Authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["secrets"][0]["file_name"], "API_KEY");
        assert_eq!(json["secrets"][0]["value"], "super-secret-value");
    }

    #[tokio::test]
    async fn job_event_ingest_redacts_mounted_secret_values() {
        // CV-7: daemon-side backstop — a job that prints its own mounted
        // secret has the value scrubbed before broadcast/persist.
        let mut state = test_state();
        let (tx, mut rx) = broadcast::channel(8);
        state.job_event_tx = Some(tx);
        let job_id = Uuid::new_v4();
        let token = state.token_store.create_token(job_id).await;
        insert_test_handle(&state, job_id, "owner").await;
        let secret_value = "sk-live-abcdef1234567890";
        state
            .token_store
            .store_harness_secrets(
                job_id,
                vec![(
                    "API_KEY".to_string(),
                    secrecy::SecretString::from(secret_value.to_string()),
                )],
            )
            .await;

        let router = OrchestratorApi::router(state);
        let req = Request::builder()
            .method("POST")
            .uri(format!("/worker/{job_id}/event"))
            .header("Authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(format!(
                r#"{{"event_type":"message","data":{{"role":"assistant","content":"leaked {secret_value} here"}}}}"#
            )))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert!(resp.status().is_success());

        let (_, _, event) = rx.recv().await.unwrap();
        let crate::channels::web::types::SseEvent::JobMessage { content, .. } = event else {
            panic!("expected JobMessage event, got {event:?}"); // safety: test
        };
        assert!(
            !content.contains(secret_value),
            "secret value must be redacted from the broadcast event: {content}"
        );
        assert!(content.contains("[redacted-secret]"));
    }

    #[tokio::test]
    async fn completion_broadcast_uses_handle_owner_without_cache() {
        let mut state = test_state();
        state.user_id = "wrong-default".to_string();
        let (event_tx, mut event_rx) = broadcast::channel(4);
        state.job_event_tx = Some(event_tx);
        let job_id = Uuid::new_v4();
        let token = state.token_store.create_token(job_id).await;
        insert_test_handle(&state, job_id, "owner-a").await;
        let router = OrchestratorApi::router(state);
        let request = Request::builder()
            .method("POST")
            .uri(format!("/worker/{job_id}/complete"))
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "success": true,
                    "message": "done",
                    "iterations": 1
                })
                .to_string(),
            ))
            .unwrap();

        assert_eq!(
            router.oneshot(request).await.unwrap().status(),
            StatusCode::OK
        );
        let (reported_job, owner_id, event) = event_rx.recv().await.unwrap();
        assert_eq!(reported_job, job_id);
        assert_eq!(owner_id, "owner-a");
        assert!(matches!(event, SseEvent::JobResult { .. }));
    }

    // -- Prompt queue tests --

    #[tokio::test]
    async fn prompt_returns_204_when_queue_empty() {
        let state = test_state();
        let job_id = Uuid::new_v4();
        let token = state.token_store.create_token(job_id).await;
        let router = OrchestratorApi::router(state);

        let req = Request::builder()
            .uri(format!("/worker/{}/prompt", job_id))
            .header("Authorization", format!("Bearer {}", token))
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn prompt_returns_queued_prompt() {
        let state = test_state();
        let job_id = Uuid::new_v4();
        let token = state.token_store.create_token(job_id).await;

        // Queue a prompt
        {
            let mut q = state.prompt_queue.lock().await;
            q.entry(job_id).or_default().push_back(PendingPrompt {
                content: "What is the status?".to_string(),
                done: false,
            });
        }

        let router = OrchestratorApi::router(state);
        let req = Request::builder()
            .uri(format!("/worker/{}/prompt", job_id))
            .header("Authorization", format!("Bearer {}", token))
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["content"], "What is the status?");
        assert_eq!(json["done"], false);
    }

    // -- Credentials handler tests --

    #[tokio::test]
    async fn credentials_returns_204_when_no_grants() {
        let state = test_state();
        let job_id = Uuid::new_v4();
        let token = state.token_store.create_token(job_id).await;
        insert_test_handle(&state, job_id, "default").await;
        let router = OrchestratorApi::router(state);

        let req = Request::builder()
            .uri(format!("/worker/{}/credentials", job_id))
            .header("Authorization", format!("Bearer {}", token))
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn credentials_returns_503_when_no_secrets_store() {
        let state = test_state();
        let job_id = Uuid::new_v4();
        let token = state.token_store.create_token(job_id).await;
        insert_test_handle(&state, job_id, "default").await;

        // Store grants so we get past the 204 check
        state
            .token_store
            .store_grants(
                job_id,
                vec![crate::orchestrator::auth::CredentialGrant {
                    secret_name: "test_secret".to_string(),
                    env_var: "TEST_SECRET".to_string(),
                }],
            )
            .await;

        let router = OrchestratorApi::router(state);
        let req = Request::builder()
            .uri(format!("/worker/{}/credentials", job_id))
            .header("Authorization", format!("Bearer {}", token))
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        // No secrets_store configured → 503
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn credentials_returns_secrets_when_store_configured() {
        use crate::testing::credentials::test_secrets_store;
        use secrecy::SecretString;
        let secrets_store = Arc::new(test_secrets_store());

        // Create a secret
        secrets_store
            .create(
                "owner-a",
                crate::secrets::CreateSecretParams {
                    name: "test_secret".to_string(),
                    value: SecretString::from("supersecretvalue".to_string()),
                    provider: None,
                    expires_at: None,
                },
            )
            .await
            .unwrap();

        let token_store = TokenStore::new();
        let jm = ContainerJobManager::new(ContainerJobConfig::default(), token_store.clone());
        let job_id = Uuid::new_v4();
        let token = token_store.create_token(job_id).await;
        token_store
            .store_grants(
                job_id,
                vec![crate::orchestrator::auth::CredentialGrant {
                    secret_name: "test_secret".to_string(),
                    env_var: "MY_SECRET".to_string(),
                }],
            )
            .await;

        let state = OrchestratorState {
            llm: Arc::new(StubLlm::default()),
            job_manager: Arc::new(jm),
            token_store,
            job_event_tx: None,
            prompt_queue: Arc::new(Mutex::new(HashMap::new())),
            store: None,
            secrets_store: Some(secrets_store),
            user_id: "default".to_string(),
            job_owner_cache: Arc::new(std::sync::RwLock::new(HashMap::new())),
            context_manager: None,
        };
        insert_test_handle(&state, job_id, "owner-a").await;

        let router = OrchestratorApi::router(state);
        let req = Request::builder()
            .uri(format!("/worker/{}/credentials", job_id))
            .header("Authorization", format!("Bearer {}", token))
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["env_var"], "MY_SECRET");
        assert_eq!(json[0]["value"], "supersecretvalue");
    }

    #[tokio::test]
    async fn mcp_credentials_return_204_without_grants() {
        let state = test_state();
        let job_id = Uuid::new_v4();
        let token = state.token_store.create_token(job_id).await;
        insert_test_handle(&state, job_id, "default").await;
        let router = OrchestratorApi::router(state);
        let req = Request::builder()
            .uri(format!("/worker/{job_id}/mcp/credentials"))
            .header("Authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            router.oneshot(req).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );
    }

    #[tokio::test]
    async fn mcp_credentials_return_503_without_secrets_store() {
        let state = test_state();
        let job_id = Uuid::new_v4();
        let token = state.token_store.create_token(job_id).await;
        state
            .token_store
            .store_mcp_grants(
                job_id,
                vec![crate::orchestrator::auth::McpCredentialGrant {
                    server_name: "example".into(),
                    secret_name: "secret".into(),
                    env_var: "TOKEN".into(),
                }],
            )
            .await;
        insert_test_handle(&state, job_id, "owner").await;
        let router = OrchestratorApi::router(state);
        let req = Request::builder()
            .uri(format!("/worker/{job_id}/mcp/credentials"))
            .header("Authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            router.oneshot(req).await.unwrap().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn mcp_credentials_use_handle_owner() {
        use crate::testing::credentials::test_secrets_store;
        use secrecy::SecretString;

        let secrets_store = Arc::new(test_secrets_store());
        secrets_store
            .create(
                "owner-a",
                crate::secrets::CreateSecretParams {
                    name: "shared_name".to_string(),
                    value: SecretString::from("owner-a-value".to_string()),
                    provider: None,
                    expires_at: None,
                },
            )
            .await
            .unwrap();
        secrets_store
            .create(
                "default",
                crate::secrets::CreateSecretParams {
                    name: "shared_name".to_string(),
                    value: SecretString::from("wrong-default-value".to_string()),
                    provider: None,
                    expires_at: None,
                },
            )
            .await
            .unwrap();
        let mut state = test_state();
        state.secrets_store = Some(secrets_store);
        let job_id = Uuid::new_v4();
        let token = state.token_store.create_token(job_id).await;
        state
            .token_store
            .store_mcp_grants(
                job_id,
                vec![crate::orchestrator::auth::McpCredentialGrant {
                    server_name: "example".to_string(),
                    secret_name: "shared_name".to_string(),
                    env_var: "TOKEN".to_string(),
                }],
            )
            .await;
        insert_test_handle(&state, job_id, "owner-a").await;
        let router = OrchestratorApi::router(state);
        let req = Request::builder()
            .uri(format!("/worker/{job_id}/mcp/credentials"))
            .header("Authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let values: Vec<McpCredentialResponse> = serde_json::from_slice(&body).unwrap();
        assert_eq!(values[0].server_name, "example");
        assert_eq!(values[0].value, "owner-a-value");
    }

    // -- Job event handler tests --

    #[tokio::test]
    async fn job_event_broadcasts_message() {
        let (tx, mut rx) = broadcast::channel(16);
        let token_store = TokenStore::new();
        let jm = ContainerJobManager::new(ContainerJobConfig::default(), token_store.clone());
        let state = OrchestratorState {
            llm: Arc::new(StubLlm::default()),
            job_manager: Arc::new(jm),
            token_store: token_store.clone(),
            job_event_tx: Some(tx),
            prompt_queue: Arc::new(Mutex::new(HashMap::new())),
            store: None,
            secrets_store: None,
            user_id: "default".to_string(),
            job_owner_cache: Arc::new(std::sync::RwLock::new(HashMap::new())),
            context_manager: None,
        };

        let job_id = Uuid::new_v4();
        let token = token_store.create_token(job_id).await;
        let router = OrchestratorApi::router(state);

        let payload = serde_json::json!({
            "event_type": "message",
            "data": {
                "role": "assistant",
                "content": "Hello from worker"
            }
        });

        let req = Request::builder()
            .method("POST")
            .uri(format!("/worker/{}/event", job_id))
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let (recv_id, recv_uid, event) = rx.recv().await.unwrap();
        assert_eq!(recv_id, job_id);
        // No store configured, so user_id falls back to empty string.
        assert_eq!(recv_uid, "");
        match event {
            SseEvent::JobMessage {
                job_id: jid,
                role,
                content,
            } => {
                assert_eq!(jid, job_id.to_string());
                assert_eq!(role, "assistant");
                assert_eq!(content, "Hello from worker");
            }
            other => panic!("Expected JobMessage, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn job_event_handles_tool_use() {
        let (tx, mut rx) = broadcast::channel(16);
        let token_store = TokenStore::new();
        let jm = ContainerJobManager::new(ContainerJobConfig::default(), token_store.clone());
        let state = OrchestratorState {
            llm: Arc::new(StubLlm::default()),
            job_manager: Arc::new(jm),
            token_store: token_store.clone(),
            job_event_tx: Some(tx),
            prompt_queue: Arc::new(Mutex::new(HashMap::new())),
            store: None,
            secrets_store: None,
            user_id: "default".to_string(),
            job_owner_cache: Arc::new(std::sync::RwLock::new(HashMap::new())),
            context_manager: None,
        };

        let job_id = Uuid::new_v4();
        let token = token_store.create_token(job_id).await;
        let router = OrchestratorApi::router(state);

        let payload = serde_json::json!({
            "event_type": "tool_use",
            "data": {
                "tool_name": "shell",
                "input": {"command": "ls"}
            }
        });

        let req = Request::builder()
            .method("POST")
            .uri(format!("/worker/{}/event", job_id))
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let (_recv_id, _recv_uid, event) = rx.recv().await.unwrap();
        match event {
            SseEvent::JobToolUse { tool_name, .. } => {
                assert_eq!(tool_name, "shell");
            }
            other => panic!("Expected JobToolUse, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn job_event_handles_unknown_type() {
        let (tx, mut rx) = broadcast::channel(16);
        let token_store = TokenStore::new();
        let jm = ContainerJobManager::new(ContainerJobConfig::default(), token_store.clone());
        let state = OrchestratorState {
            llm: Arc::new(StubLlm::default()),
            job_manager: Arc::new(jm),
            token_store: token_store.clone(),
            job_event_tx: Some(tx),
            prompt_queue: Arc::new(Mutex::new(HashMap::new())),
            store: None,
            secrets_store: None,
            user_id: "default".to_string(),
            job_owner_cache: Arc::new(std::sync::RwLock::new(HashMap::new())),
            context_manager: None,
        };

        let job_id = Uuid::new_v4();
        let token = token_store.create_token(job_id).await;
        let router = OrchestratorApi::router(state);

        let payload = serde_json::json!({
            "event_type": "custom_thing",
            "data": { "message": "something custom" }
        });

        let req = Request::builder()
            .method("POST")
            .uri(format!("/worker/{}/event", job_id))
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let (_recv_id, _recv_uid, event) = rx.recv().await.unwrap();
        // Unknown event types fall through to JobStatus
        assert!(matches!(event, SseEvent::JobStatus { .. }));
    }

    // -- Status update test --

    #[tokio::test]
    async fn report_status_updates_handle() {
        let state = test_state();
        let job_id = Uuid::new_v4();
        let token = state.token_store.create_token(job_id).await;

        // Insert a handle so update_worker_status has something to update
        {
            let mut containers = state.job_manager.containers.write().await;
            containers.insert(
                job_id,
                crate::orchestrator::job_manager::ContainerHandle {
                    job_id,
                    container_id: "test-container".to_string(),
                    state: crate::orchestrator::job_manager::ContainerState::Running,
                    mode: crate::orchestrator::job_manager::JobMode::Worker,
                    owner_id: "default".to_string(),
                    mcp_servers: Vec::new(),
                    created_at: chrono::Utc::now(),
                    project_dir: None,
                    task_description: "test".to_string(),
                    last_worker_status: None,
                    worker_iteration: 0,
                    completion_result: None,
                    harness: None,
                },
            );
        }

        let jm = Arc::clone(&state.job_manager);
        let router = OrchestratorApi::router(state);

        let update = serde_json::json!({
            "state": "in_progress",
            "message": "Iteration 5",
            "iteration": 5
        });

        let req = Request::builder()
            .method("POST")
            .uri(format!("/worker/{}/status", job_id))
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&update).unwrap()))
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let handle = jm.get_handle(job_id).await.unwrap();
        assert_eq!(handle.worker_iteration, 5);
        assert_eq!(handle.last_worker_status.as_deref(), Some("Iteration 5"));
    }
}
