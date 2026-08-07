//! Step execution.
//!
//! - [`ExecutionLoop`] — core loop replacing `run_agentic_loop()`
//! - [`structured`] — Tier 0 action execution (structured tool calls)
//! - [`context`] — context building for LLM calls
//! - [`intent`] — tool intent nudge detection

pub mod compaction;
pub mod context;
mod llm_stream;
pub mod loop_engine;
pub mod orchestrator;
pub mod prompt;
pub mod scripting;
pub mod structured;
pub mod trace;

pub use loop_engine::ExecutionLoop;

/// Maximum tool actions allowed in one execution step.
pub(crate) const MAX_ACTIONS_PER_STEP: usize = 50;

pub(crate) fn capability_unavailable_message(action_name: &str) -> String {
    format!(
        "Capability action '{action_name}' is unavailable for this request. Choose an advertised action or ask the user/operator to enable the required capability."
    )
}

pub(crate) fn capability_lease_unavailable_message(action_name: &str) -> String {
    format!(
        "Capability access for action '{action_name}' is unavailable or exhausted. Choose another advertised action or ask the user/operator to renew capability access."
    )
}

pub(crate) fn policy_denied_message(reason: &str) -> String {
    format!(
        "Call denied by policy: {reason}. This call cannot succeed unchanged; choose an allowed action or parameters, or ask the user/operator to change policy."
    )
}

pub(crate) fn safe_model_failure_message(
    operation: &str,
    error: &crate::types::error::EngineError,
) -> String {
    match error {
        crate::types::error::EngineError::Llm { kind, .. } => format!(
            "{operation} failed ({kind}). {}",
            kind.recovery_instruction()
        ),
        _ => format!(
            "{operation} failed internally. Continue without this result or ask the user/operator to retry."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_failures_include_actionable_next_steps() {
        let unavailable = capability_unavailable_message("search");
        assert!(unavailable.contains("advertised action"));
        assert!(unavailable.contains("enable the required capability"));

        let exhausted = capability_lease_unavailable_message("search");
        assert!(exhausted.contains("renew capability access"));

        let denied = policy_denied_message("network writes disabled");
        assert!(denied.contains("cannot succeed unchanged"));
        assert!(denied.contains("change policy"));
    }
}
