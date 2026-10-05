//! Thread — the unit of work.
//!
//! A thread is a bounded task or investigation. It unifies the concepts of
//! Session (interactive conversation), Job (background work), Routine
//! (scheduled execution), and Sub-agent (delegated reasoning) into a single
//! abstraction with a shared state machine.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::capability::LeaseId;
use crate::types::error::EngineError;
use crate::types::event::{EventKind, ThreadEvent};
use crate::types::message::ThreadMessage;
use crate::types::project::ProjectId;

use super::{OwnerId, default_user_id};

/// Strongly-typed thread identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ThreadId(pub Uuid);

impl ThreadId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ThreadId {
    fn default() -> Self {
        Self::new()
    }
}

// ── State machine ───────────────────────────────────────────

/// Thread lifecycle state.
///
/// ```text
/// Created → Running → Waiting → Running (resume) | Completed (external stop)
///                   → Suspended → Running (resume) | Completed (external stop)
///                   → Completed → Done | Failed
///                   → Failed
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ThreadState {
    /// Thread has been created but not yet started.
    Created,
    /// Thread is actively executing steps.
    Running,
    /// Waiting for external input (user approval, child completion).
    Waiting,
    /// Paused by system (resource pressure, priority preemption).
    Suspended,
    /// Execution finished successfully.
    Completed,
    /// Fully finished (terminal).
    Done,
    /// Terminal failure.
    Failed,
}

impl ThreadState {
    /// Check whether a transition to `target` is valid.
    pub fn can_transition_to(self, target: Self) -> bool {
        matches!(
            (self, target),
            // From Created
            (Self::Created, Self::Running)
            | (Self::Created, Self::Failed)
            // From Running
            | (Self::Running, Self::Waiting)
            | (Self::Running, Self::Suspended)
            | (Self::Running, Self::Completed)
            | (Self::Running, Self::Failed)
            // From Waiting
            | (Self::Waiting, Self::Running)
            | (Self::Waiting, Self::Completed)
            | (Self::Waiting, Self::Failed)
            // From Suspended
            | (Self::Suspended, Self::Running)
            | (Self::Suspended, Self::Completed)
            | (Self::Suspended, Self::Failed)
            // From Completed
            | (Self::Completed, Self::Done)
            | (Self::Completed, Self::Failed)
        )
    }

    /// Whether this state is terminal (no further transitions possible).
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed)
    }

    /// Whether this state represents active work.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Running | Self::Waiting)
    }
}

// ── Thread type ─────────────────────────────────────────────

/// The nature of the work a thread performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThreadType {
    /// Interactive conversation with a user.
    Foreground,
    /// Background research or sub-task.
    Research,
    /// Long-running goal that spawns threads over time.
    Mission,
}

// ── Thread configuration ────────────────────────────────────

/// Host-selected mission context. Model arguments and mutable REPL state cannot
/// grant maintenance recall or relax an insight worker's write restrictions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum MissionContext {
    #[default]
    General,
    ConversationInsights {
        recall_query: String,
        #[serde(default)]
        batch: Option<crate::types::insight::InsightBatch>,
        #[serde(default)]
        mission_id: Option<crate::types::mission::MissionId>,
        #[serde(default)]
        run_id: Option<ThreadId>,
        #[serde(default)]
        generation: u64,
    },
}

impl MissionContext {
    pub fn is_conversation_insights(&self) -> bool {
        matches!(self, Self::ConversationInsights { .. })
    }
}

