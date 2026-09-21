//! Missions — long-running goals that spawn threads over time.
//!
//! A mission represents an ongoing objective that periodically spawns
//! threads to make progress. Missions can run on a schedule (cron),
//! in response to events, or be triggered manually.

use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::project::ProjectId;
use crate::types::thread::ThreadId;

use super::{OwnerId, default_user_id};

/// Strongly-typed mission identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MissionId(pub Uuid);

impl MissionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for MissionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for MissionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Lifecycle status of a mission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MissionStatus {
    /// Mission is actively spawning threads on cadence.
    Active,
    /// Mission is paused — no new threads will be spawned.
    Paused,
    /// Mission has achieved its goal.
    Completed,
    /// Mission has been abandoned or failed irrecoverably.
    Failed,
}

/// How a mission triggers new threads.
///
/// The engine owns cron scheduling and manual fire behavior. The bridge/host
/// connects external webhook and event sources to the matching mission.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MissionCadence {
    /// Spawn on a cron schedule (e.g., "0 */6 * * *" for every 6 hours).
    Cron {
        expression: String,
        timezone: Option<String>,
    },
    /// Spawn in response to a channel message matching a pattern.
    OnEvent { event_pattern: String },
    /// Spawn in response to a structured system event (from tools or external).
    OnSystemEvent { source: String, event_type: String },
    /// Spawn when an external webhook is received at a registered path.
    /// The bridge registers the webhook endpoint and routes payloads here.
    Webhook {
        path: String,
        /// Runtime-only plaintext. Host persistence stores an encrypted secret reference.
        #[serde(default, skip_serializing)]
        secret: Option<String>,
    },
    /// Only spawn when manually triggered (via mission_fire tool or API).
    Manual,
}

impl std::fmt::Debug for MissionCadence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cron {
                expression,
                timezone,
            } => f
                .debug_struct("Cron")
                .field("expression", expression)
                .field("timezone", timezone)
                .finish(),
            Self::OnEvent { event_pattern } => f
                .debug_struct("OnEvent")
                .field("event_pattern", event_pattern)
                .finish(),
            Self::OnSystemEvent { source, event_type } => f
                .debug_struct("OnSystemEvent")
                .field("source", source)
                .field("event_type", event_type)
                .finish(),
            Self::Webhook { path, secret } => f
                .debug_struct("Webhook")
                .field("path", path)
                .field("secret_configured", &secret.is_some())
                .finish(),
            Self::Manual => f.write_str("Manual"),
        }
    }
}

/// A mission — a long-running goal that spawns threads over time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mission {
    pub id: MissionId,
    pub project_id: ProjectId,
    /// Tenant isolation: the user who owns this mission.
    #[serde(default = "default_user_id")]
    pub user_id: String,
    pub name: String,
    pub goal: String,
    pub status: MissionStatus,
    pub cadence: MissionCadence,

    // ── Evolving strategy ──
    /// What the next thread should focus on (updated after each thread).
    pub current_focus: Option<String>,
    /// What approaches have been tried and what happened.
    ///
    /// Bounded to [`MAX_APPROACH_HISTORY`] entries with FIFO eviction (goal
    /// #30 tracker item (d) — full FINAL responses were retained unboundedly,
    /// growing the persisted record and every subsequent meta-prompt).
    /// Evicted entries are summarized into `metadata`; the full thread
    /// transcripts remain on disk.
    pub approach_history: Vec<String>,

    // ── Progress tracking ──
    /// History of threads spawned by this mission.
    pub thread_history: Vec<ThreadId>,
    /// Optional criteria for declaring the mission complete.
    pub success_criteria: Option<String>,

    // ── Notification ──
    /// Channels to notify when a mission thread completes (e.g. "gateway", "repl").
    /// Empty means no proactive notification (results only in approach_history).
    #[serde(default)]
    pub notify_channels: Vec<String>,

    // ── Budget ──
    /// Maximum threads per day (0 = unlimited). New missions without an
    /// explicit value start at [`DEFAULT_MAX_THREADS_PER_DAY`].
    pub max_threads_per_day: u32,
    /// Threads spawned on `threads_today_date`.
    pub threads_today: u32,
    /// UTC date associated with `threads_today`. Missing on older persisted missions.
    #[serde(default)]
    pub threads_today_date: Option<NaiveDate>,

    // ── Trigger payload ──
    /// Payload from the most recent trigger (webhook body, event data, etc.).
    /// Injected into the thread's context so the code can access it.
    pub last_trigger_payload: Option<serde_json::Value>,

    // ── Trigger dedup ──
    /// Dedup keys of trigger payloads this mission has already processed
    /// (see [`trigger_dedup_key`]). Persisted with the mission so re-delivered
    /// events are skipped at trigger time — even across daemon restarts —
    /// instead of spawning a duplicate thread (work item #69). Bounded to
    /// [`MAX_PROCESSED_TRIGGER_KEYS`] with FIFO eviction.
    #[serde(default)]
    pub processed_trigger_keys: Vec<String>,

    /// Last durably reserved insight source batch. Survives listener restarts.
    #[serde(default)]
    pub insight_cursor: Option<crate::types::insight::InsightCursor>,

    /// A deletion tombstone blocks automatic reconciliation and same-name replay.
    #[serde(default)]
    pub deleted_at: Option<DateTime<Utc>>,
    /// Invalidates suspended/in-flight insight writers on opt-out or purge.
    #[serde(default)]
    pub insight_generation: u64,

    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// When the next thread should be spawned (for Cron cadence).
    pub next_fire_at: Option<DateTime<Utc>>,
    /// When this mission last spawned a thread (UTC). `None` means the
    /// mission has never fired. Surfaced in mission listings so an "Active"
    /// status cannot be mistaken for a dispatching one (registry honesty,
    /// work item #143 / WI#109 follow-up).
    #[serde(default)]
    pub last_fired_at: Option<DateTime<Utc>>,

    // ── Retry ──
    /// Automatic retry policy for mission threads that end without
    /// completing — interrupted (signal-stopped, lost join handle) or
    /// terminally failed. `max_retries: 0` disables retry entirely (the
    /// fire is lost, as before work item #188).
    #[serde(default)]
    pub retry_policy: MissionRetryPolicy,
}

