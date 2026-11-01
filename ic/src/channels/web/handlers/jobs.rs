//! Job and sandbox API handlers.

use std::collections::HashSet;
use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::channels::web::auth::AuthenticatedUser;
use crate::channels::web::server::GatewayState;
use crate::channels::web::types::*;
use crate::orchestrator::job_cancel::{CancelPolicy, JobCancelError, JobCanceller};

/// Query parameters for `GET /api/jobs`.
#[derive(Debug, Deserialize)]
pub struct JobListQuery {
    /// Optional mode filter: `worker`, `harness`, `external`, or
    /// `external:<worker>` (WI#479). Jobs with no stored mode match `worker`
    /// — the historical default — but never `external*`.
    pub mode: Option<String>,
}

impl JobListQuery {
    /// Whether a job carrying `job_mode` should be returned for this filter.
    fn accepts(&self, job_mode: Option<&str>) -> bool {
        let Some(filter) = self.mode.as_deref().map(str::trim) else {
            return true;
        };
        if filter.is_empty() {
            return true;
        }
        // Stored modes are canonical lowercase (`worker`, `harness`,
        // `external:<name>`), so a case-insensitive filter lets `?mode=External`
        // behave like `?mode=external` instead of silently matching nothing.
        let filter = filter.to_ascii_lowercase();
        // The detail response omits the `worker` default, so treat it as
        // equivalent to an absent mode here for filter purposes.
        let mode = match job_mode {
            Some(m) if !m.is_empty() => m,
            _ => "worker",
        };
        match filter.as_str() {
            "external" => mode.starts_with("external:"),
            _ => mode == filter,
        }
    }
}

pub async fn jobs_list_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Query(query): Query<JobListQuery>,
) -> Result<Json<JobListResponse>, (StatusCode, String)> {
    let store = state.store.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Database not available".to_string(),
    ))?;

    let mut jobs: Vec<JobInfo> = Vec::new();
    let mut seen_ids: HashSet<Uuid> = HashSet::new();

    // Fetch sandbox jobs scoped to this user.
    match store.list_sandbox_jobs_for_user(&user.user_id).await {
        Ok(sandbox_jobs) => {
            for j in &sandbox_jobs {
                let ui_state = match j.status.as_str() {
                    "creating" => "pending",
                    "running" => "in_progress",
                    s => s,
                };
                // WI#479: the record already carries `job_mode`, so lane
                // attribution needs no per-job detail call.
                let mode = Some(j.job_mode.as_str()).filter(|m| !m.is_empty());
                if !query.accepts(mode) {
                    continue;
                }
                seen_ids.insert(j.id);
                jobs.push(JobInfo {
                    id: j.id,
                    title: j.task.clone(),
                    state: ui_state.to_string(),
                    user_id: j.user_id.clone(),
                    created_at: j.created_at.to_rfc3339(),
                    started_at: j.started_at.map(|dt| dt.to_rfc3339()),
                    job_mode: mode.filter(|m| *m != "worker").map(str::to_string),
                    worker_name: worker_name_from_mode(mode),
                });
            }
        }
        Err(e) => {
            tracing::warn!("Failed to list sandbox jobs: {}", e);
        }
    }

    // Fetch agent (non-sandbox) jobs scoped to this user, deduplicating by ID.
    // Agent jobs carry no sandbox `job_mode`, so only the unfiltered list and
    // the explicit `worker` alias include them.
    match store.list_agent_jobs_for_user(&user.user_id).await {
        Ok(agent_jobs) => {
            for j in &agent_jobs {
                if seen_ids.contains(&j.id) {
                    continue;
                }
                if !query.accepts(None) {
                    continue;
                }
                jobs.push(JobInfo {
                    id: j.id,
                    title: j.title.clone(),
                    state: j.status.clone(),
                    user_id: j.user_id.clone(),
                    created_at: j.created_at.to_rfc3339(),
                    started_at: j.started_at.map(|dt| dt.to_rfc3339()),
                    job_mode: None,
                    worker_name: None,
                });
            }
        }
        Err(e) => {
            tracing::warn!("Failed to list agent jobs: {}", e);
        }
    }

    // Most recent first.
    jobs.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    Ok(Json(JobListResponse { jobs }))
}