/// Execution parameters for a thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadConfig {
    /// Maximum number of LLM call iterations.
    pub max_iterations: usize,
    /// Maximum wall-clock duration for the thread.
    pub max_duration: Option<std::time::Duration>,
    /// Whether to detect and nudge on tool intent without action calls.
    pub enable_tool_intent_nudge: bool,
    /// Maximum number of tool intent nudges per thread.
    pub max_tool_intent_nudges: u32,
    /// Require at least one action or code attempt before accepting a text
    /// completion for this thread.
    #[serde(default)]
    pub require_action_attempt: bool,
    /// Maximum number of corrective nudges when action-attempt obligation is
    /// enabled.
    #[serde(default = "default_max_action_requirement_nudges")]
    pub max_action_requirement_nudges: u32,
    /// Maximum engine-level recovery attempts per model failure streak.
    /// Per-kind policy caps (`MODEL_RECOVERY_POLICY` in the orchestrator)
    /// apply on top. Default: 1 (legacy one-attempt behavior). Clamped to
    /// 0..=3. Env override: ENGINE_V2_MODEL_RECOVERY_ATTEMPTS.
    /// Note: when this is at least 1, context_overflow is exempt from the
    /// per-streak clamp (WI#298) — its shrink ladder changes the inputs
    /// instead of blindly retrying, so it uses its policy cap (2);
    /// `model_recovery_total_attempts` still bounds it over the thread
    /// lifetime, and 0 still disables engine recovery for every kind.
    #[serde(default = "default_model_recovery_max_attempts")]
    pub model_recovery_max_attempts: u32,
    /// Total engine-level recovery attempts allowed per thread lifetime.
    /// Bounds cumulative recovery time (the Monty sandbox has no clock for a
    /// wall-clock budget). Default: 4. Env override:
    /// ENGINE_V2_MODEL_RECOVERY_TOTAL_ATTEMPTS.
    #[serde(default = "default_model_recovery_total_attempts")]
    pub model_recovery_total_attempts: u32,

    // ── Human delay / supervision mode ──
    /// When true, every tool action is gated through human approval,
    /// regardless of the tool's `requires_approval()` tier.
    /// Auto-expires after `supervised_timeout_secs` on no response.
    #[serde(default)]
    pub supervised_mode: bool,
    /// Whether multiple channel actors share this conversation scope. Approval-
    /// requiring effects must not use per-user grants in this context.
    #[serde(default)]
    pub shared_conversation: bool,
    /// Timeout in seconds for supervised mode approval prompts.
    /// After this duration with no human response, the action is
    /// auto-denied (fail-closed). Default: 300 (5 minutes).
    #[serde(default = "default_supervised_timeout_secs")]
    pub supervised_timeout_secs: u64,
    /// Timeout in seconds for a single async tool or LLM future awaited
    /// during a `ResolveFutures` round. When a deferred tool or LLM task
    /// takes longer than this cap, its task is aborted and the call
    /// resolves as an error instead of hanging the thread forever
    /// (ResolveFutures otherwise awaits pending futures unbounded).
    /// Default: 300 (5 minutes). Env override:
    /// ENGINE_V2_TOOL_ASYNC_TIMEOUT_SECS.
    #[serde(default = "default_tool_async_timeout_secs")]
    pub tool_async_timeout_secs: u64,

    // ── Budget controls (Phase 4, from RLM cross-reference) ──
    /// Maximum cumulative input+output tokens before termination.
    pub max_tokens_total: Option<u64>,
    /// Maximum consecutive steps with errors before termination.
    /// Resets to 0 on any successful step (matching official RLM behavior).
    pub max_consecutive_errors: Option<u32>,
    /// Model context limit in tokens (for compaction threshold calculation).
    /// Default: 128,000. Used to trigger compaction at 85% usage.
    pub model_context_limit: usize,
    /// Whether to enable automatic compaction when context grows large.
    pub enable_compaction: bool,
    /// Compaction threshold as fraction of model_context_limit (0.0-1.0).
    /// Default: 0.85 (matching official RLM).
    pub compaction_threshold: f64,
    /// Maximum cumulative USD cost before termination.
    /// Requires the LlmBackend to populate `TokenUsage::cost_usd`.
    pub max_budget_usd: Option<f64>,
    /// Depth of this thread in the recursive call tree.
    /// Root threads are depth 0. Sub-calls via rlm_query() increment depth.
    pub depth: u32,
    /// Maximum recursion depth for rlm_query() sub-calls.
    pub max_depth: u32,
    /// True for rlm_query() child threads whose parent is a Mission thread.
    ///
    /// rlm children are typed [`ThreadType::Research`] for lease and
    /// anti-recursion purposes, but they execute on behalf of a mission and
    /// must keep the mission-only `__save_memory_doc__` host function.
    /// Without this flag the child's name lookup raises NameError and the
    /// model concludes the function "is not callable from this sandbox" and
    /// pivots to a plain workspace write — losing doc_type/tags metadata.
    #[serde(default)]
    pub mission_child: bool,

    /// Assigned by MissionManager and inherited by mission children.
    #[serde(default)]
    pub mission_context: MissionContext,

    /// Host-provided conversation identity for newly ingested user evidence.
    #[serde(default)]
    pub conversation_id: Option<crate::types::conversation::ConversationId>,

    // ── Channel delivery constraints ──
    /// Delivery-constraint guidance appended to the system prompt at thread
    /// creation. Set by the host bridge for rate-metered channels (e.g.
    /// DarkIRC's bounded, paced messages); `None` for unconstrained channels.
    /// The engine itself is channel-neutral — it only relays this text.
    #[serde(default)]
    pub channel_context: Option<String>,
}

