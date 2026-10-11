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
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    control(&state, &user.user_id, &worker, CronAction::Status).await
}

pub async fn cron_worker_control_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(worker): Path<String>,
    Json(request): Json<CronControlRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    control(&state, &user.user_id, &worker, request.action).await
}

async fn control(
    state: &GatewayState,
    user_id: &str,
    worker: &str,
    action: CronAction,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Check identity before looking up a worker or even the manager: a
    // non-owner cannot enumerate mission configuration through errors.
    if user_id != state.owner_id {
        return Err((StatusCode::FORBIDDEN, "Owner access required".to_string()));
    }
    let manager = state.external_worker_manager.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "External worker manager not available".to_string(),
    ))?;
    let status = tokio::time::timeout(Duration::from_secs(10), async {
        match action {
            CronAction::Pause => manager.pause_cron_mission(worker, user_id).await?,
            CronAction::Resume => manager.resume_cron_mission(worker, user_id).await?,
            CronAction::Status => {}
        }
        // Status is queued behind the mutation, so the response reflects the
        // applied command rather than merely acknowledging its enqueue.
        manager.cron_mission_status(worker, user_id).await
    })
    .await
    .map_err(|_| {
        (
            StatusCode::GATEWAY_TIMEOUT,
            "Cron supervisor did not respond".to_string(),
        )
    })?
    .map_err(|error| {
        let code = match error {
            OrchestratorError::CronMissionNotOwned { .. } => StatusCode::FORBIDDEN,
            OrchestratorError::ExternalWorkerNotCron { .. } => StatusCode::NOT_FOUND,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        };
        (code, error.to_string())
    })?;
    Ok(Json(
        serde_json::json!({"action": action, "mission": status}),
    ))
}