pub async fn jobs_summary_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<JobSummaryResponse>, (StatusCode, String)> {
    let store = state.store.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Database not available".to_string(),
    ))?;

    let mut total = 0;
    let mut pending = 0;
    let mut in_progress = 0;
    let mut completed = 0;
    let mut failed = 0;
    let mut stuck = 0;

    // Sandbox job counts scoped to this user.
    match store.sandbox_job_summary_for_user(&user.user_id).await {
        Ok(s) => {
            total += s.total;
            pending += s.creating;
            in_progress += s.running;
            completed += s.completed;
            failed += s.failed + s.interrupted;
            stuck += s.stuck;
        }
        Err(e) => {
            tracing::warn!("Failed to fetch sandbox job summary: {}", e);
        }
    }

    // Agent job counts scoped to this user.
    match store.agent_job_summary_for_user(&user.user_id).await {
        Ok(s) => {
            total += s.total;
            pending += s.pending;
            in_progress += s.in_progress;
            completed += s.completed;
            failed += s.failed;
            stuck += s.stuck;
        }
        Err(e) => {
            tracing::warn!("Failed to fetch agent job summary: {}", e);
        }
    }

    Ok(Json(JobSummaryResponse {
        total,
        pending,
        in_progress,
        completed,
        failed,
        stuck,
    }))
}

pub async fn jobs_detail_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<String>,
) -> Result<Json<JobDetailResponse>, (StatusCode, String)> {
    let store = state.store.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Database not available".to_string(),
    ))?;

    let job_id = Uuid::parse_str(&id)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid job ID".to_string()))?;

    // Try sandbox job from DB first.
    match store.get_sandbox_job(job_id).await {
        Ok(Some(job)) => {
            if job.user_id != user.user_id {
                return Err((StatusCode::NOT_FOUND, "Job not found".to_string()));
            }
            let browse_id = std::path::Path::new(&job.project_dir)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| job.id.to_string());

            let ui_state = match job.status.as_str() {
                "creating" => "pending",
                "running" => "in_progress",
                s => s,
            };

            let elapsed_secs = job.started_at.map(|start| {
                let end = job.completed_at.unwrap_or_else(chrono::Utc::now);
                (end - start).num_seconds().max(0) as u64
            });

            // Synthesize transitions from timestamps.
            let mut transitions = Vec::new();
            if let Some(started) = job.started_at {
                transitions.push(TransitionInfo {
                    from: "creating".to_string(),
                    to: "running".to_string(),
                    timestamp: started.to_rfc3339(),
                    reason: None,
                });
            }
            if let Some(completed) = job.completed_at {
                transitions.push(TransitionInfo {
                    from: "running".to_string(),
                    to: job.status.clone(),
                    timestamp: completed.to_rfc3339(),
                    reason: job.failure_reason.clone(),
                });
            }

            let mode = store.get_sandbox_job_mode(job.id).await.ok().flatten();
            // WI#328: running external worker jobs are promptable via the
            // runtime control channel only when the worker actually
            // advertised the `task_prompt` capability (MR !1508 review CT-1);
            // other sandbox modes remain non-promptable from the web UI.
            let can_prompt = job.status == "running"
                && mode.as_deref().is_some_and(|m| m.starts_with("external:"))
                && match state.external_worker_manager.as_ref() {
                    Some(ewm) => matches!(
                        ewm.control_capability(job.id).await,
                        Some(crate::orchestrator::external_worker::ControlCapability::Supported)
                    ),
                    None => false,
                };

            // WI#475: surface the live `stuck` / `repair_attempts` state when
            // the daemon has an in-memory context for this job. The DB sandbox
            // row carries neither, so absent context (restart/eviction) leaves
            // them `None`.
            let (stuck, repair_attempts) = match state.context_manager.as_ref() {
                Some(cm) => match cm.get_context(job_id).await {
                    Ok(ctx) => (
                        Some(ctx.state == crate::context::JobState::Stuck),
                        Some(ctx.repair_attempts),
                    ),
                    Err(_) => (None, None),
                },
                None => (None, None),
            };

            return Ok(Json(JobDetailResponse {
                id: job.id,
                title: job.task.clone(),
                description: String::new(),
                state: ui_state.to_string(),
                user_id: job.user_id.clone(),
                created_at: job.created_at.to_rfc3339(),
                started_at: job.started_at.map(|dt| dt.to_rfc3339()),
                completed_at: job.completed_at.map(|dt| dt.to_rfc3339()),
                elapsed_secs,
                project_dir: Some(job.project_dir.clone()),
                browse_url: Some(format!("/projects/{}/", browse_id)),
                job_mode: mode.filter(|m| m != "worker"),
                transitions,
                can_restart: state.job_manager.is_some(),
                can_prompt,
                job_kind: Some("sandbox".to_string()),
                stuck,
                repair_attempts,
            }));
        }
        Ok(None) => {}
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            ));
        }
    }

    // Fall back to agent job from DB.
    match store.get_job(job_id).await {
        Ok(Some(ctx)) => {
            if ctx.user_id != user.user_id {
                return Err((StatusCode::NOT_FOUND, "Job not found".to_string()));
            }
            let elapsed_secs = ctx.started_at.map(|start| {
                let end = ctx.completed_at.unwrap_or_else(chrono::Utc::now);
                (end - start).num_seconds().max(0) as u64
            });

            // Only show prompt bar for jobs that have a running worker (Pending/InProgress).
            // Stuck jobs have no active worker loop, so messages would be silently dropped.
            let is_promptable = matches!(
                ctx.state,
                crate::context::JobState::Pending | crate::context::JobState::InProgress
            );
            // WI#475: prefer live in-memory state when present (it carries the
            // current stuck flag and attempt count); fall back to the
            // DB-reconstructed context's persisted `repair_attempts`.
            let (stuck, repair_attempts) = match state.context_manager.as_ref() {
                Some(cm) => match cm.get_context(job_id).await {
                    Ok(live) => (
                        Some(live.state == crate::context::JobState::Stuck),
                        Some(live.repair_attempts),
                    ),
                    Err(_) => (None, Some(ctx.repair_attempts)),
                },
                None => (None, Some(ctx.repair_attempts)),
            };
            Ok(Json(JobDetailResponse {
                id: ctx.job_id,
                title: ctx.title.clone(),
                description: ctx.description.clone(),
                state: ctx.state.to_string(),
                user_id: ctx.user_id.clone(),
                created_at: ctx.created_at.to_rfc3339(),
                started_at: ctx.started_at.map(|dt| dt.to_rfc3339()),
                completed_at: ctx.completed_at.map(|dt| dt.to_rfc3339()),
                elapsed_secs,
                project_dir: None,
                browse_url: None,
                job_mode: None,
                transitions: Vec::new(),
                can_restart: state.scheduler.is_some(),
                can_prompt: is_promptable && state.scheduler.is_some(),
                job_kind: Some("agent".to_string()),
                stuck,
                repair_attempts,
            }))
        }
        Ok(None) => Err((StatusCode::NOT_FOUND, "Job not found".to_string())),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Database error: {}", e),
        )),
    }
}

