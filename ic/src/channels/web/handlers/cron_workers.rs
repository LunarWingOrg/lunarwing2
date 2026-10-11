//! Owner-only recovery for cron worker missions, independent of the LLM engine.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};

use crate::channels::web::auth::AuthenticatedUser;
use crate::channels::web::server::GatewayState;
use crate::error::OrchestratorError;
use crate::orchestrator::ExternalWorkerManager;
use crate::orchestrator::external_worker::CronCommandOrigin;

/// Overall budget for one control request: enqueue plus the status reply.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CronAction {
    Pause,
    Resume,
    Status,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CronControlRequest {
    action: CronAction,
}

pub async fn cron_worker_status_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(worker): Path<String>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, String)> {
    control(&state, &user.user_id, &worker, CronAction::Status).await
}

pub async fn cron_worker_control_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(worker): Path<String>,
    Json(request): Json<CronControlRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, String)> {
    control(&state, &user.user_id, &worker, request.action).await
}

/// Enqueue the Pause/Resume mutation for `action`. Returns `Ok(false)` for
/// Status (nothing to enqueue). Completes when the supervisor's command
/// channel accepts the command — which is NOT the same as the supervisor
/// having applied it; only the status reply proves application (MR !1531
/// DQ-4).
async fn enqueue_mutation(
    manager: &ExternalWorkerManager,
    worker: &str,
    user_id: &str,
    action: CronAction,
) -> Result<bool, OrchestratorError> {
    match action {
        CronAction::Pause => manager
            .pause_cron_mission(worker, user_id, CronCommandOrigin::OwnerApi)
            .await
            .map(|()| true),
        CronAction::Resume => manager
            .resume_cron_mission(worker, user_id, CronCommandOrigin::OwnerApi)
            .await
            .map(|()| true),
        CronAction::Status => Ok(false),
    }
}

fn command_error(error: OrchestratorError) -> (StatusCode, String) {
    let code = match error {
        OrchestratorError::CronMissionNotOwned { .. } => StatusCode::FORBIDDEN,
        OrchestratorError::ExternalWorkerNotCron { .. } => StatusCode::NOT_FOUND,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    (code, error.to_string())
}

async fn control(
    state: &GatewayState,
    user_id: &str,
    worker: &str,
    action: CronAction,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, String)> {
    // Check identity before looking up a worker or even the manager: a
    // non-owner cannot enumerate mission configuration through errors.
    if user_id != state.owner_id {
        return Err((StatusCode::FORBIDDEN, "Owner access required".to_string()));
    }
    let manager = state.external_worker_manager.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "External worker manager not available".to_string(),
    ))?;
    let deadline = tokio::time::Instant::now() + COMMAND_TIMEOUT;
    // Phase 1 (mutations only): enqueue the command. A timeout here means the
    // channel never accepted it — nothing was enqueued, so 504 is accurate.
    let enqueued =
        match tokio::time::timeout_at(deadline, enqueue_mutation(manager, worker, user_id, action))
            .await
        {
            Ok(Ok(enqueued)) => enqueued,
            Ok(Err(error)) => return Err(command_error(error)),
            Err(_) => {
                return Err((
                    StatusCode::GATEWAY_TIMEOUT,
                    "Cron supervisor did not accept the command in time; nothing was enqueued"
                        .to_string(),
                ));
            }
        };
    // Phase 2: the status reply is queued behind the mutation, so it reflects
    // the applied command rather than merely acknowledging its enqueue.
    match tokio::time::timeout_at(deadline, manager.cron_mission_status(worker, user_id)).await {
        Ok(Ok(status)) => Ok((
            StatusCode::OK,
            Json(serde_json::json!({"action": action, "mission": status})),
        )),
        Ok(Err(error)) => Err(command_error(error)),
        Err(_) if enqueued => {
            // DQ-4: the command was accepted by the queue but not confirmed in
            // time — it may already be applied or still queued. Pause/resume
            // are idempotent (a repeated resume resets the breaker again and
            // writes a duplicate audit row), so report 202 instead of a
            // misleading 504.
            Ok((
                StatusCode::ACCEPTED,
                Json(serde_json::json!({
                    "action": action,
                    "queued": true,
                    "detail": "command accepted by the supervisor queue but not confirmed \
                               in time; re-query status before retrying",
                })),
            ))
        }
        Err(_) => Err((
            StatusCode::GATEWAY_TIMEOUT,
            "Cron supervisor did not respond".to_string(),
        )),
    }
}