impl Default for ThreadConfig {
    fn default() -> Self {
        Self {
            max_iterations: 50,
            max_duration: None,
            enable_tool_intent_nudge: true,
            max_tool_intent_nudges: 2,
            require_action_attempt: false,
            max_action_requirement_nudges: 2,
            model_recovery_max_attempts: std::env::var("ENGINE_V2_MODEL_RECOVERY_ATTEMPTS")
                .ok()
                .and_then(|value| value.parse::<u32>().ok())
                .map(|value| value.min(3))
                .unwrap_or(1),
            model_recovery_total_attempts: std::env::var("ENGINE_V2_MODEL_RECOVERY_TOTAL_ATTEMPTS")
                .ok()
                .and_then(|value| value.parse::<u32>().ok())
                .map(|value| value.min(16))
                .unwrap_or(4),
            supervised_mode: false,
            shared_conversation: false,
            supervised_timeout_secs: 300,
            tool_async_timeout_secs: std::env::var("ENGINE_V2_TOOL_ASYNC_TIMEOUT_SECS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(300),
            max_tokens_total: None,
            max_consecutive_errors: None,
            max_budget_usd: None,
            model_context_limit: 128_000,
            enable_compaction: false,
            compaction_threshold: 0.85,
            depth: 0,
            max_depth: 1,
            mission_child: false,
            mission_context: MissionContext::General,
            conversation_id: None,
            channel_context: None,
        }
    }
}

fn default_max_action_requirement_nudges() -> u32 {
    2
}

fn default_supervised_timeout_secs() -> u64 {
    300
}

fn default_tool_async_timeout_secs() -> u64 {
    300
}

fn default_model_recovery_max_attempts() -> u32 {
    1
}

fn default_model_recovery_total_attempts() -> u32 {
    4
}

// ── Thread ──────────────────────────────────────────────────

/// A thread — the unit of work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    pub id: ThreadId,
    pub goal: String,
    pub thread_type: ThreadType,
    pub state: ThreadState,
    pub project_id: ProjectId,
    /// Tenant isolation: the user who owns this thread.
    #[serde(default = "default_user_id")]
    pub user_id: String,
    pub parent_id: Option<ThreadId>,
    pub config: ThreadConfig,
    /// User-visible transcript for the thread.
    pub messages: Vec<ThreadMessage>,
    /// Only fresh user inputs. Inherited history and generated goals never enter
    /// this collection. Missing on legacy threads: fail closed for extraction.
    #[serde(default)]
    pub insight_inputs: Vec<crate::types::insight::InsightSource>,
    /// Internal execution transcript used by the orchestrator for inference,
    /// tool traces, compaction, and resumable working state.
    #[serde(default)]
    pub internal_messages: Vec<ThreadMessage>,
    pub events: Vec<ThreadEvent>,
    pub capability_leases: Vec<LeaseId>,
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub step_count: usize,
    pub total_tokens_used: u64,
    /// Cumulative USD cost across all steps.
    pub total_cost_usd: f64,
}

