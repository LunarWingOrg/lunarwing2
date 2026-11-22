//! Read-only runtime-estimation diagnostics (WI#318 A2/A4).
//!
//! `GET /api/estimates` is user-scoped: the bearer token resolves the user and
//! only that user's models are returned. `GET /api/estimates/counters` and
//! `GET /api/estimates/evaluation`, and `GET /api/estimates/llm` expose
//! owner-only aggregates without user identities. All routes use the existing
//! gateway auth middleware and return no request content.

use std::sync::Arc;

use axum::{Json, Router, extract::State, http::StatusCode, routing::get};

use crate::channels::web::auth::AuthenticatedUser;
use crate::channels::web::server::GatewayState;
use crate::estimation::{
    EvaluationSummary, LlmTimingDiagnostics, ToolEstimateCounters, ToolEstimateDiagnostics,
};
use crate::gate::wait_analytics::GateWaitDiagnostics;
use crate::tools::ToolRegistry;

pub(crate) fn routes() -> Router<Arc<GatewayState>> {
    Router::new()
        .route("/api/estimates", get(estimates_handler))
        .route("/api/estimates/counters", get(estimates_counters_handler))
        .route(
            "/api/estimates/evaluation",
            get(estimates_evaluation_handler),
        )
        .route("/api/estimates/llm", get(estimates_llm_handler))
        .route(
            "/api/estimates/gate-waits",
            get(estimates_gate_waits_handler),
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

/// Owner-only gate-wait aggregate, separate from runtime and LLM estimates.
/// It reports observed waits by bounded channel family and outcome; it is not
/// a prediction and never influences gate expiry or approval policy.
async fn estimates_gate_waits_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<GateWaitDiagnostics>, (StatusCode, String)> {
    if user.user_id != state.owner_id {
        return Err((StatusCode::FORBIDDEN, "Owner access required".to_string()));
    }
    match crate::bridge::gate_wait_diagnostics().await {
        Ok(Some(diagnostics)) => Ok(Json(diagnostics)),
        Ok(None) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "Engine pending-gate store is not initialized".to_string(),
        )),
        Err(error) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Gate-wait diagnostics unavailable: {error}"),
        )),
    }
}

/// Owner-only daemon-wide LLM logical-call duration models with configured
/// provider and model labels, never request content.
async fn estimates_llm_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<LlmTimingDiagnostics>, (StatusCode, String)> {
    if user.user_id != state.owner_id {
        return Err((StatusCode::FORBIDDEN, "Owner access required".to_string()));
    }
    state
        .llm_timings
        .as_deref()
        .map(|timings| Json(timings.diagnostics()))
        .ok_or((
            StatusCode::SERVICE_UNAVAILABLE,
            "LLM timing diagnostics unavailable".to_string(),
        ))
}
