//! Read-only runtime-estimation diagnostics (WI#318 A2/A4).
//!
//! `GET /api/estimates` is user-scoped: the bearer token resolves the user and
//! only that user's models are returned. `GET /api/estimates/evaluation`
//! exposes the operator-only held-out trace aggregate, which contains no user
//! identity. Neither route adds a listener; both sit behind the existing
//! gateway auth middleware. Output is aggregate numbers plus canonical tool
//! names, so a tool parameter can never appear in a response.

use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};

use crate::channels::web::auth::AuthenticatedUser;
use crate::channels::web::server::GatewayState;
use crate::estimation::{EvaluationSummary, ToolEstimateDiagnostics};
use crate::tools::ToolRegistry;

fn registry(state: &GatewayState) -> Result<&Arc<ToolRegistry>, (StatusCode, String)> {
    state.tool_registry.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Tool registry unavailable".to_string(),
    ))
}

/// Per-user tool runtime models, counters and category policy.
pub async fn estimates_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<ToolEstimateDiagnostics>, (StatusCode, String)> {
    let registry = registry(&state)?;
    Ok(Json(registry.estimates().diagnostics(&user.user_id)))
}

/// Operator-only aggregate of the opt-in held-out evaluation trace.
pub async fn estimates_evaluation_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<EvaluationSummary>, (StatusCode, String)> {
    // Check identity before revealing whether the trace is enabled.
    if user.user_id != state.owner_id {
        return Err((StatusCode::FORBIDDEN, "Owner access required".to_string()));
    }
    let registry = registry(&state)?;
    registry.estimates().evaluation().map(Json).ok_or((
        StatusCode::NOT_FOUND,
        "Evaluation trace is not enabled".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use crate::estimation::{CategoryPolicy, EvaluationTrace, ToolEstimates, ToolEstimationMode};

    /// The user-facing payload must not carry another user's models, the
    /// private learner key, or any tool parameter value.
    #[test]
    fn diagnostics_payload_is_user_scoped_and_parameter_free() {
        let secret = "sk-ant-not-for-output";
        let params = serde_json::json!({ "authorization": secret });
        let estimates = ToolEstimates::new(ToolEstimationMode::Display);
        for _ in 0..5 {
            let observation = estimates
                .begin("alice", &crate::tools::builtin::EchoTool, &params)
                .expect("observation");
            estimates.complete(observation, std::time::Duration::from_millis(5), None);
        }
        let rendered = serde_json::to_string(&estimates.diagnostics("alice")).expect("json");
        assert!(rendered.contains("echo"));
        assert!(!rendered.contains(secret));
        assert!(!rendered.contains("alice"));
        assert!(!rendered.contains("tool:"));
        let bob = serde_json::to_string(&estimates.diagnostics("bob")).expect("json");
        assert!(!bob.contains("echo"));
    }

    /// The operator endpoint is served only from an enabled, explicitly
    /// composed trace; the default registry starts with it off.
    #[test]
    fn evaluation_payload_requires_an_enabled_trace_and_is_aggregate_only() {
        let off = ToolEstimates::new(ToolEstimationMode::Observe);
        assert!(off.evaluation().is_none());

        let traced = ToolEstimates::configured(
            ToolEstimationMode::Observe,
            CategoryPolicy::default(),
            EvaluationTrace::from_setting(Some("16")),
        );
        let params = serde_json::json!({ "api_key": "sk-eval-secret" });
        let observation = traced
            .begin("alice", &crate::tools::builtin::EchoTool, &params)
            .expect("observation");
        traced.complete(observation, std::time::Duration::from_millis(2), None);
        let rendered = serde_json::to_string(&traced.evaluation().expect("enabled")).expect("json");
        assert!(rendered.contains("echo"));
        assert!(!rendered.contains("alice"));
        assert!(!rendered.contains("sk-eval-secret"));
    }
}
