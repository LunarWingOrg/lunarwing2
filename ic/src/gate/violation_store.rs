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
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::bootstrap::lunarwing_base_dir;

const MAX_ACTORS: usize = 1_024;
const MAX_RECORDS_PER_ACTOR: usize = 64;
const MAX_RECORD_FIELD_BYTES: usize = 256;
const MAX_ACTOR_ID_BYTES: usize = 512;
const MAX_PERSISTED_BYTES: u64 = 64 * 1024 * 1024;
const PERSIST_DEBOUNCE: Duration = Duration::from_millis(250);

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
    #[serde(default)]
    updated_at_secs: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ViolationFile {
    agents: HashMap<String, AgentViolations>,
}

#[derive(Debug, Default)]
struct StoreState {
    agents: HashMap<String, AgentViolations>,
    generation: u64,
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
#[derive(Debug)]
pub struct ViolationStore {
    config: ViolationStoreConfig,
    inner: Arc<Mutex<StoreState>>,
    persist_tx: Option<SyncSender<()>>,
    writer: Option<JoinHandle<()>>,
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
        Self::new_with_debounce(path, config, PERSIST_DEBOUNCE)
    }

    fn new_with_debounce(path: PathBuf, config: ViolationStoreConfig, debounce: Duration) -> Self {
        let mut config = config;
        config.max_violations = config.max_violations.clamp(1, MAX_RECORDS_PER_ACTOR as u32);
        let now = now_secs();
        let mut agents = load_from_disk(&path);
        prune_all(&mut agents, now, config.decay_secs);
        for entry in agents.values_mut() {
            cap_records(entry);
            if entry.updated_at_secs == 0 {
                entry.updated_at_secs = entry
                    .records
                    .last()
                    .map_or(0, |record| record.timestamp_secs);
            }
        }
        enforce_actor_cap(&mut agents);
        let inner = Arc::new(Mutex::new(StoreState {
            agents,
            generation: 0,
        }));
        let (persist_tx, persist_rx) = sync_channel(1);
        let writer_state = Arc::clone(&inner);
        let writer = std::thread::Builder::new()
            .name("coord-violation-writer".to_string())
            .spawn(move || writer_loop(path, writer_state, persist_rx, debounce));
        let (persist_tx, writer) = match writer {
            Ok(writer) => (Some(persist_tx), Some(writer)),
            Err(err) => {
                tracing::warn!(error = %err, "Failed to start coordination violation writer");
                (None, None)
            }
        };
        Self {
            config,
            inner,
            persist_tx,
            writer,
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
        self.record_violation_at(agent, reason, hook, channel, now_secs())
    }

    /// `record_violation` with an explicit clock, so tests can exercise
    /// disable-window expiry and re-arming without sleeping.
    fn record_violation_at(
        &self,
        agent: &str,
        reason: &str,
        hook: &str,
        channel: &str,
        now: u64,
    ) -> ViolationStatus {
        if agent.len() > MAX_ACTOR_ID_BYTES {
            tracing::warn!(
                agent_len = agent.len(),
                "Coordination violation actor ID too large"
            );
            return ViolationStatus::Disabled {
                until_secs: now.saturating_add(self.config.disable_duration_secs),
            };
        }
        let mut guard = lock(&self.inner);
        let pruned = prune_all(&mut guard.agents, now, self.config.decay_secs);
        if !guard.agents.contains_key(agent)
            && guard.agents.len() >= MAX_ACTORS
            && !evict_oldest_non_manual(&mut guard.agents)
        {
            if pruned {
                guard.generation = guard.generation.wrapping_add(1);
            }
            drop(guard);
            if pruned {
                self.schedule_persist();
            }
            tracing::warn!(agent, "Coordination violation actor cap reached");
            return ViolationStatus::Disabled {
                until_secs: now.saturating_add(self.config.disable_duration_secs),
            };
        }
        let entry = guard.agents.entry(agent.to_string()).or_default();
        entry.records.push(ViolationRecord {
            timestamp_secs: now,
            reason: bounded_string(reason),
            hook: bounded_string(hook),
            channel: bounded_string(channel),
        });
        entry.updated_at_secs = now;
        cap_records(entry);
        let strikes = entry.records.len() as u32;
        let max_violations = self.config.max_violations.min(MAX_RECORDS_PER_ACTOR as u32);
        if strikes >= max_violations && entry.disabled_until_secs.is_none() {
            entry.disabled_until_secs = Some(now.saturating_add(self.config.disable_duration_secs));
        }
        let status = status_of(entry, now);
        guard.generation = guard.generation.wrapping_add(1);
        drop(guard);
        self.schedule_persist();
        status
    }

    /// Current status for an agent (active violations only; expired disables clear).
    pub fn status(&self, agent: &str) -> ViolationStatus {
        let now = now_secs();
        if agent.len() > MAX_ACTOR_ID_BYTES {
            return ViolationStatus::Disabled {
                until_secs: now.saturating_add(self.config.disable_duration_secs),
            };
        }
        let mut guard = lock(&self.inner);
        let (status, remove, mut changed) = match guard.agents.get_mut(agent) {
            Some(entry) => {
                let changed = prune_decayed(entry, now, self.config.decay_secs);
                let remove = entry.records.is_empty()
                    && !entry.manual_disabled
                    && entry.disabled_until_secs.is_none();
                (status_of(entry, now), remove, changed)
            }
            None => (ViolationStatus::Active, false, false),
        };
        if remove {
            guard.agents.remove(agent);
            changed = true;
        }
        if changed {
            guard.generation = guard.generation.wrapping_add(1);
        }
        drop(guard);
        if changed {
            self.schedule_persist();
        }
        status
    }

    /// Statuses for all tracked agents (admin endpoint).
    pub fn all_statuses(&self) -> HashMap<String, ViolationStatus> {
        let now = now_secs();
        let mut guard = lock(&self.inner);
        let changed = prune_all(&mut guard.agents, now, self.config.decay_secs);
        let statuses = guard
            .agents
            .iter()
            .map(|(agent, entry)| (agent.clone(), status_of(entry, now)))
            .collect();
        if changed {
            guard.generation = guard.generation.wrapping_add(1);
        }
        drop(guard);
        if changed {
            self.schedule_persist();
        }
        statuses
    }

    /// Whether the agent is currently disabled (manual or auto, unexpired).
    pub fn is_disabled(&self, agent: &str) -> bool {
        matches!(self.status(agent), ViolationStatus::Disabled { .. })
    }

    /// Reset all violations and re-enable the agent.
    pub fn clear(&self, agent: &str) {
        let mut guard = lock(&self.inner);
        if guard.agents.remove(agent).is_some() {
            guard.generation = guard.generation.wrapping_add(1);
            drop(guard);
            self.schedule_persist();
        }
    }

    /// Manually disable the agent until [`ViolationStore::manual_enable`] or
    /// [`ViolationStore::clear`].
    pub fn manual_disable(&self, agent: &str) {
        if agent.len() > MAX_ACTOR_ID_BYTES {
            tracing::warn!(agent_len = agent.len(), "Manual-disable actor ID too large");
            return;
        }
        let mut guard = lock(&self.inner);
        let now = now_secs();
        prune_all(&mut guard.agents, now, self.config.decay_secs);
        if !guard.agents.contains_key(agent)
            && guard.agents.len() >= MAX_ACTORS
            && !evict_oldest_non_manual(&mut guard.agents)
        {
            tracing::warn!("Coordination violation actor cap reached; manual disable not stored");
            return;
        }
        let entry = guard.agents.entry(agent.to_string()).or_default();
        entry.manual_disabled = true;
        entry.updated_at_secs = now;
        guard.generation = guard.generation.wrapping_add(1);
        drop(guard);
        self.schedule_persist();
    }

    /// Re-enable a manually disabled agent (violations are kept).
    pub fn manual_enable(&self, agent: &str) {
        let mut guard = lock(&self.inner);
        if let Some(entry) = guard.agents.get_mut(agent) {
            entry.manual_disabled = false;
            entry.disabled_until_secs = None;
            entry.updated_at_secs = now_secs();
            guard.generation = guard.generation.wrapping_add(1);
            drop(guard);
            self.schedule_persist();
        }
    }

    fn schedule_persist(&self) {
        let Some(tx) = &self.persist_tx else {
            return;
        };
        match tx.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => {}
            Err(TrySendError::Disconnected(())) => {
                tracing::warn!("Coordination violation writer stopped unexpectedly");
            }
        }
    }
}