/// Cancel one sandbox job through the shared sequencer, for both runtime
/// kinds (external worker and container).
///
/// Order matters (WI#476): the cancel is delivered to the runtime first, the
/// persisted status is written next, and the in-memory context transition is
/// last. A context failure is reported on its own — it must never skip the
/// durable `cancelled` write, or the row would stay `running` while the
/// runtime is already gone. The container branch needs this explicitly: the
/// exit watcher skips `Stopped` handles, so nothing else moves its context.
async fn cancel_sandbox_job(
    state: &GatewayState,
    job_id: Uuid,
    job_mode: Option<&str>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let external = job_mode.is_some_and(|mode| mode.starts_with("external:"));
    if external && state.external_worker_manager.is_none() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "External worker manager not available".to_string(),
        ));
    }

    let context_manager = state.context_manager.clone().or_else(|| {
        state
            .external_worker_manager
            .as_ref()
            .and_then(|manager| manager.context_manager_handle().cloned())
    });
    if external && context_manager.is_none() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "Job context manager not available".to_string(),
        ));
    }

    let mut canceller = JobCanceller::new(context_manager);
    if let Some(scheduler) = &state.scheduler {
        canceller = canceller.with_scheduler(Arc::clone(scheduler));
    }
    if let Some(manager) = &state.job_manager {
        canceller = canceller.with_container_jobs(Arc::clone(manager));
    }
    if let Some(manager) = &state.external_worker_manager {
        canceller = canceller.with_external_workers(Arc::clone(manager));
    }
    if let Some(store) = &state.store {
        canceller = canceller.with_store(Arc::clone(store));
    }

    let report = match canceller.cancel(job_id, CancelPolicy::RequireRuntime).await {
        Ok(report) => report,
        Err(error @ JobCancelError::Finalizing { .. })
        | Err(error @ JobCancelError::NoActiveRuntime { .. }) => {
            return Err((StatusCode::CONFLICT, error.to_string()));
        }
        Err(error) => return Err((StatusCode::INTERNAL_SERVER_ERROR, error.to_string())),
    };

    if let Some(error) = report.persistence_error {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "Cancellation was delivered, but the terminal status could not be persisted: {error}"
            ),
        ));
    }
    if let Some(error) = report.context_error {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "Cancellation was delivered and persisted, but the in-memory job context could not be synchronized: {error}"
            ),
        ));
    }

    Ok(Json(serde_json::json!({
        "status": "cancelled",
        "job_id": job_id,
        "context_synced": report.context_synced,
    })))
}

