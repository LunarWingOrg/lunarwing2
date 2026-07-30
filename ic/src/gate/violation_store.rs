//! Persistent per-agent coordination violation tracking (three-strike enforcement).
//!
//! Part of the multi-agent team coordination pipeline
//! (`docs/proposals/MULTIAGENTTEAMORCHESTRATION_INTEGRATION_PLAN_v2.0.4.0.md`,
//! Phase 3). Coordination hooks call [`ViolationStore::record_violation`] when
//! they reject a message; the store escalates
//! `Active → Warning → FinalWarning → Disabled` and persists to
//! `$LUNARWING_BASE_DIR/coordination-violations.json` via atomic temp+rename,
//! the same pattern as `FileGatePersistence`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::bootstrap::lunarwing_base_dir;

/// Escalation status for an agent's coordination violations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViolationStatus {
    /// No active violations.
    Active,
    /// One strike — silent halt already applied by the rejecting hook.
    Warning,
    /// Two strikes — ops alert fired (structured `tracing::warn!`).
    FinalWarning,
    /// Three or more strikes — agent is disabled until the given unix time.
    Disabled {
        /// Unix timestamp (seconds) when the auto-disable expires.
        until_secs: u64,
    },
}

/// A single recorded coordination violation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViolationRecord {
    /// Unix timestamp (seconds) of the violation.
    pub timestamp_secs: u64,
    /// Human-readable reason from the rejecting hook.
    pub reason: String,
    /// Name of the hook that recorded the violation.
    pub hook: String,
    /// Channel where the violation occurred.
    pub channel: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AgentViolations {
    records: Vec<ViolationRecord>,
    manual_disabled: bool,
    disabled_until_secs: Option<u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ViolationFile {
    agents: HashMap<String, AgentViolations>,
}

/// Tunables for violation escalation.
#[derive(Debug, Clone)]
pub struct ViolationStoreConfig {
    /// Strikes before auto-disable (default 3).
    pub max_violations: u32,
    /// Auto-disable duration in seconds (default 86400 = 24h).
    pub disable_duration_secs: u64,
    /// Violations older than this many seconds are pruned (default 604800 = 7d).
    pub decay_secs: u64,
}

impl Default for ViolationStoreConfig {
    fn default() -> Self {
        Self {
            max_violations: 3,
            disable_duration_secs: 86_400,
            decay_secs: 604_800,
        }
    }
}

impl ViolationStoreConfig {
    /// Load from environment, falling back to defaults.
    ///
    /// - `COORD_VIOLATION_MAX` (default 3)
    /// - `COORD_VIOLATION_DISABLE_DURATION` (default 86400)
    /// - `COORD_VIOLATION_DECAY` (default 604800)
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Some(v) = parse_env_u64("COORD_VIOLATION_MAX") {
            config.max_violations = v as u32;
        }
        if let Some(v) = parse_env_u64("COORD_VIOLATION_DISABLE_DURATION") {
            config.disable_duration_secs = v;
        }
        if let Some(v) = parse_env_u64("COORD_VIOLATION_DECAY") {
            config.decay_secs = v;
        }
        config
    }
}

fn parse_env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.trim().parse().ok()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Persistent violation store shared by coordination hooks and the
/// coordination gate. Cheap to clone: wrap in `Arc`.
pub struct ViolationStore {
    path: PathBuf,
    config: ViolationStoreConfig,
    inner: Mutex<HashMap<String, AgentViolations>>,
}

impl ViolationStore {
    /// Default on-disk location: `$LUNARWING_BASE_DIR/coordination-violations.json`.
    pub fn default_path() -> PathBuf {
        lunarwing_base_dir().join("coordination-violations.json")
    }

    /// Open (or create) the store at the default path.
    pub fn with_default_path(config: ViolationStoreConfig) -> Self {
        Self::new(Self::default_path(), config)
    }

    /// Open (or create) the store at an explicit path, loading existing state.
    pub fn new(path: PathBuf, config: ViolationStoreConfig) -> Self {
        let agents = load_from_disk(&path);
        Self {
            path,
            config,
            inner: Mutex::new(agents),
        }
    }