impl Drop for ViolationStore {
    fn drop(&mut self) {
        self.persist_tx.take();
        if let Some(writer) = self.writer.take()
            && writer.join().is_err()
        {
            tracing::warn!("Coordination violation writer panicked");
        }
    }
}

fn lock(mutex: &Mutex<StoreState>) -> std::sync::MutexGuard<'_, StoreState> {
    // Poisoning means another thread panicked mid-update; the in-memory map is
    // still the best available state, so recover it rather than crash.
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn prune_decayed(entry: &mut AgentViolations, now: u64, decay_secs: u64) -> bool {
    let before_records = entry.records.len();
    let before_disabled_until = entry.disabled_until_secs;
    entry
        .records
        .retain(|r| now.saturating_sub(r.timestamp_secs) < decay_secs);
    // An expired automatic disable clears independently of strike decay so a
    // later violation can re-arm the disable window (ISSUE-008). Manual
    // disables carry no timestamp and are unaffected.
    if entry.disabled_until_secs.is_some_and(|until| until <= now) {
        entry.disabled_until_secs = None;
    }
    if entry.records.is_empty() && !entry.manual_disabled {
        entry.disabled_until_secs = None;
    }
    before_records != entry.records.len() || before_disabled_until != entry.disabled_until_secs
}

fn prune_all(agents: &mut HashMap<String, AgentViolations>, now: u64, decay_secs: u64) -> bool {
    let before_agents = agents.len();
    let mut changed = false;
    for entry in agents.values_mut() {
        changed |= prune_decayed(entry, now, decay_secs);
    }
    agents.retain(|_, entry| {
        entry.manual_disabled
            || entry.disabled_until_secs.is_some_and(|until| until > now)
            || !entry.records.is_empty()
    });
    changed || before_agents != agents.len()
}