pub async fn jobs_cancel_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let job_id = Uuid::parse_str(&id)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid job ID".to_string()))?;

    // Try sandbox job cancellation.
    if let Some(ref store) = state.store {
        match store.get_sandbox_job(job_id).await {
            Ok(Some(job)) => {
                if job.user_id != user.user_id {
                    return Err((StatusCode::NOT_FOUND, "Job not found".to_string()));
                }
                if job.status != "running" && job.status != "creating" {
                    return Err((
                        StatusCode::CONFLICT,
                        format!("Job is '{}' and cannot be cancelled", job.status),
                    ));
                }

                let mode = store
                    .get_sandbox_job_mode(job_id)
                    .await
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                // Sandbox cancellation always goes through the shared
                // sequencer: deliver, persist, then move the context.
                return cancel_sandbox_job(state.as_ref(), job_id, mode.as_deref()).await;
            }
            Ok(None) => {}
            Err(e) => {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Database error: {}", e),
                ));
            }
        }
    }

    // Fall back to agent job cancellation: stop the worker via the scheduler
    // (which updates the in-memory ContextManager AND aborts the task handle),
    // then persist the status to the DB as a fallback.
    if let Some(ref store) = state.store {
        match store.get_job(job_id).await {
            Ok(Some(job)) => {
                if job.user_id != user.user_id {
                    return Err((StatusCode::NOT_FOUND, "Job not found".to_string()));
                }
                if job.state.is_active() {
                    // Try to stop via scheduler (aborts the worker task + updates
                    // in-memory ContextManager). This is best-effort — the job may
                    // not be in the scheduler map if it already finished.
                    let stopped = if let Some(ref slot) = state.scheduler
                        && let Some(ref scheduler) = *slot.read().await
                    {
                        scheduler.stop(job_id).await.map_err(|error| {
                            (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
                        })?
                    } else {
                        false
                    };
                    if !stopped {
                        return Err((
                            StatusCode::CONFLICT,
                            "Job has no active scheduler runtime; cancellation was not delivered"
                                .to_string(),
                        ));
                    }

                    // Always persist cancellation to the DB so the state is
                    // consistent even if the scheduler wasn't available or the
                    // job wasn't in its in-memory map.
                    store
                        .update_job_status(
                            job_id,
                            crate::context::JobState::Cancelled,
                            Some("Cancelled by user"),
                        )
                        .await
                        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                }
                return Ok(Json(serde_json::json!({
                    "status": "cancelled",
                    "job_id": job_id,
                })));
            }
            Ok(None) => {}
            Err(e) => {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Database error: {}", e),
                ));
            }
        }
    }

    Err((StatusCode::NOT_FOUND, "Job not found".to_string()))
}

