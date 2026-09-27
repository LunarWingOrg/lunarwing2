//! Mission observability API handlers.
//!
//! Read-only tenant-facing surface for engine missions (work item #217).
//! Missions remain managed through the model-facing tools; these routes only
//! expose list/summary/detail views so tenants can see what their missions
//! are doing without a chat turn.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Serialize;

use crate::bridge::{EngineMissionDetail, EngineMissionInfo};
use crate::channels::web::auth::AuthenticatedUser;
use crate::channels::web::server::GatewayState;

#[derive(Debug, Serialize)]
pub struct MissionListResponse {
    pub missions: Vec<EngineMissionInfo>,
}

#[derive(Debug, Serialize)]
pub struct MissionSummaryResponse {
    pub total: u64,
    pub active: u64,
    pub paused: u64,
    pub completed: u64,
    pub budget_exhausted: u64,
    pub never_fired: u64,
}

#[derive(Debug, Serialize)]
pub struct MissionDetailResponse {
    pub mission: EngineMissionDetail,
}

pub async fn missions_list_handler(
    State(_state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<MissionListResponse>, (StatusCode, String)> {
    let missions = crate::bridge::list_engine_missions(None, &user.user_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(MissionListResponse { missions }))
}

pub async fn missions_summary_handler(
    State(_state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<MissionSummaryResponse>, (StatusCode, String)> {
    let missions = crate::bridge::list_engine_missions(None, &user.user_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let total = missions.len() as u64;
    let active = missions
        .iter()
        .filter(|m| m.status.eq_ignore_ascii_case("active"))
        .count() as u64;
    let paused = missions
        .iter()
        .filter(|m| m.status.eq_ignore_ascii_case("paused"))
        .count() as u64;
    let completed = missions
        .iter()
        .filter(|m| m.status.eq_ignore_ascii_case("completed"))
        .count() as u64;
    let budget_exhausted = missions.iter().filter(|m| m.budget_exhausted).count() as u64;
    let never_fired = missions.iter().filter(|m| m.never_fired).count() as u64;

    Ok(Json(MissionSummaryResponse {
        total,
        active,
        paused,
        completed,
        budget_exhausted,
        never_fired,
    }))
}

pub async fn missions_detail_handler(
    State(_state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<String>,
) -> Result<Json<MissionDetailResponse>, (StatusCode, String)> {
    // validate the id shape before touching the engine store
    if uuid::Uuid::parse_str(&id).is_err() {
        return Err((StatusCode::BAD_REQUEST, "Invalid mission ID".to_string()));
    }

    let mission = crate::bridge::get_engine_mission(&id, &user.user_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "Mission not found".to_string()))?;

    Ok(Json(MissionDetailResponse { mission }))
}