    /// Record a violation and return the agent's resulting status.
    ///
    /// Strike 3 (or `max_violations`) sets the auto-disable window.
    pub fn record_violation(
        &self,
        agent: &str,
        reason: &str,
        hook: &str,
        channel: &str,
    ) -> ViolationStatus {
        let now = now_secs();
        let mut guard = lock(&self.inner);
        let entry = guard.entry(agent.to_string()).or_default();
        prune_decayed(entry, now, self.config.decay_secs);
        entry.records.push(ViolationRecord {
            timestamp_secs: now,
            reason: reason.to_string(),
            hook: hook.to_string(),
            channel: channel.to_string(),
        });
        let strikes = entry.records.len() as u32;
        if strikes >= self.config.max_violations && entry.disabled_until_secs.is_none() {
            entry.disabled_until_secs = Some(now + self.config.disable_duration_secs);
        }
        let status = status_of(entry, now);
        persist(&self.path, &guard);
        status
    }

    /// Current status for an agent (active violations only; expired disables clear).
    pub fn status(&self, agent: &str) -> ViolationStatus {
        let now = now_secs();
        let guard = lock(&self.inner);
        match guard.get(agent) {
            Some(entry) => status_of(entry, now),
            None => ViolationStatus::Active,
        }
    }

    /// Statuses for all tracked agents (admin endpoint).
    pub fn all_statuses(&self) -> HashMap<String, ViolationStatus> {
        let now = now_secs();
        let guard = lock(&self.inner);
        guard
            .iter()
            .map(|(agent, entry)| (agent.clone(), status_of(entry, now)))
            .collect()
    }

    /// Whether the agent is currently disabled (manual or auto, unexpired).
    pub fn is_disabled(&self, agent: &str) -> bool {
        matches!(self.status(agent), ViolationStatus::Disabled { .. })
            || lock(&self.inner)
                .get(agent)
                .is_some_and(|e| e.manual_disabled)
    }

    /// Reset all violations and re-enable the agent.
    pub fn clear(&self, agent: &str) {
        let mut guard = lock(&self.inner);
        guard.remove(agent);
        persist(&self.path, &guard);
    }

    /// Manually disable the agent until [`ViolationStore::manual_enable`] or
    /// [`ViolationStore::clear`].
    pub fn manual_disable(&self, agent: &str) {
        let mut guard = lock(&self.inner);
        guard.entry(agent.to_string()).or_default().manual_disabled = true;
        persist(&self.path, &guard);
    }

    /// Re-enable a manually disabled agent (violations are kept).
    pub fn manual_enable(&self, agent: &str) {
        let mut guard = lock(&self.inner);
        if let Some(entry) = guard.get_mut(agent) {
            entry.manual_disabled = false;
            entry.disabled_until_secs = None;
        }
        persist(&self.path, &guard);
    }
}

fn lock(
    mutex: &Mutex<HashMap<String, AgentViolations>>,
) -> std::sync::MutexGuard<'_, HashMap<String, AgentViolations>> {
    // Poisoning means another thread panicked mid-update; the in-memory map is
    // still the best available state, so recover it rather than crash.
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn prune_decayed(entry: &mut AgentViolations, now: u64, decay_secs: u64) {
    entry
        .records
        .retain(|r| now.saturating_sub(r.timestamp_secs) < decay_secs);
    if entry.records.is_empty() && !entry.manual_disabled {
        entry.disabled_until_secs = None;
    }
}

fn status_of(entry: &AgentViolations, now: u64) -> ViolationStatus {
    if entry.manual_disabled {
        return ViolationStatus::Disabled { until_secs: u64::MAX };
    }
    if let Some(until) = entry.disabled_until_secs
        && until > now
    {
        return ViolationStatus::Disabled { until_secs: until };
    }
    match entry.records.len() {
        0 => ViolationStatus::Active,
        1 => ViolationStatus::Warning,
        _ => ViolationStatus::FinalWarning,
    }
}