/// Default daily thread budget applied when a mission is created without an
/// explicit `max_threads_per_day` (0 on the field = unlimited). An hourly
/// mission needs 24 fires of coverage per day, so the old default of 10 left
/// an unmonitored blind spot (work item #189); 30 covers hourly cadences with
/// headroom for retries. Operators can still set any value, including 0 for
/// unlimited, via `mission_update`.
///
/// Budget/retry interaction (WI#188): every successfully spawned thread
/// increments `threads_today`, so any retry scheme that respawns a thread
/// draws from the same daily budget — one failed fire plus one retry costs
/// two slots. The 30-thread default leaves roughly 6 slots of headroom over
/// an hourly cadence for such respawns. Rollback on spawn failure is the
/// only refund: a thread that fails to spawn refunds its slot (the fire
/// rolls back to the pre-fire mission state), and a trigger re-delivery
/// whose dedup key was already processed spawns nothing and costs nothing.
pub const DEFAULT_MAX_THREADS_PER_DAY: u32 = 30;

/// Automatic retry policy for interrupted or failed mission threads
/// (work item #188). A mission thread that ends without completing —
/// signal-stopped mid-run, lost join handle, or a terminal model/orchestrator
/// failure — previously meant a permanently lost fire (event missions) or a
/// missed cadence window (cron missions). With `max_retries > 0` the manager
/// schedules a delayed re-fire with exponential backoff. Retry state lives in
/// the orchestrator-state metadata of the failing thread
/// (`RetryState::from_metadata`) so a daemon restart can re-arm the pending
/// re-fire without new persisted schema.
///
/// Budget interaction: every retry respawn passes the normal budget gate and
/// increments `threads_today`, so retries draw from the same daily budget —
/// with the old default of 10, an hourly mission plus 3 retries per fire
/// would starve the day by mid-morning (see [`DEFAULT_MAX_THREADS_PER_DAY`]).
/// A budget-exhausted retry is abandoned by its re-fire attempt.
///
/// Redelivery: the outcome processor also releases the trigger dedup key of
/// the failing fire, so an event/webhook payload that is re-delivered after
/// an unsuccessful run can fire again (the rollback contract previously
/// covered spawn failure only).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MissionRetryPolicy {
    /// Max automatic re-fires per failing fire (0 = no retry).
    pub max_retries: u32,
    /// Delay in seconds before the first retry.
    pub initial_delay_secs: u64,
    /// Multiplier applied to the delay after each retry.
    pub backoff_multiplier: f64,
    /// Maximum delay in seconds (caps exponential growth).
    pub max_delay_secs: u64,
}