fn cap_records(entry: &mut AgentViolations) {
    let excess = entry.records.len().saturating_sub(MAX_RECORDS_PER_ACTOR);
    if excess > 0 {
        entry.records.drain(..excess);
    }
}

fn enforce_actor_cap(agents: &mut HashMap<String, AgentViolations>) {
    while agents.len() > MAX_ACTORS {
        if !evict_oldest_non_manual(agents) {
            let oldest = oldest_actor(agents, false);
            if let Some(agent) = oldest {
                agents.remove(&agent);
            }
        }
    }
}

fn evict_oldest_non_manual(agents: &mut HashMap<String, AgentViolations>) -> bool {
    oldest_actor(agents, true).is_some_and(|agent| agents.remove(&agent).is_some())
}

fn oldest_actor(agents: &HashMap<String, AgentViolations>, exclude_manual: bool) -> Option<String> {
    agents
        .iter()
        .filter(|(_, entry)| !exclude_manual || !entry.manual_disabled)
        .min_by(|(agent_a, entry_a), (agent_b, entry_b)| {
            entry_a
                .updated_at_secs
                .cmp(&entry_b.updated_at_secs)
                .then_with(|| agent_a.cmp(agent_b))
        })
        .map(|(agent, _)| agent.clone())
}

fn bounded_string(value: &str) -> String {
    let end = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= MAX_RECORD_FIELD_BYTES)
        .last()
        .unwrap_or(0);
    if value.len() <= MAX_RECORD_FIELD_BYTES {
        value.to_string()
    } else {
        value[..end].to_string()
    }
}