pub async fn jobs_restart_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = state.store.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Database not available".to_string(),
    ))?;

    let old_job_id = Uuid::parse_str(&id)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid job ID".to_string()))?;

    // Try sandbox job restart first.
    match store.get_sandbox_job(old_job_id).await {
        Ok(Some(old_job)) => {
            if old_job.user_id != user.user_id {
                return Err((StatusCode::NOT_FOUND, "Job not found".to_string()));
            }
            if old_job.status != "interrupted" && old_job.status != "failed" {
                return Err((
                    StatusCode::CONFLICT,
                    format!("Cannot restart job in state '{}'", old_job.status),
                ));
            }

            // CV-9 (WI#326): harness jobs cannot be restarted as plain
            // workers — their secrets, injection snapshot, and staging dirs
            // were per-dispatch and are gone, and the persisted task text
            // still references them. Require a fresh harness dispatch.
            match store.get_sandbox_job_mode(old_job_id).await {
                Ok(Some(ref mode)) if mode == "harness" => {
                    return Err((
                        StatusCode::CONFLICT,
                        "Harness jobs cannot be restarted: secrets, injection snapshots, and \
                         write-back staging are per-dispatch. Create a new job with mode 'harness'."
                            .to_string(),
                    ));
                }
                Ok(_) => {}
                Err(e) => {
                    return Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Database error: {e}"),
                    ));
                }
            }

            let jm = state.job_manager.as_ref().ok_or((
                StatusCode::SERVICE_UNAVAILABLE,
                "Sandbox not enabled".to_string(),
            ))?;

            // Enrich the task with failure context.
            let task = if let Some(ref reason) = old_job.failure_reason {
                format!(
                    "Previous attempt failed: {}. Retry: {}",
                    reason, old_job.task
                )
            } else {
                old_job.task.clone()
            };

            let new_job_id = Uuid::new_v4();
            let now = chrono::Utc::now();
            let grants: crate::orchestrator::auth::SandboxJobGrants =
                serde_json::from_str(&old_job.credential_grants_json).map_err(|error| {
                    (
                        StatusCode::CONFLICT,
                        format!("Cannot restart job with invalid saved grants: {error}"),
                    )
                })?;
            let (mcp_servers, mcp_credential_grants) = crate::orchestrator::snapshot_worker_mcp(
                Some(store.as_ref()),
                state.secrets_store.as_deref(),
                &old_job.user_id,
                &grants.mcp_servers,
            )
            .await
            .map_err(|e| {
                (
                    StatusCode::CONFLICT,
                    format!("Cannot restart job with saved MCP selection: {e}"),
                )
            })?;
            let grants_json = serde_json::to_string(&grants).map_err(|error| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to persist restarted job grants: {error}"),
                )
            })?;

            let record = crate::history::SandboxJobRecord {
                id: new_job_id,
                task: task.clone(),
                status: "creating".to_string(),
                user_id: old_job.user_id.clone(),
                project_dir: old_job.project_dir.clone(),
                success: None,
                failure_reason: None,
                created_at: now,
                started_at: None,
                completed_at: None,
                credential_grants_json: grants_json,
                job_mode: "worker".to_string(),
            };
            store
                .save_sandbox_job(&record)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            let mode = crate::orchestrator::job_manager::JobMode::Worker;

            let project_dir = std::path::PathBuf::from(&old_job.project_dir);
            let _token = jm
                .create_job(crate::orchestrator::ContainerJobRequest {
                    job_id: new_job_id,
                    task,
                    project_dir: Some(project_dir),
                    mode,
                    owner_id: old_job.user_id.clone(),
                    credential_grants: grants.credentials,
                    mcp_servers,
                    mcp_credential_grants,
                    max_iterations: lunarwing_common::DEFAULT_WORKER_ITERATIONS,
                    // Harness jobs are rejected above (WI#326 CV-9); only
                    // plain worker jobs restart, so there are no harness
                    // options to re-dispatch.
                    harness: None,
                })
                .await
                .map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to create container: {}", e),
                    )
                })?;

            store
                .update_sandbox_job_status(new_job_id, "running", None, None, Some(now), None)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            return Ok(Json(serde_json::json!({
                "status": "restarted",
                "old_job_id": old_job_id,
                "new_job_id": new_job_id,
            })));
        }
        Ok(None) => {}
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            ));
        }
    }

    // Try agent job restart: dispatch a new job via the scheduler.
    match store.get_job(old_job_id).await {
        Ok(Some(old_job)) => {
            if old_job.user_id != user.user_id {
                return Err((StatusCode::NOT_FOUND, "Job not found".to_string()));
            }
            if old_job.state.is_active() {
                return Err((
                    StatusCode::CONFLICT,
                    format!("Cannot restart job in state '{}'", old_job.state),
                ));
            }

            let slot = state.scheduler.as_ref().ok_or((
                StatusCode::SERVICE_UNAVAILABLE,
                "Scheduler not available".to_string(),
            ))?;
            let scheduler_guard = slot.read().await;
            let scheduler = scheduler_guard.as_ref().ok_or((
                StatusCode::SERVICE_UNAVAILABLE,
                "Agent not started yet".to_string(),
            ))?;

            // Look up failure reason (O(1) point lookup).
            let failure_reason = store
                .get_agent_job_failure_reason(old_job_id)
                .await
                .ok()
                .flatten()
                .unwrap_or_default();

            let title = if !failure_reason.is_empty() {
                format!(
                    "Previous attempt failed: {}. Retry: {}",
                    failure_reason, old_job.title
                )
            } else {
                old_job.title.clone()
            };

            let new_job_id = scheduler
                .dispatch_job(&old_job.user_id, &title, &old_job.description, None)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            Ok(Json(serde_json::json!({
                "status": "restarted",
                "old_job_id": old_job_id,
                "new_job_id": new_job_id,
            })))
        }
        Ok(None) => Err((StatusCode::NOT_FOUND, "Job not found".to_string())),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Database error: {}", e),
        )),
    }
}