/// Persisted retry state riding a failing mission thread's orchestrator-state
/// metadata (work item #188). The outcome processor writes it onto the failed
/// thread; the retry sleep task and the post-restart sweep read it back
/// (`metadata["mission_retry"]`), so a pending re-fire survives the daemon
/// lifetime without new mission columns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryState {
    /// 1-based retry attempt currently scheduled (1 = first retry).
    pub attempt: u32,
    /// The trigger payload being re-fired (None for payload-less cadences).
    pub payload: Option<serde_json::Value>,
    /// UTC time after which the re-fire may run.
    pub not_before: DateTime<Utc>,
}

impl RetryState {
    /// Metadata key under which the retry state rides on the failing thread.
    pub const METADATA_KEY: &'static str = "mission_retry";

    /// Read a scheduled retry off a thread's state metadata.
    pub fn from_metadata(metadata: &serde_json::Value) -> Option<Self> {
        metadata
            .get(Self::METADATA_KEY)
            .and_then(|value| serde_json::from_value(value.clone()).ok())
    }

    /// Serialize this state for storage under [`Self::METADATA_KEY`].
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self)
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()))
    }
}

impl Default for MissionRetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay_secs: 60,
            backoff_multiplier: 2.0,
            max_delay_secs: 3600,
        }
    }
}

impl MissionRetryPolicy {
    /// Whether automatic retry is enabled at all (`max_retries > 0`).
    pub fn is_enabled(&self) -> bool {
        self.max_retries > 0
    }

    /// Compute the retry delay for the given 1-based attempt number.
    /// Returns `None` when retries are disabled or the attempt exceeds
    /// `max_retries` (1 = first retry).
    pub fn delay_for_attempt(&self, attempt: u32) -> Option<Duration> {
        if self.max_retries == 0 || attempt == 0 || attempt > self.max_retries {
            return None;
        }
        let delay_secs = (self.initial_delay_secs as f64)
            * self.backoff_multiplier.powi((attempt - 1) as i32);
        if !delay_secs.is_finite() || delay_secs <= 0.0 {
            return Some(Duration::from_secs(self.max_delay_secs));
        }
        let clamped = delay_secs.min(self.max_delay_secs as f64) as u64;
        Some(Duration::from_secs(clamped))
    }
}

/// System event types the engine emits via `fire_on_system_event` (source
/// `engine`). User-facing cadence strings cannot subscribe to these with
/// `event:<pattern>` — that matches channel-message text, which never equals
/// a lifecycle event — so an `OnEvent` pattern equal to one of these names is
/// a mis-registration (the mission could never fire as intended) and is
/// rejected at `create_mission`/`update_mission` (work item #143 / WI#109
/// follow-up). `system:<source>/<event_type>` is the subscribing grammar.
pub const KNOWN_SYSTEM_EVENT_TYPES: &[&str] = &[
    "thread_start",
    "thread_completed_with_issues",
    "thread_completed_with_learnings",
    "conversation_insights_due",
];

/// Maximum number of processed trigger dedup keys retained per mission.
/// Oldest keys are evicted FIFO once the cap is exceeded.
pub const MAX_PROCESSED_TRIGGER_KEYS: usize = 512;

/// Maximum number of approach-history entries retained per mission.
/// Oldest entries are evicted FIFO once the cap is exceeded.
pub const MAX_APPROACH_HISTORY: usize = 20;

/// Derive a stable dedup key from a mission trigger payload.
///
/// Event producers identify re-deliveries with `source_thread_id` (the thread
/// whose completion emitted the event) and/or `content_hash` (a producer-chosen
/// content identity). Returns `None` when the payload carries neither — such
/// fires are never deduplicated (e.g. cron and manual fires pass no payload).
pub fn trigger_dedup_key(payload: &serde_json::Value) -> Option<String> {
    let source = payload
        .get("source_thread_id")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty());
    let hash = payload
        .get("content_hash")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty());
    match (source, hash) {
        (Some(source), Some(hash)) => Some(format!("src:{source}:{hash}")),
        (Some(source), None) => Some(format!("src:{source}")),
        (None, Some(hash)) => Some(format!("hash:{hash}")),
        (None, None) => None,
    }
}

