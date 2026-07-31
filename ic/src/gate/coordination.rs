//! Coordination gate — blocks all tool execution for auto-disabled agents.
//!
//! Part of the multi-agent team coordination pipeline
//! (`docs/proposals/MULTIAGENTTEAMORCHESTRATION_INTEGRATION_PLAN_v2.0.4.0.md`,
//! Phase 3). The gate only checks disabled status; violation counting happens
//! in the coordination hooks (`crate::hooks::coordination`).
//!
//! Priority 5 — above the lease gate (10) and tool-tier gate (20).

use std::sync::Arc;

use async_trait::async_trait;
use lunarwing_engine::gate::{ExecutionGate, GateContext, GateDecision};

use crate::gate::violation_store::ViolationStore;

/// Prefix marking a deny reason that must not be surfaced to the channel.
///
/// Disabled agents go dark: no channel message, no error surfaced. Surfacing
/// paths (gate pipeline callers) should check this prefix and log at `debug`
/// level only.
pub const SILENT_DENY_PREFIX: &str = "[SILENT] ";

/// Returns true when a deny reason is marked silent via [`SILENT_DENY_PREFIX`].
pub fn is_silent_deny(reason: &str) -> bool {
    reason.starts_with(SILENT_DENY_PREFIX)
}

/// Execution gate that denies all tool execution while an agent is disabled
/// for coordination violations.
pub struct CoordinationGate {
    store: Arc<ViolationStore>,
    /// This agent's own identifier (`COORD_AGENT_ID`).
    agent_id: String,
    /// When true the gate allows everything (testing escape hatch).
    permissive: bool,
}

impl CoordinationGate {
    /// Create a gate backed by the given violation store.
    pub fn new(store: Arc<ViolationStore>, agent_id: String) -> Self {
        let permissive = std::env::var("COORD_PERMISSIVE")
            .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
            .unwrap_or(false);
        Self {
            store,
            agent_id,
            permissive,
        }
    }

    /// Silent-deny reason when this agent (or `user_id`) is disabled.
    ///
    /// Returns `None` in permissive mode or when neither identity is
    /// disabled. Callers that cannot build a full [`GateContext`] (e.g. the
    /// effect bridge adapter) use this directly; they must treat the
    /// [`SILENT_DENY_PREFIX`] reason as log-only and stop execution before
    /// any tool side effect.
    pub fn deny_reason(&self, user_id: &str) -> Option<String> {
        if self.permissive {
            return None;
        }
        let disabled_agent = if self.store.is_disabled(&self.agent_id) {
            Some(self.agent_id.as_str())
        } else if self.store.is_disabled(user_id) {
            Some(user_id)
        } else {
            None
        };
        disabled_agent.map(|agent| {
            format!(
                "{SILENT_DENY_PREFIX}agent '{agent}' auto-disabled for coordination violations"
            )
        })
    }
}

#[async_trait]
impl ExecutionGate for CoordinationGate {
    fn name(&self) -> &str {
        "coordination"
    }

    fn priority(&self) -> u32 {
        5
    }

    async fn evaluate(&self, ctx: &GateContext<'_>) -> GateDecision {
        match self.deny_reason(ctx.user_id) {
            Some(reason) => GateDecision::Deny { reason },
            None => GateDecision::Allow,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::violation_store::ViolationStoreConfig;

    fn disabled_store() -> Arc<ViolationStore> {
        let dir = std::env::temp_dir().join(format!("coord-gate-test-{}", std::process::id()));
        let store = ViolationStore::new(
            dir.join("violations.json"),
            ViolationStoreConfig {
                max_violations: 1,
                disable_duration_secs: 3600,
                decay_secs: 600,
            },
        );
        store.record_violation("bad-agent", "r", "hook", "chan");
        Arc::new(store)
    }

    #[test]
    fn test_silent_deny_prefix() {
        assert!(is_silent_deny("[SILENT] nope"));
        assert!(!is_silent_deny("regular deny"));
    }

    #[tokio::test]
    async fn test_gate_denies_disabled_agent() {
        let store = disabled_store();
        let gate = CoordinationGate::new(store, "bad-agent".to_string());
        assert_eq!(gate.name(), "coordination");
        assert_eq!(gate.priority(), 5);
        // Evaluate against a different user; own agent_id is disabled → deny.
        let decision = evaluate_with_user(&gate, "some-user").await;
        match decision {
            GateDecision::Deny { reason } => assert!(is_silent_deny(&reason)),
            other => panic!("expected deny, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_gate_denies_disabled_user() {
        let store = disabled_store();
        let gate = CoordinationGate::new(store, "good-agent".to_string());
        let decision = evaluate_with_user(&gate, "bad-agent").await;
        assert!(matches!(decision, GateDecision::Deny { .. }));
    }

    #[tokio::test]
    async fn test_gate_allows_clean_agent() {
        let store = disabled_store();
        let gate = CoordinationGate::new(store, "good-agent".to_string());
        let decision = evaluate_with_user(&gate, "good-user").await;
        assert!(matches!(decision, GateDecision::Allow));
    }

    async fn evaluate_with_user(gate: &CoordinationGate, user: &str) -> GateDecision {
        use lunarwing_engine::gate::ExecutionMode;
        use lunarwing_engine::types::capability::{ActionDef, EffectType};
        use lunarwing_engine::types::thread::ThreadId;
        use std::collections::HashSet;

        let action_def = ActionDef {
            name: "test".to_string(),
            description: String::new(),
            parameters_schema: serde_json::json!({}),
            effects: vec![EffectType::ReadLocal],
            requires_approval: false,
        };
        let params = serde_json::Value::Null;
        let auto_approved: HashSet<String> = HashSet::new();
        let ctx = GateContext {
            user_id: user,
            thread_id: ThreadId::new(),
            source_channel: "test",
            action_name: &action_def.name,
            call_id: "call-1",
            parameters: &params,
            action_def: &action_def,
            execution_mode: ExecutionMode::Interactive,
            auto_approved: &auto_approved,
            supervised_mode: false,
            supervised_timeout_secs: 300,
        };
        gate.evaluate(&ctx).await
    }
}