/// Submit a follow-up prompt to a running job.
///
/// Routes to the appropriate backend:
/// - Claude Code sandbox jobs → prompt queue (polled by the bridge)
/// - External worker jobs → runtime control channel `task_prompt` (WI#328)
/// - Agent (non-sandbox) jobs → WorkerMessage injection via scheduler
/// - Worker-mode sandbox jobs → not supported (no mechanism to inject)
pub async fn jobs_prompt_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let job_id: uuid::Uuid = id
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid job ID".to_string()))?;

    let content = body
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or((
            StatusCode::BAD_REQUEST,
            "Missing 'content' field".to_string(),
        ))?
        .to_string();

    let done = body.get("done").and_then(|v| v.as_bool()).unwrap_or(false);

    // Try sandbox job path first: verify ownership, then route by job mode.
    if let Some(ref s) = state.store
        && let Ok(Some(sandbox_job)) = s.get_sandbox_job(job_id).await
    {
        // Verify ownership.
        if sandbox_job.user_id != user.user_id {
            return Err((StatusCode::NOT_FOUND, "Job not found".to_string()));
        }

        // External worker jobs accept follow-up prompts over the runtime
        // control channel (WI#328) — delivered as `task_prompt` frames the
        // worker holds until its current turn ends. Validation errors are
        // client errors (400), a full queue is transient (429), and a worker
        // that never advertised the capability is an explicit 400 rather than
        // a false "queued" (MR !1508 review CT-1/CT-4/CT-6).
        let mode = s
            .get_sandbox_job_mode(job_id)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        if mode.as_deref().is_some_and(|m| m.starts_with("external:")) {
            let manager = state.external_worker_manager.as_ref().ok_or((
                StatusCode::SERVICE_UNAVAILABLE,
                "External worker manager not available".to_string(),
            ))?;
            let seq = manager
                .send_prompt(job_id, &user.user_id, &content, done)
                .await
                .map_err(|e| {
                    use crate::error::OrchestratorError;
                    match &e {
                        OrchestratorError::ExternalWorkerInvalidControlPrompt { .. } => {
                            (StatusCode::BAD_REQUEST, e.to_string())
                        }
                        OrchestratorError::ExternalWorkerControlUnsupported { .. } => {
                            (StatusCode::BAD_REQUEST, e.to_string())
                        }
                        OrchestratorError::ExternalWorkerControlQueueFull { .. } => {
                            (StatusCode::TOO_MANY_REQUESTS, e.to_string())
                        }
                        _ => (StatusCode::CONFLICT, e.to_string()),
                    }
                })?;
            return Ok(Json(serde_json::json!({
                "status": "queued",
                "job_id": job_id.to_string(),
                "seq": seq,
            })));
        }

        return Err((
            StatusCode::NOT_IMPLEMENTED,
            "Follow-up prompts are not supported for worker-mode sandbox jobs".to_string(),
        ));
    }

    // Try agent job path: verify ownership, then send via scheduler.
    if let Some(ref store) = state.store {
        match store.get_job(job_id).await {
            Ok(Some(agent_job)) => {
                if agent_job.user_id != user.user_id {
                    return Err((StatusCode::NOT_FOUND, "Job not found".to_string()));
                }
            }
            Ok(None) => {
                return Err((StatusCode::NOT_FOUND, "Job not found".to_string()));
            }
            Err(e) => {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Database error: {}", e),
                ));
            }
        }
    }

    let slot = state.scheduler.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        "Agent job prompts require the scheduler to be configured".to_string(),
    ))?;
    let scheduler_guard = slot.read().await;
    if let Some(ref scheduler) = *scheduler_guard
        && scheduler.is_running(job_id).await
    {
        scheduler
            .send_message(job_id, content)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        return Ok(Json(serde_json::json!({
            "status": "sent",
            "job_id": job_id.to_string(),
        })));
    }

    Err((
        StatusCode::NOT_FOUND,
        "Job not found or not running".to_string(),
    ))
}