impl Thread {
    /// Create a new thread in the `Created` state.
    pub fn new(
        goal: impl Into<String>,
        thread_type: ThreadType,
        project_id: ProjectId,
        user_id: impl Into<String>,
        config: ThreadConfig,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: ThreadId::new(),
            goal: goal.into(),
            thread_type,
            state: ThreadState::Created,
            project_id,
            user_id: user_id.into(),
            parent_id: None,
            config,
            messages: Vec::new(),
            insight_inputs: Vec::new(),
            internal_messages: Vec::new(),
            events: Vec::new(),
            capability_leases: Vec::new(),
            metadata: serde_json::Value::Object(serde_json::Map::new()),
            created_at: now,
            updated_at: now,
            completed_at: None,
            step_count: 0,
            total_tokens_used: 0,
            total_cost_usd: 0.0,
        }
    }

    /// Create a child thread with a parent reference.
    pub fn with_parent(mut self, parent_id: ThreadId) -> Self {
        self.parent_id = Some(parent_id);
        self
    }

    pub fn owner_id(&self) -> OwnerId<'_> {
        OwnerId::from_user_id(&self.user_id)
    }

    pub fn is_owned_by(&self, user_id: &str) -> bool {
        self.owner_id().matches_user(user_id)
    }

    /// Transition to a new state, recording an event.
    pub fn transition_to(
        &mut self,
        new_state: ThreadState,
        reason: Option<String>,
    ) -> Result<(), EngineError> {
        if !self.state.can_transition_to(new_state) {
            return Err(EngineError::InvalidTransition {
                from: self.state,
                to: new_state,
            });
        }

        let event = ThreadEvent::new(
            self.id,
            EventKind::StateChanged {
                from: self.state,
                to: new_state,
                reason,
            },
        );
        self.events.push(event);
        self.state = new_state;
        self.updated_at = Utc::now();

        if new_state == ThreadState::Completed || new_state == ThreadState::Done {
            self.completed_at = Some(Utc::now());
        }

        Ok(())
    }

    /// Add an event to this thread's log.
    pub fn add_event(&mut self, kind: EventKind) {
        self.events.push(ThreadEvent::new(self.id, kind));
        self.updated_at = Utc::now();
    }

    /// Add a message to this thread's conversation.
    pub fn add_message(&mut self, message: ThreadMessage) {
        if message.role == crate::types::message::MessageRole::SealedStepRecord {
            // WI#227: a sealed machine step record is inference-only data.
            // Route it into the internal transcript so it can never appear in
            // the user-visible conversation, whatever path called this. Still
            // emit the MessageAdded event: channel UIs interpret the role +
            // preview into status hints ("Code executed", "Code error —
            // retrying...") that predate the seal and must keep working.
            let preview = if message.content.chars().count() > 80 {
                let p: String = message.content.chars().take(80).collect();
                format!("{p}...")
            } else {
                message.content.clone()
            };
            self.add_event(EventKind::MessageAdded {
                role: message.role.wire_name().to_string(),
                content_preview: preview,
            });
            self.internal_messages.push(message);
            self.updated_at = Utc::now();
            return;
        }
        if self.thread_type == ThreadType::Foreground
            && self.parent_id.is_none()
            && !self.config.mission_child
            && !self.config.mission_context.is_conversation_insights()
            && message.role == crate::types::message::MessageRole::User
            && message.provenance == crate::types::provenance::Provenance::User
            && !message.content.trim().is_empty()
        {
            self.insight_inputs
                .push(crate::types::insight::InsightSource {
                    message_id: uuid::Uuid::new_v4(),
                    thread_id: self.id,
                    conversation_id: self.config.conversation_id,
                    timestamp: message.timestamp,
                    text: message.content.clone(),
                    truncated: false,
                });
        }
        let preview = if message.content.chars().count() > 80 {
            let p: String = message.content.chars().take(80).collect();
            format!("{p}...")
        } else {
            message.content.clone()
        };
        self.add_event(EventKind::MessageAdded {
            role: message.role.wire_name().to_string(),
            content_preview: preview,
        });
        self.messages.push(message);
    }

    /// Add a message to the internal execution transcript without exposing it
    /// as a user-visible conversation message.
    pub fn add_internal_message(&mut self, message: ThreadMessage) {
        self.internal_messages.push(message);
        self.updated_at = Utc::now();
    }

    /// Stamp the restart-failure marker (WI#301). Called by the boot-time
    /// recovery sweep when it fails a thread that was still in flight at
    /// kill time. The marker is permanent history — it is never cleared —
    /// so recovery/refire policy can distinguish "interrupted by restart"
    /// from "failed by error" for the lifetime of the record.
    pub fn mark_failed_by_restart(&mut self) {
        upsert_metadata_bool(&mut self.metadata, RESTART_FAILURE_METADATA_KEY);
    }

    /// Whether the boot-time sweep failed this thread after a daemon restart
    /// (as opposed to the thread failing on its own).
    pub fn is_failed_by_restart(&self) -> bool {
        metadata_flag(&self.metadata, RESTART_FAILURE_METADATA_KEY)
    }

    /// Stamp that the mission-side recovery pass has recorded and notified
    /// this restart-lost fire (WI#301). Keeps the record/notify pass
    /// idempotent across repeated boots while the permanent
    /// `restart_failure` marker stays as history.
    pub fn mark_restart_failure_recorded(&mut self) {
        upsert_metadata_bool(&mut self.metadata, RESTART_FAILURE_RECORDED_METADATA_KEY);
    }

    /// Whether the mission-side recovery pass already recorded/notified the
    /// restart-lost fire for this thread.
    pub fn is_restart_failure_recorded(&self) -> bool {
        metadata_flag(&self.metadata, RESTART_FAILURE_RECORDED_METADATA_KEY)
    }
}