fn load_from_disk(path: &PathBuf) -> HashMap<String, AgentViolations> {
    match std::fs::read_to_string(path) {
        Ok(content) => match serde_json::from_str::<ViolationFile>(&content) {
            Ok(file) => file.agents,
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "Coordination violations file is invalid JSON, starting empty"
                );
                HashMap::new()
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "Failed to read coordination violations file, starting empty"
            );
            HashMap::new()
        }
    }
}

/// Atomic write: serialize to a temp file in the same directory, then rename.
fn persist(path: &PathBuf, agents: &HashMap<String, AgentViolations>) {
    let file = ViolationFile {
        agents: agents.clone(),
    };
    let json = match serde_json::to_vec_pretty(&file) {
        Ok(json) => json,
        Err(err) => {
            tracing::warn!(error = %err, "Failed to serialize coordination violations");
            return;
        }
    };
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(path = %parent.display(), error = %err, "Failed to create violations dir");
        return;
    }
    let tmp = path.with_extension("json.tmp");
    if let Err(err) = std::fs::write(&tmp, &json) {
        tracing::warn!(path = %tmp.display(), error = %err, "Failed to write violations temp file");
        return;
    }
    if let Err(err) = std::fs::rename(&tmp, path) {
        tracing::warn!(path = %path.display(), error = %err, "Failed to rename violations file");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> ViolationStore {
        let dir = std::env::temp_dir().join(format!(
            "coord-violations-test-{}-{}",
            std::process::id(),
            now_secs()
        ));
        ViolationStore::new(
            dir.join("violations.json"),
            ViolationStoreConfig {
                max_violations: 3,
                disable_duration_secs: 3600,
                decay_secs: 600,
            },
        )
    }

    #[test]
    fn test_escalation_active_to_disabled() {
        let store = test_store();
        assert_eq!(store.status("agent-a"), ViolationStatus::Active);
        assert_eq!(
            store.record_violation("agent-a", "r1", "hook", "chan"),
            ViolationStatus::Warning
        );
        assert_eq!(
            store.record_violation("agent-a", "r2", "hook", "chan"),
            ViolationStatus::FinalWarning
        );
        let status = store.record_violation("agent-a", "r3", "hook", "chan");
        assert!(matches!(status, ViolationStatus::Disabled { .. }));
        assert!(store.is_disabled("agent-a"));
    }

    #[test]
    fn test_persistence_roundtrip() {
        let store = test_store();
        let path = store.path.clone();
        let config = ViolationStoreConfig {
            max_violations: 3,
            disable_duration_secs: 3600,
            decay_secs: 600,
        };
        store.record_violation("agent-b", "reason", "hook", "chan");
        drop(store);
        let reopened = ViolationStore::new(path, config);
        assert_eq!(reopened.status("agent-b"), ViolationStatus::Warning);
    }

    #[test]
    fn test_clear_resets() {
        let store = test_store();
        store.record_violation("agent-c", "r", "hook", "chan");
        store.record_violation("agent-c", "r", "hook", "chan");
        store.clear("agent-c");
        assert_eq!(store.status("agent-c"), ViolationStatus::Active);
    }

    #[test]
    fn test_manual_disable_enable() {
        let store = test_store();
        store.manual_disable("agent-d");
        assert!(store.is_disabled("agent-d"));
        store.manual_enable("agent-d");
        assert!(!store.is_disabled("agent-d"));
        assert_eq!(store.status("agent-d"), ViolationStatus::Active);
    }

    #[test]
    fn test_expired_disable_clears() {
        let dir = std::env::temp_dir().join(format!(
            "coord-violations-expiry-{}-{}",
            std::process::id(),
            now_secs()
        ));
        let store = ViolationStore::new(
            dir.join("violations.json"),
            ViolationStoreConfig {
                max_violations: 1,
                disable_duration_secs: 0, // already expired
                decay_secs: 600,
            },
        );
        store.record_violation("agent-e", "r", "hook", "chan");
        assert!(!store.is_disabled("agent-e"));
    }
}