/// Load persisted job events for a job (for history replay on page open).
pub async fn jobs_events_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = state.store.as_ref().ok_or((
        StatusCode::NOT_IMPLEMENTED,
        "Database not available".to_string(),
    ))?;

    let job_id: uuid::Uuid = id
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid job ID".to_string()))?;

    // Verify ownership before returning events.
    match store.get_sandbox_job(job_id).await {
        Ok(Some(job)) => {
            if job.user_id != user.user_id {
                return Err((StatusCode::NOT_FOUND, "Job not found".to_string()));
            }
        }
        Ok(None) => {
            return Err((StatusCode::NOT_FOUND, "Job not found".to_string()));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            ));
        }
    }

    let events = store
        .list_job_events(job_id, None)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let events_json: Vec<serde_json::Value> = events
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "id": e.id,
                "event_type": e.event_type,
                "data": e.data,
                "created_at": e.created_at.to_rfc3339(),
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "job_id": job_id.to_string(),
        "events": events_json,
    })))
}

// --- Project file handlers for sandbox jobs ---

#[derive(Deserialize)]
pub struct FilePathQuery {
    pub path: Option<String>,
}

pub async fn job_files_list_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<String>,
    Query(query): Query<FilePathQuery>,
) -> Result<Json<ProjectFilesResponse>, (StatusCode, String)> {
    let store = state.store.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Database not available".to_string(),
    ))?;

    let job_id = Uuid::parse_str(&id)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid job ID".to_string()))?;

    let job = store
        .get_sandbox_job(job_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "Job not found".to_string()))?;

    if job.user_id != user.user_id {
        return Err((StatusCode::NOT_FOUND, "Job not found".to_string()));
    }

    let base = std::path::PathBuf::from(&job.project_dir);
    let rel_path = query.path.as_deref().unwrap_or("");
    let target = base.join(rel_path);

    // Path traversal guard.
    let canonical = target
        .canonicalize()
        .map_err(|_| (StatusCode::NOT_FOUND, "Path not found".to_string()))?;
    let base_canonical = base
        .canonicalize()
        .map_err(|_| (StatusCode::NOT_FOUND, "Project dir not found".to_string()))?;
    if !canonical.starts_with(&base_canonical) {
        return Err((StatusCode::FORBIDDEN, "Forbidden".to_string()));
    }

    let mut entries = Vec::new();
    let mut read_dir = tokio::fs::read_dir(&canonical)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "Cannot read directory".to_string()))?;

    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry
            .file_type()
            .await
            .map(|ft| ft.is_dir())
            .unwrap_or(false);
        let rel = if rel_path.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", rel_path, name)
        };
        entries.push(ProjectFileEntry {
            name,
            path: rel,
            is_dir,
        });
    }

    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));

    Ok(Json(ProjectFilesResponse { entries }))
}

pub async fn job_files_read_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<String>,
    Query(query): Query<FilePathQuery>,
) -> Result<Json<ProjectFileReadResponse>, (StatusCode, String)> {
    let store = state.store.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Database not available".to_string(),
    ))?;

    let job_id = Uuid::parse_str(&id)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid job ID".to_string()))?;

    let job = store
        .get_sandbox_job(job_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "Job not found".to_string()))?;

    if job.user_id != user.user_id {
        return Err((StatusCode::NOT_FOUND, "Job not found".to_string()));
    }

    let path = query.path.as_deref().ok_or((
        StatusCode::BAD_REQUEST,
        "path parameter required".to_string(),
    ))?;

    let base = std::path::PathBuf::from(&job.project_dir);
    let file_path = base.join(path);

    let canonical = file_path
        .canonicalize()
        .map_err(|_| (StatusCode::NOT_FOUND, "File not found".to_string()))?;
    let base_canonical = base
        .canonicalize()
        .map_err(|_| (StatusCode::NOT_FOUND, "Project dir not found".to_string()))?;
    if !canonical.starts_with(&base_canonical) {
        return Err((StatusCode::FORBIDDEN, "Forbidden".to_string()));
    }

    let content = tokio::fs::read_to_string(&canonical)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "Cannot read file".to_string()))?;

    Ok(Json(ProjectFileReadResponse {
        path: path.to_string(),
        content,
    }))
}
