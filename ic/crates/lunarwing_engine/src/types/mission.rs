//! Missions — long-running goals that spawn threads over time.
//!
//! A mission represents an ongoing objective that periodically spawns
//! threads to make progress. Missions can run on a schedule (cron),
//! in response to events, or be triggered manually.

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
    /// Maximum threads per day (0 = unlimited).
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

    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// When the next thread should be spawned (for Cron cadence).
    pub next_fire_at: Option<DateTime<Utc>>,
}

/// Maximum number of processed trigger dedup keys retained per mission.
/// Oldest keys are evicted FIFO once the cap is exceeded.
pub const MAX_PROCESSED_TRIGGER_KEYS: usize = 512;

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
            max_threads_per_day: 10,
            threads_today: 0,
            threads_today_date: Some(now.date_naive()),
            last_trigger_payload: None,
            processed_trigger_keys: Vec::new(),
            metadata: serde_json::Value::Object(serde_json::Map::new()),
            created_at: now,
            updated_at: now,
            next_fire_at: None,
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

    /// Record that a thread was spawned for this mission.
    pub fn record_thread(&mut self, thread_id: ThreadId) {
        self.thread_history.push(thread_id);
        self.updated_at = Utc::now();
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
        matches!(
            self.status,
            MissionStatus::Completed | MissionStatus::Failed
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
