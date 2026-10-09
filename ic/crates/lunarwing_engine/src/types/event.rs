//! Event sourcing types.
//!
//! Every significant action within a thread is recorded as an event.
//! This enables replay, debugging, reflection, and trace-based testing.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::capability::LeaseId;

/// Parameter keys whose values must never appear in a human-readable summary
/// or a persisted event payload (WI#352, run-33 AQ-4/AQ-5). Exact match on the
/// top-level object key. `webhook_secret` is the `mission_create` /
/// `mission_update` shared-secret credential; the rest are common credential
/// key names an LLM may supply verbatim on a freeform tool call.
pub(crate) const SENSITIVE_PARAM_KEYS: &[&str] = &[
    "api_key",
    "auth_token",
    "password",
    "secret",
    "token",
    "webhook_secret",
];

/// Generate a short human-readable summary of tool parameters for display.
///
/// For `http`: shows the URL. For `web_search`: shows the query.
/// For other tools: shows the first string argument, truncated.
/// Returns `None` for empty or unrecognizable params.
///
/// The generic fallback skips values keyed under [`SENSITIVE_PARAM_KEYS`] so a
/// credential (e.g. a `mission_create` `webhook_secret`) cannot ride the
/// summary into a channel status line or a persisted event (WI#352, AQ-5).
pub fn summarize_params(action_name: &str, params: &serde_json::Value) -> Option<String> {
    let summary = match action_name {
        "http" | "web_fetch" => params
            .get("url")
            .and_then(|v| v.as_str())
            .map(|u| truncate(u, 80)),
        "web_search" | "llm_context" => params
            .get("query")
            .and_then(|v| v.as_str())
            .map(|q| truncate(q, 60)),
        "memory_search" => params
            .get("query")
            .and_then(|v| v.as_str())
            .map(|q| truncate(q, 60)),
        "memory_write" => params
            .get("target")
            .and_then(|v| v.as_str())
            .map(|t| t.to_string()),
        "memory_read" => params
            .get("path")
            .and_then(|v| v.as_str())
            .map(|p| p.to_string()),
        "shell" => params
            .get("command")
            .and_then(|v| v.as_str())
            .map(|c| truncate(c, 60)),
        "message" => params
            .get("content")
            .and_then(|v| v.as_str())
            .map(|c| truncate(c, 40)),
        _ => {
            // Generic: show the first string value whose key is not on the
            // sensitive-key deny list (WI#352, AQ-5).
            if let Some(obj) = params.as_object() {
                obj.iter()
                    .filter(|(key, _)| !SENSITIVE_PARAM_KEYS.contains(&key.as_str()))
                    .find_map(|(_, v)| v.as_str())
                    .map(|s| truncate(s, 50))
            } else {
                None
            }
        }
    };
    summary.filter(|s| !s.is_empty())
}

/// Return a display copy of `params` with every [`SENSITIVE_PARAM_KEYS`] value
/// replaced by `"[REDACTED]"` (WI#352, run-33 AQ-4).
///
/// Used when a gated call's parameters are persisted in an
/// [`EventKind::ApprovalRequested`] event: the event history is an at-rest
/// record, not the resume path, so it keeps only the redacted copy. The
/// pending gate holds the raw parameters for the actual resume.
pub fn redact_event_params(params: &serde_json::Value) -> serde_json::Value {
    let mut redacted = params.clone();
    if let Some(obj) = redacted.as_object_mut() {
        for key in SENSITIVE_PARAM_KEYS {
            if obj.contains_key(*key) {
                obj.insert(
                    (*key).to_string(),
                    serde_json::Value::String("[REDACTED]".into()),
                );
            }
        }
    }
    redacted
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        // Find a safe UTF-8 boundary
        let mut end = max.min(s.len());
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

pub const ACTION_RESULT_PREVIEW_MAX_BYTES: usize = 1000;

/// Build a bounded display preview from a tool output value.
pub fn preview_from_output(output: &serde_json::Value) -> Option<String> {
    let raw = match output {
        serde_json::Value::Null => return None,
        serde_json::Value::String(value) => value.clone(),
        value => value.to_string(),
    };

    if raw.is_empty() {
        None
    } else {
        Some(truncate(&raw, ACTION_RESULT_PREVIEW_MAX_BYTES))
    }
}
use crate::types::step::{StepId, TokenUsage};
use crate::types::thread::{ThreadId, ThreadState};

/// Strongly-typed event identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventId(pub Uuid);

impl EventId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}

