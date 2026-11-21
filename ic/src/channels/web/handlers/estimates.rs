//! Read-only runtime-estimation diagnostics (WI#318 A2/A4).
//!
//! `GET /api/estimates` is user-scoped: the bearer token resolves the user and
//! only that user's models are returned. `GET /api/estimates/counters` and
//! `GET /api/estimates/evaluation` expose owner-only aggregates, without user
//! identities. All routes sit behind the existing gateway auth middleware;
//! no tool parameters are returned.

use std::sync::Arc;

use axum::{Json, Router, extract::State, http::StatusCode, routing::get};

use crate::channels::web::auth::AuthenticatedUser;
use crate::channels::web::server::GatewayState;
use crate::estimation::{EvaluationSummary, ToolEstimateCounters, ToolEstimateDiagnostics};
use crate::tools::ToolRegistry;

pub(crate) fn routes() -> Router<Arc<GatewayState>> {
    Router::new()
        .route("/api/estimates", get(estimates_handler))
        .route("/api/estimates/counters", get(estimates_counters_handler))
        .route(
            "/api/estimates/evaluation",
            get(estimates_evaluation_handler),
        )
}

fn registry(state: &GatewayState) -> Result<&ToolRegistry, (StatusCode, String)> {
    state.tool_registry.as_deref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Tool registry unavailable".to_string(),
    ))
}

fn owner_registry<'a>(
    state: &'a GatewayState,
    user_id: &str,
) -> Result<&'a ToolRegistry, (StatusCode, String)> {
    // Check identity before revealing registry availability or trace state.
    if user_id != state.owner_id {
        return Err((StatusCode::FORBIDDEN, "Owner access required".to_string()));
    }
    registry(state)
}

/// Per-user tool runtime models and effective display policy.
async fn estimates_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<ToolEstimateDiagnostics>, (StatusCode, String)> {
    let registry = registry(&state)?;
    Ok(Json(registry.estimates().diagnostics(&user.user_id)))
}

/// Unlabeled registry-wide counts, available even when the trace is off.
async fn estimates_counters_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<ToolEstimateCounters>, (StatusCode, String)> {
    let registry = owner_registry(&state, &user.user_id)?;
    Ok(Json(registry.estimates().aggregate_counters()))
}

/// Operator-only aggregate of the opt-in held-out evaluation trace.
async fn estimates_evaluation_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<EvaluationSummary>, (StatusCode, String)> {
    let registry = owner_registry(&state, &user.user_id)?;
    registry.estimates().evaluation().map(Json).ok_or((
        StatusCode::NOT_FOUND,
        "Evaluation trace is not enabled".to_string(),
    ))
}