impl Mission {
    pub fn new(
        project_id: ProjectId,
        user_id: impl Into<String>,
        name: impl Into<String>,
        goal: impl Into<String>,
        cadence: MissionCadence,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: MissionId::new(),
            project_id,
            user_id: user_id.into(),
            name: name.into(),
            goal: goal.into(),
            status: MissionStatus::Active,
            cadence,
            current_focus: None,
            approach_history: Vec::new(),
            thread_history: Vec::new(),
            success_criteria: None,
            notify_channels: Vec::new(),
            max_threads_per_day: DEFAULT_MAX_THREADS_PER_DAY,
            threads_today: 0,
            threads_today_date: Some(now.date_naive()),
            last_trigger_payload: None,
            processed_trigger_keys: Vec::new(),
            insight_cursor: None,
            deleted_at: None,
            insight_generation: 0,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
            created_at: now,
            updated_at: now,
            next_fire_at: None,
            last_fired_at: None,
            retry_policy: MissionRetryPolicy::default(),
        }
    }

    pub fn with_success_criteria(mut self, criteria: impl Into<String>) -> Self {
        self.success_criteria = Some(criteria.into());
        self
    }

    pub fn owner_id(&self) -> OwnerId<'_> {
        OwnerId::from_user_id(&self.user_id)
    }

    pub fn is_owned_by(&self, user_id: &str) -> bool {
        self.owner_id().matches_user(user_id)
    }

    /// Record a thread that was spawned for this mission.
    pub fn record_thread(&mut self, thread_id: ThreadId) {
        self.thread_history.push(thread_id);
        self.last_fired_at = Some(Utc::now());
        self.updated_at = Utc::now();
    }

    /// Record an approach-outcome entry, evicting the oldest entries once
    /// [`MAX_APPROACH_HISTORY`] is exceeded. Evicted entries are folded into
    /// a compact summary line so early context is not silently lost — the
    /// full thread transcripts remain on disk.
    pub fn record_approach(&mut self, entry: impl Into<String>) {
        self.approach_history.push(entry.into());
        if self.approach_history.len() > MAX_APPROACH_HISTORY {
            let overflow = self.approach_history.len() - MAX_APPROACH_HISTORY;
            let evicted: Vec<String> = self.approach_history.drain(..overflow).collect();
            self.summarize_evicted_approaches(&evicted);
        }
        self.updated_at = Utc::now();
    }

    /// Compress evicted approach entries into the mission's strategy state.
    ///
    /// Eviction only happens on long-lived missions (10 runs/day default),
    /// so the summary prefix is capped to keep the stored record bounded.
    fn summarize_evicted_approaches(&mut self, evicted: &[String]) {
        if evicted.is_empty() {
            return;
        }
        const SUMMARY_KEY: &str = "early_runs_summary";
        let previous = self
            .metadata
            .get(SUMMARY_KEY)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let mut summary = format!(
            "{previous}{} run(s) evicted from approach history (full transcripts on disk). ",
            evicted.len()
        );
        // Keep the summary itself bounded regardless of how often eviction runs.
        summary.truncate(500);
        if !self.metadata.is_object() {
            self.metadata = serde_json::json!({});
        }
        if let Some(metadata) = self.metadata.as_object_mut() {
            metadata.insert(SUMMARY_KEY.to_string(), serde_json::Value::String(summary));
        }
    }

    /// Whether a trigger payload with this dedup key was already processed.
    pub fn has_processed_trigger(&self, key: &str) -> bool {
        self.processed_trigger_keys.iter().any(|k| k == key)
    }

    /// Mark a trigger dedup key as processed, evicting the oldest keys once
    /// [`MAX_PROCESSED_TRIGGER_KEYS`] is exceeded.
    pub fn record_processed_trigger(&mut self, key: String) {
        if self.has_processed_trigger(&key) {
            return;
        }
        self.processed_trigger_keys.push(key);
        if self.processed_trigger_keys.len() > MAX_PROCESSED_TRIGGER_KEYS {
            let overflow = self.processed_trigger_keys.len() - MAX_PROCESSED_TRIGGER_KEYS;
            self.processed_trigger_keys.drain(..overflow);
        }
    }

    /// Whether the mission is in a terminal state.
    pub fn is_terminal(&self) -> bool {
        self.deleted_at.is_some()
            || matches!(
                self.status,
                MissionStatus::Completed | MissionStatus::Failed
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_mission_defaults_to_thirty_threads_per_day() {
        // WI#189: the old default of 10 gave an hourly mission only 10 hours
        // of daily coverage. 30 covers hourly cadences plus retry headroom.
        let mission = Mission::new(
            ProjectId::new(),
            "test-user",
            "hourly",
            "keep working",
            MissionCadence::Manual,
        );
        assert_eq!(mission.max_threads_per_day, DEFAULT_MAX_THREADS_PER_DAY);
        assert_eq!(DEFAULT_MAX_THREADS_PER_DAY, 30);
    }

    #[test]
    fn legacy_mission_without_budget_date_still_deserializes() {
        let mission = Mission::new(
            ProjectId::new(),
            "test-user",
            "legacy",
            "keep working",
            MissionCadence::Manual,
        );
        let mut value = serde_json::to_value(mission).expect("serialize mission");
        value
            .as_object_mut()
            .expect("mission should serialize as object")
            .remove("threads_today_date");

        let restored: Mission = serde_json::from_value(value).expect("deserialize legacy mission");
        assert_eq!(restored.threads_today_date, None);
    }

    #[test]
    fn legacy_mission_without_retry_policy_still_deserializes_and_defaults() {
        // WI#188: missions persisted before the retry field existed must
        // deserialize unchanged and gain the default retry policy.
        let mission = Mission::new(
            ProjectId::new(),
            "test-user",
            "legacy",
            "keep working",
            MissionCadence::Manual,
        );
        let mut value = serde_json::to_value(mission).expect("serialize mission");
        value
            .as_object_mut()
            .expect("mission should serialize as object")
            .remove("retry_policy");

        let restored: Mission = serde_json::from_value(value).expect("deserialize legacy mission");
        assert_eq!(restored.retry_policy, MissionRetryPolicy::default());
        assert!(restored.retry_policy.is_enabled());
    }

    #[test]
    fn retry_policy_defaults_and_delay_schedule() {
        let policy = MissionRetryPolicy::default();
        assert_eq!(policy.max_retries, 3);
        // WI#188 default schedule: 1m → 2m → 4m (capped at 1h); attempt 0
        // and attempts past max_retries get no delay.
        assert_eq!(policy.delay_for_attempt(0), None);
        assert_eq!(policy.delay_for_attempt(1), Some(Duration::from_secs(60)));
        assert_eq!(policy.delay_for_attempt(2), Some(Duration::from_secs(120)));
        assert_eq!(policy.delay_for_attempt(3), Some(Duration::from_secs(240)));
        assert_eq!(policy.delay_for_attempt(4), None);
    }

    #[test]
    fn retry_policy_disabled_never_retries() {
        let policy = MissionRetryPolicy {
            max_retries: 0,
            ..MissionRetryPolicy::default()
        };
        assert!(!policy.is_enabled());
        assert_eq!(policy.delay_for_attempt(1), None);
    }

    #[test]
    fn retry_policy_delay_is_capped() {
        let policy = MissionRetryPolicy {
            max_retries: 10,
            initial_delay_secs: 60,
            backoff_multiplier: 10.0,
            max_delay_secs: 3600,
        };
        assert_eq!(policy.delay_for_attempt(5), Some(Duration::from_secs(3600)));
    }

    #[test]
    fn legacy_mission_without_processed_trigger_keys_still_deserializes() {
        let mission = Mission::new(
            ProjectId::new(),
            "test-user",
            "legacy",
            "keep working",
            MissionCadence::Manual,
        );
        let mut value = serde_json::to_value(mission).expect("serialize mission");
        value
            .as_object_mut()
            .expect("mission should serialize as object")
            .remove("processed_trigger_keys");

        let restored: Mission = serde_json::from_value(value).expect("deserialize legacy mission");
        assert!(restored.processed_trigger_keys.is_empty());
    }

    #[test]
    fn trigger_dedup_key_combinations() {
        assert_eq!(
            trigger_dedup_key(&serde_json::json!({
                "source_thread_id": "3bfdad4f",
                "content_hash": "abc123",
            })),
            Some("src:3bfdad4f:abc123".to_string())
        );
        assert_eq!(
            trigger_dedup_key(&serde_json::json!({"source_thread_id": "3bfdad4f"})),
            Some("src:3bfdad4f".to_string())
        );
        assert_eq!(
            trigger_dedup_key(&serde_json::json!({"content_hash": "abc123"})),
            Some("hash:abc123".to_string())
        );
        // Empty strings and missing fields yield no key — never dedup those.
        assert_eq!(
            trigger_dedup_key(&serde_json::json!({
                "source_thread_id": "",
                "content_hash": "",
            })),
            None
        );
        assert_eq!(trigger_dedup_key(&serde_json::json!({"goal": "x"})), None);
        // Non-string values are ignored rather than stringified.
        assert_eq!(
            trigger_dedup_key(&serde_json::json!({"source_thread_id": 42})),
            None
        );
    }

    #[test]
    fn processed_trigger_keys_are_deduped_and_bounded() {
        let mut mission = Mission::new(
            ProjectId::new(),
            "test-user",
            "dedup",
            "keep working",
            MissionCadence::Manual,
        );
        assert!(!mission.has_processed_trigger("src:a"));

        mission.record_processed_trigger("src:a".to_string());
        assert!(mission.has_processed_trigger("src:a"));
        // Re-recording the same key does not grow the set.
        mission.record_processed_trigger("src:a".to_string());
        assert_eq!(mission.processed_trigger_keys.len(), 1);

        for i in 0..MAX_PROCESSED_TRIGGER_KEYS + 10 {
            mission.record_processed_trigger(format!("src:k{i}"));
        }
        assert_eq!(
            mission.processed_trigger_keys.len(),
            MAX_PROCESSED_TRIGGER_KEYS
        );
        // Oldest keys evicted FIFO: "src:a" and k0..k9 are gone, k10 remains.
        assert!(!mission.has_processed_trigger("src:a"));
        assert!(!mission.has_processed_trigger("src:k9"));
        assert!(mission.has_processed_trigger("src:k10"));
    }

    #[test]
    fn approach_history_is_bounded_with_summary_of_evictions() {
        // Goal #30 tracker item (d): full FINAL responses were retained
        // unboundedly. The history must FIFO-cap and keep a compact summary
        // of what was evicted.
        let mut mission = Mission::new(
            ProjectId::new(),
            "test-user",
            "bounded",
            "keep working",
            MissionCadence::Manual,
        );
        for i in 0..MAX_APPROACH_HISTORY + 5 {
            mission.record_approach(format!("run {i}"));
        }
        assert_eq!(mission.approach_history.len(), MAX_APPROACH_HISTORY);
        // Oldest evicted FIFO: "run 0".."run 4" are gone, "run 5" remains.
        assert!(!mission.approach_history.contains(&"run 4".to_string()));
        assert_eq!(
            mission.approach_history.first().map(String::as_str),
            Some("run 5")
        );
        // The eviction count is summarized in metadata.
        let summary = mission
            .metadata
            .get("early_runs_summary")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        assert!(summary.contains("run(s) evicted"), "summary was: {summary}");
    }

    #[test]
    fn approach_history_bound_survives_legacy_unbounded_records() {
        // Missions persisted before the bound may carry more entries than
        // MAX_APPROACH_HISTORY; deserialization keeps them intact and the
        // next record_approach trims back to the cap.
        let mission = Mission::new(
            ProjectId::new(),
            "test-user",
            "legacy",
            "keep working",
            MissionCadence::Manual,
        );
        let mut value = serde_json::to_value(mission).expect("serialize mission");
        let oversized: Vec<String> = (0..MAX_APPROACH_HISTORY + 40)
            .map(|i| format!("legacy run {i}"))
            .collect();
        value
            .as_object_mut()
            .expect("mission should serialize as object")
            .insert(
                "approach_history".to_string(),
                serde_json::Value::Array(
                    oversized
                        .into_iter()
                        .map(serde_json::Value::String)
                        .collect(),
                ),
            );

        let mut restored: Mission =
            serde_json::from_value(value).expect("deserialize oversized mission");
        assert!(restored.approach_history.len() > MAX_APPROACH_HISTORY);
        restored.record_approach("new run");
        assert_eq!(restored.approach_history.len(), MAX_APPROACH_HISTORY);
        assert_eq!(
            restored.approach_history.last().map(String::as_str),
            Some("new run")
        );
    }

    #[test]
    fn webhook_secret_deserializes_but_never_serializes_or_debugs() {
        let cadence: MissionCadence = serde_json::from_value(serde_json::json!({
            "Webhook": {
                "path": "build-ready",
                "secret": "plaintext-webhook-secret"
            }
        }))
        .expect("deserialize legacy webhook cadence");

        assert!(matches!(
            &cadence,
            MissionCadence::Webhook { path, secret }
                if path == "build-ready"
                    && secret.as_deref() == Some("plaintext-webhook-secret")
        ));
        let serialized = serde_json::to_string(&cadence).expect("serialize cadence");
        assert!(!serialized.contains("plaintext-webhook-secret"));
        assert!(!serialized.contains("\"secret\""));
        assert!(!format!("{cadence:?}").contains("plaintext-webhook-secret"));
    }
}