/// A recorded event in a thread's execution history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadEvent {
    pub id: EventId,
    pub thread_id: ThreadId,
    pub timestamp: DateTime<Utc>,
    pub kind: EventKind,
}

impl ThreadEvent {
    pub fn new(thread_id: ThreadId, kind: EventKind) -> Self {
        Self {
            id: EventId::new(),
            thread_id,
            timestamp: Utc::now(),
            kind,
        }
    }
}

/// The specific kind of event that occurred.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventKind {
    // ── Thread lifecycle ────────────────────────────────────
    StateChanged {
        from: ThreadState,
        to: ThreadState,
        reason: Option<String>,
    },

    // ── Step lifecycle ──────────────────────────────────────
    StepStarted {
        step_id: StepId,
    },
    StepCompleted {
        step_id: StepId,
        tokens: TokenUsage,
    },
    StepFailed {
        step_id: StepId,
        error: String,
    },

    // ── Action execution ────────────────────────────────────
    ActionExecuted {
        step_id: StepId,
        action_name: String,
        call_id: String,
        duration_ms: u64,
        /// Short human-readable summary of parameters (e.g., URL for http tool).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params_summary: Option<String>,
        /// Bounded preview of the post-safety tool output.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_preview: Option<String>,
    },
    ActionFailed {
        step_id: StepId,
        action_name: String,
        call_id: String,
        error: String,
        /// Short human-readable summary of parameters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params_summary: Option<String>,
    },

    // ── Capability leases ───────────────────────────────────
    LeaseGranted {
        lease_id: LeaseId,
        capability_name: String,
    },
    LeaseRevoked {
        lease_id: LeaseId,
        reason: String,
    },
    LeaseExpired {
        lease_id: LeaseId,
    },

    // ── Messages ────────────────────────────────────────────
    MessageAdded {
        role: String,
        content_preview: String,
    },
    /// A live, provider-neutral assistant text fragment.
    ///
    /// This event is broadcast for delivery but is not persisted in the
    /// thread's event history.
    ResponseDelta {
        content: String,
    },

    // ── Thread tree ─────────────────────────────────────────
    ChildSpawned {
        child_id: ThreadId,
        goal: String,
    },
    ChildCompleted {
        child_id: ThreadId,
    },

    // ── Approval flow ───────────────────────────────────────
    ApprovalRequested {
        action_name: String,
        call_id: String,
        /// Display copy of the gated call's parameters with
        /// [`SENSITIVE_PARAM_KEYS`] values redacted (WI#352, run-33 AQ-4):
        /// this event is persisted in the thread's event history, which is an
        /// at-rest record, not the resume path. The raw parameters live on
        /// the host's pending gate (unredacted, needed to resume) and in the
        /// thread transcript's action arguments (a pre-existing at-rest
        /// class).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parameters: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allow_always: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gate_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params_summary: Option<String>,
    },
    ApprovalReceived {
        call_id: String,
        approved: bool,
    },

    // ── Self-improvement ──────────────────────────────────────
    SelfImprovementStarted,
    SelfImprovementComplete {
        prompt_updated: bool,
        patterns_added: usize,
    },
    SelfImprovementFailed {
        error: String,
    },

    // ── Skill activation ───────────────────────────────────────
    SkillActivated {
        skill_names: Vec<String>,
    },

    // ── Orchestrator versioning ───────────────────────────────
    OrchestratorRollback {
        from_version: u64,
        to_version: u64,
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::{EventKind, ThreadEvent, preview_from_output, redact_event_params, summarize_params};
    use crate::types::thread::ThreadId;

    #[test]
    fn summarize_params_never_surfaces_sensitive_values() {
        // WI#352 (run-33 AQ-5): a CodeAct mission_create carrying a webhook
        // secret must not put the secret into the channel status summary,
        // even when it is the only (or first) string value.
        let secret_only = serde_json::json!({"webhook_secret": "super-secret-value"});
        assert_eq!(summarize_params("mission_create", &secret_only), None);

        let with_benign = serde_json::json!({
            "webhook_secret": "super-secret-value",
            "goal": "report on events",
        });
        let summary = summarize_params("mission_create", &with_benign)
            .expect("a non-sensitive string value still summarizes");
        assert!(!summary.contains("super-secret-value"));

        // A secret that is not the first string value is still skipped.
        let trailing_secret = serde_json::json!({
            "name": "n",
            "webhook_secret": "super-secret-value",
        });
        let summary = summarize_params("mission_update", &trailing_secret)
            .expect("name summarizes");
        assert_eq!(summary, "n");

        for key in ["api_key", "auth_token", "password", "secret", "token"] {
            let params = serde_json::json!({key: "super-secret-value"});
            assert_eq!(
                summarize_params("some_custom_tool", &params),
                None,
                "{key} must be deny-listed"
            );
        }
    }

    #[test]
    fn redact_event_params_replaces_only_sensitive_values() {
        let params = serde_json::json!({
            "name": "digest",
            "webhook_secret": "super-secret-value",
            "max_threads_per_day": 5,
        });
        let redacted = redact_event_params(&params);
        assert_eq!(redacted["webhook_secret"], "[REDACTED]");
        assert_eq!(redacted["name"], "digest");
        assert_eq!(redacted["max_threads_per_day"], 5);
        assert!(!redacted.to_string().contains("super-secret-value"));
        // The input is untouched.
        assert_eq!(params["webhook_secret"], "super-secret-value");

        // Non-object params pass through unchanged.
        let scalar = serde_json::json!("webhook_secret");
        assert_eq!(redact_event_params(&scalar), scalar);
    }

    #[test]
    fn response_delta_round_trips_through_serde() {
        let event = ThreadEvent::new(
            ThreadId::new(),
            EventKind::ResponseDelta {
                content: "partial response".to_string(),
            },
        );

        let json = serde_json::to_value(&event).expect("response delta should serialize");
        let decoded: ThreadEvent =
            serde_json::from_value(json).expect("response delta should deserialize");

        assert_eq!(decoded.thread_id, event.thread_id);
        assert!(matches!(
            decoded.kind,
            EventKind::ResponseDelta { content } if content == "partial response"
        ));
    }

    #[test]
    fn action_result_preview_handles_strings_json_and_empty_values() {
        assert_eq!(preview_from_output(&serde_json::Value::Null), None);
        assert_eq!(preview_from_output(&serde_json::json!("")), None);
        assert_eq!(
            preview_from_output(&serde_json::json!("hello")).as_deref(),
            Some("hello")
        );
        assert_eq!(
            preview_from_output(&serde_json::json!({"count": 2})).as_deref(),
            Some("{\"count\":2}")
        );
    }

    #[test]
    fn action_result_preview_truncates_at_utf8_boundary() {
        let preview = preview_from_output(&serde_json::json!("你".repeat(400)))
            .expect("non-empty output should produce a preview");

        assert!(preview.len() <= 1003);
        assert!(preview.ends_with("..."));
    }

    #[test]
    fn action_executed_without_preview_round_trips_as_legacy_event() {
        let event = ThreadEvent::new(
            ThreadId::new(),
            EventKind::ActionExecuted {
                step_id: crate::types::step::StepId::new(),
                action_name: "shell".into(),
                call_id: "call-1".into(),
                duration_ms: 1,
                params_summary: None,
                result_preview: None,
            },
        );

        let json = serde_json::to_value(&event).expect("event should serialize");
        assert!(
            json["kind"]["ActionExecuted"]
                .get("result_preview")
                .is_none()
        );
        let decoded: ThreadEvent =
            serde_json::from_value(json).expect("legacy event should deserialize");
        assert!(matches!(
            decoded.kind,
            EventKind::ActionExecuted {
                result_preview: None,
                ..
            }
        ));
    }
}