/// Metadata key marking a thread the boot-time recovery sweep failed because
/// the daemon restarted while the thread was still in flight (WI#301).
pub const RESTART_FAILURE_METADATA_KEY: &str = "restart_failure";

/// Metadata key recording that the restart-lost fire was already recorded in
/// the mission's approach history and notified to its channels (WI#301).
pub const RESTART_FAILURE_RECORDED_METADATA_KEY: &str = "restart_failure_recorded";

/// State-transition reason recorded when a run is stopped by host policy
/// (signal stop, daemon shutdown) rather than failing on its own (WI#302).
/// The mission recovery sweeps read the terminal event's reason to tell a
/// settled stop apart from a genuine failure, so the literal must stay
/// shared by every writer (orchestrator stop paths) and reader.
pub const EXECUTION_STOPPED_REASON: &str = "execution stopped";

/// Variant of [`EXECUTION_STOPPED_REASON`] for a stop that arrived while a
/// provider stream was in flight.
pub const STREAM_STOPPED_REASON: &str = "stopped during LLM stream";

/// Whether a terminal transition reason marks a host-requested stop rather
/// than a genuine failure (budget, provider error, …).
pub fn is_host_stop_reason(reason: Option<&str>) -> bool {
    matches!(reason, Some(EXECUTION_STOPPED_REASON) | Some(STREAM_STOPPED_REASON))
}