fn status_of(entry: &AgentViolations, now: u64) -> ViolationStatus {
    if entry.manual_disabled {
        return ViolationStatus::Disabled {
            until_secs: u64::MAX,
        };
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

fn load_from_disk(path: &Path) -> HashMap<String, AgentViolations> {
    let content = match std::fs::metadata(path) {
        Ok(metadata) if metadata.len() > MAX_PERSISTED_BYTES => {
            tracing::warn!(
                path = %path.display(),
                size = metadata.len(),
                max = MAX_PERSISTED_BYTES,
                "Coordination violations file exceeds size limit, starting empty"
            );
            return HashMap::new();
        }
        Ok(_) => std::fs::read_to_string(path),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return HashMap::new(),
        Err(err) => Err(err),
    };
    match content {
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
fn writer_loop(path: PathBuf, state: Arc<Mutex<StoreState>>, rx: Receiver<()>, debounce: Duration) {
    let mut persisted_generation = 0;
    let mut dirty = false;
    let mut retry_delay = debounce;
    loop {
        if !dirty && rx.recv().is_err() {
            break;
        }
        let wait = if dirty { retry_delay } else { debounce };
        let disconnected = debounce_notifications(&rx, wait);
        let (generation, agents) = {
            let guard = lock(&state);
            (guard.generation, guard.agents.clone())
        };
        if generation == persisted_generation {
            dirty = false;
        } else if persist(&path, agents) {
            persisted_generation = generation;
            dirty = lock(&state).generation != persisted_generation;
            retry_delay = debounce;
        } else {
            dirty = true;
            retry_delay = retry_delay.saturating_mul(2).min(Duration::from_secs(30));
        }
        if disconnected {
            break;
        }
    }
}

fn debounce_notifications(rx: &Receiver<()>, debounce: Duration) -> bool {
    let deadline = Instant::now() + debounce;
    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return false;
        };
        match rx.recv_timeout(remaining) {
            Ok(()) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return false,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return true,
        }
    }
}

fn persist(path: &Path, agents: HashMap<String, AgentViolations>) -> bool {
    let file = ViolationFile { agents };
    let json = match serde_json::to_vec_pretty(&file) {
        Ok(json) => json,
        Err(err) => {
            tracing::warn!(error = %err, "Failed to serialize coordination violations");
            return false;
        }
    };
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(path = %parent.display(), error = %err, "Failed to create violations dir");
        return false;
    }
    let tmp = path.with_extension("json.tmp");
    if let Err(err) = std::fs::write(&tmp, &json) {
        tracing::warn!(path = %tmp.display(), error = %err, "Failed to write violations temp file");
        return false;
    }
    if let Err(err) = std::fs::rename(&tmp, path) {
        tracing::warn!(path = %path.display(), error = %err, "Failed to rename violations file");
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Unique temp dir per call: PID + nanosecond clock + process-local
    /// counter, so parallel tests in one binary never share a store file.
    fn unique_test_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "coord-violations-{tag}-{}-{nanos}-{seq}",
            std::process::id()
        ))
    }

    fn test_store() -> ViolationStore {
        ViolationStore::new_with_debounce(
            unique_test_dir("store").join("violations.json"),
            ViolationStoreConfig {
                max_violations: 3,
                disable_duration_secs: 3600,
                decay_secs: 600,
            },
            Duration::from_secs(60),
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
        let path = unique_test_dir("roundtrip").join("violations.json");
        let config = ViolationStoreConfig {
            max_violations: 3,
            disable_duration_secs: 3600,
            decay_secs: 600,
        };
        let store = ViolationStore::new(path.clone(), config.clone());
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
        let store = ViolationStore::new(
            unique_test_dir("expiry").join("violations.json"),
            ViolationStoreConfig {
                max_violations: 1,
                disable_duration_secs: 0, // already expired
                decay_secs: 600,
            },
        );
        store.record_violation("agent-e", "r", "hook", "chan");
        assert!(!store.is_disabled("agent-e"));
    }

    #[test]
    fn test_expired_autodisable_rearms_on_new_violation() {
        // ISSUE-008 regression: an expired auto-disable must not block a new
        // disable window for the remaining strike-decay period.
        let store = ViolationStore::new(
            unique_test_dir("rearm").join("violations.json"),
            ViolationStoreConfig {
                max_violations: 3,
                disable_duration_secs: 100,
                decay_secs: 100_000,
            },
        );
        let t0 = 1_000_000;
        store.record_violation_at("agent-f", "r1", "hook", "chan", t0);
        store.record_violation_at("agent-f", "r2", "hook", "chan", t0);
        let status = store.record_violation_at("agent-f", "r3", "hook", "chan", t0);
        assert!(
            matches!(status, ViolationStatus::Disabled { until_secs } if until_secs == t0 + 100)
        );
        // Past the disable window but within strike decay: a new violation
        // re-arms a fresh disable window instead of sitting at FinalWarning.
        let t1 = t0 + 150;
        let status = store.record_violation_at("agent-f", "r4", "hook", "chan", t1);
        assert!(
            matches!(status, ViolationStatus::Disabled { until_secs } if until_secs == t1 + 100)
        );
        // Manual disable is independent of the expiry normalization.
        store.manual_disable("agent-g");
        assert!(store.is_disabled("agent-g"));
    }

    #[test]
    fn test_records_and_actors_are_bounded() {
        let store = test_store();
        let t0 = 1_000_000;
        for index in 0..(MAX_RECORDS_PER_ACTOR + 5) {
            store.record_violation_at("busy", "r", "hook", "chan", t0 + index as u64);
        }
        for index in 0..=MAX_ACTORS {
            store.record_violation_at(
                &format!("agent-{index:04}"),
                "r",
                "hook",
                "chan",
                t0 + MAX_RECORDS_PER_ACTOR as u64,
            );
        }

        let guard = lock(&store.inner);
        assert_eq!(guard.agents.len(), MAX_ACTORS);
        assert_eq!(guard.agents["busy"].records.len(), MAX_RECORDS_PER_ACTOR);
        assert!(!guard.agents.contains_key("agent-0000"));
    }

    #[test]
    fn test_oversized_actor_fails_closed_without_storage() {
        let store = test_store();
        let actor = "a".repeat(MAX_ACTOR_ID_BYTES + 1);

        assert!(matches!(
            store.record_violation(&actor, "reason", "hook", "channel"),
            ViolationStatus::Disabled { .. }
        ));
        assert!(store.is_disabled(&actor));
        assert!(!lock(&store.inner).agents.contains_key(&actor));
    }

    #[test]
    fn test_recording_globally_prunes_expired_empty_actors() {
        let store = test_store();
        let t0 = 1_000_000;
        store.record_violation_at("stale-a", "r", "hook", "chan", t0);
        store.record_violation_at("stale-b", "r", "hook", "chan", t0);
        store.record_violation_at("fresh", "r", "hook", "chan", t0 + 600);

        let guard = lock(&store.inner);
        assert_eq!(guard.agents.len(), 1);
        assert!(guard.agents.contains_key("fresh"));
    }

    #[test]
    fn test_write_behind_uses_immutable_snapshot_and_flushes_on_drop() {
        let path = unique_test_dir("snapshot").join("violations.json");
        let config = ViolationStoreConfig {
            max_violations: 3,
            disable_duration_secs: 3600,
            decay_secs: 600,
        };
        let store = ViolationStore::new_with_debounce(
            path.clone(),
            config.clone(),
            Duration::from_secs(60),
        );
        store.record_violation("agent", "first", "hook", "chan");
        assert!(
            !path.exists(),
            "request path must not persist synchronously"
        );
        let snapshot = lock(&store.inner).agents.clone();
        store.record_violation("agent", "second", "hook", "chan");
        assert_eq!(snapshot["agent"].records.len(), 1);

        drop(store);
        let reopened = ViolationStore::new(path, config);
        assert_eq!(reopened.status("agent"), ViolationStatus::FinalWarning);
    }
}