fn metadata_flag(metadata: &serde_json::Value, key: &str) -> bool {
    metadata
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn upsert_metadata_bool(metadata: &mut serde_json::Value, key: &str) {
    if let serde_json::Value::Object(map) = metadata {
        map.insert(key.to_string(), serde_json::Value::Bool(true));
    } else {
        *metadata = serde_json::json!({ key: true });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_thread() -> Thread {
        Thread::new(
            "test goal",
            ThreadType::Foreground,
            ProjectId::new(),
            "test-user",
            ThreadConfig::default(),
        )
    }

    // ── State machine tests ─────────────────────────────────

    #[test]
    fn created_can_transition_to_running() {
        assert!(ThreadState::Created.can_transition_to(ThreadState::Running));
    }

    #[test]
    fn created_can_transition_to_failed() {
        assert!(ThreadState::Created.can_transition_to(ThreadState::Failed));
    }

    #[test]
    fn created_cannot_transition_to_completed() {
        assert!(!ThreadState::Created.can_transition_to(ThreadState::Completed));
    }

    #[test]
    fn running_can_transition_to_waiting() {
        assert!(ThreadState::Running.can_transition_to(ThreadState::Waiting));
    }

    #[test]
    fn running_can_transition_to_suspended() {
        assert!(ThreadState::Running.can_transition_to(ThreadState::Suspended));
    }

    #[test]
    fn running_can_transition_to_completed() {
        assert!(ThreadState::Running.can_transition_to(ThreadState::Completed));
    }

    #[test]
    fn running_can_transition_to_failed() {
        assert!(ThreadState::Running.can_transition_to(ThreadState::Failed));
    }

    #[test]
    fn waiting_can_resume_to_running() {
        assert!(ThreadState::Waiting.can_transition_to(ThreadState::Running));
    }

    #[test]
    fn waiting_can_complete_on_external_stop() {
        assert!(ThreadState::Waiting.can_transition_to(ThreadState::Completed));
    }

    #[test]
    fn suspended_can_resume_to_running() {
        assert!(ThreadState::Suspended.can_transition_to(ThreadState::Running));
    }

    #[test]
    fn suspended_can_complete_on_external_stop() {
        assert!(ThreadState::Suspended.can_transition_to(ThreadState::Completed));
    }

    #[test]
    fn completed_can_transition_to_done() {
        assert!(ThreadState::Completed.can_transition_to(ThreadState::Done));
    }

    #[test]
    fn completed_can_transition_to_failed_during_finalization() {
        assert!(ThreadState::Completed.can_transition_to(ThreadState::Failed));
    }

    #[test]
    fn done_is_terminal() {
        assert!(ThreadState::Done.is_terminal());
        assert!(!ThreadState::Done.can_transition_to(ThreadState::Running));
    }

    #[test]
    fn failed_is_terminal() {
        assert!(ThreadState::Failed.is_terminal());
        assert!(!ThreadState::Failed.can_transition_to(ThreadState::Running));
    }

    #[test]
    fn running_is_active() {
        assert!(ThreadState::Running.is_active());
    }

    #[test]
    fn waiting_is_active() {
        assert!(ThreadState::Waiting.is_active());
    }

    #[test]
    fn created_is_not_active() {
        assert!(!ThreadState::Created.is_active());
    }

    // ── Thread lifecycle tests ──────────────────────────────

    #[test]
    fn new_thread_is_created() {
        let t = make_thread();
        assert_eq!(t.state, ThreadState::Created);
        assert!(t.events.is_empty());
        assert!(t.messages.is_empty());
    }

    #[test]
    fn valid_transition_succeeds() {
        let mut t = make_thread();
        assert!(t.transition_to(ThreadState::Running, None).is_ok());
        assert_eq!(t.state, ThreadState::Running);
        assert_eq!(t.events.len(), 1);
    }

    #[test]
    fn invalid_transition_fails() {
        let mut t = make_thread();
        let result = t.transition_to(ThreadState::Completed, None);
        assert!(result.is_err());
        assert_eq!(t.state, ThreadState::Created);
    }

    #[test]
    fn full_lifecycle_created_to_done() {
        let mut t = make_thread();
        t.transition_to(ThreadState::Running, None).unwrap();
        t.transition_to(ThreadState::Completed, Some("finished".into()))
            .unwrap();
        t.transition_to(ThreadState::Done, None).unwrap();
        assert!(t.state.is_terminal());
        assert_eq!(t.events.len(), 3);
        assert!(t.completed_at.is_some());
    }

    #[test]
    fn add_message_records_event() {
        let mut t = make_thread();
        t.add_message(ThreadMessage::user("hello"));
        assert_eq!(t.messages.len(), 1);
        assert_eq!(t.events.len(), 1);
        match &t.events[0].kind {
            EventKind::MessageAdded { role, .. } => assert_eq!(role, "User"),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn child_thread_has_parent() {
        let parent = make_thread();
        let child = Thread::new(
            "child goal",
            ThreadType::Research,
            parent.project_id,
            "test-user",
            ThreadConfig::default(),
        )
        .with_parent(parent.id);
        assert_eq!(child.parent_id, Some(parent.id));
    }

    #[test]
    fn thread_config_without_channel_context_still_deserializes() {
        // ThreadConfig is persisted with threads; configs serialized before
        // `channel_context` existed must keep deserializing (serde default).
        let legacy = serde_json::json!({
            "max_iterations": 50,
            "max_duration": null,
            "enable_tool_intent_nudge": true,
            "max_tool_intent_nudges": 2,
            "max_tokens_total": null,
            "max_consecutive_errors": null,
            "max_budget_usd": null,
            "model_context_limit": 128000,
            "enable_compaction": false,
            "compaction_threshold": 0.85,
            "depth": 0,
            "max_depth": 1
        });
        let config: ThreadConfig =
            serde_json::from_value(legacy).expect("legacy config must deserialize");
        assert!(config.channel_context.is_none());
    }

    #[test]
    fn thread_config_channel_context_round_trips() {
        let config = ThreadConfig {
            channel_context: Some("- keep replies under 400 bytes".to_string()),
            ..ThreadConfig::default()
        };
        let json = serde_json::to_string(&config).expect("serialize");
        let parsed: ThreadConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            parsed.channel_context.as_deref(),
            Some("- keep replies under 400 bytes")
        );
    }

    // ── Restart-failure markers (WI#301) ────────────────────

    #[test]
    fn restart_markers_default_to_absent() {
        let t = make_thread();
        assert!(!t.is_failed_by_restart());
        assert!(!t.is_restart_failure_recorded());
    }

    #[test]
    fn mark_failed_by_restart_sets_only_the_restart_flag() {
        let mut t = make_thread();
        t.mark_failed_by_restart();
        assert!(t.is_failed_by_restart());
        assert!(!t.is_restart_failure_recorded());
    }

    #[test]
    fn restart_failure_recorded_is_independent_of_the_failure_marker() {
        let mut t = make_thread();
        t.mark_failed_by_restart();
        t.mark_restart_failure_recorded();
        assert!(t.is_failed_by_restart());
        assert!(t.is_restart_failure_recorded());
    }

    #[test]
    fn restart_markers_handle_non_object_metadata() {
        let mut t = make_thread();
        t.metadata = serde_json::Value::Null;
        t.mark_failed_by_restart();
        assert!(t.is_failed_by_restart());
        t.metadata = serde_json::Value::String("legacy".into());
        t.mark_restart_failure_recorded();
        assert!(t.is_restart_failure_recorded());
        assert!(!t.is_failed_by_restart());
    }

    #[test]
    fn restart_markers_survive_metadata_round_trip() {
        let mut t = make_thread();
        t.mark_failed_by_restart();
        t.mark_restart_failure_recorded();
        let json = serde_json::to_string(&t).expect("serialize");
        let parsed: Thread = serde_json::from_str(&json).expect("deserialize");
        assert!(parsed.is_failed_by_restart());
        assert!(parsed.is_restart_failure_recorded());
    }
}
