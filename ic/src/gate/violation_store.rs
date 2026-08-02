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
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

const MAX_TRACKED_AGENTS: usize = 1_024;
const MAX_RECORDS_PER_AGENT: usize = 32;
const MAX_AGENT_ID_BYTES: usize = 512;
const MAX_REASON_BYTES: usize = 2_048;
const MAX_HOOK_BYTES: usize = 256;
const MAX_CHANNEL_BYTES: usize = 1_024;
const MAX_PERSISTED_BYTES: u64 = 16 * 1024 * 1024;
const PERSIST_DEBOUNCE: Duration = Duration::from_millis(250);
const PERSIST_ACK_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// - `COORD_VIOLATION_MAX` (default 3, capped at 64)
    /// - `COORD_VIOLATION_DISABLE_DURATION` (default 86400)
    /// - `COORD_VIOLATION_DECAY` (default 604800)
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Some(v) = parse_env_u64("COORD_VIOLATION_MAX") {
            config.max_violations = v.clamp(1, MAX_RECORDS_PER_AGENT as u64) as u32;
        }
        if let Some(v) = parse_env_u64("COORD_VIOLATION_DISABLE_DURATION") {
            config.disable_duration_secs = v;
        }
        if let Some(v) = parse_env_u64("COORD_VIOLATION_DECAY") {
            config.decay_secs = v;
        }
        config
    }

    fn normalized(mut self) -> Self {
        self.max_violations = self.max_violations.clamp(1, MAX_RECORDS_PER_AGENT as u32);
        self
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

#[derive(Debug)]
enum PersistCommand {
    Dirty,
    Flush(mpsc::Sender<()>),
    Shutdown,
}

/// Persistent violation store shared by coordination hooks and the
/// coordination gate. Cheap to clone: wrap in `Arc`.
#[derive(Debug)]
pub struct ViolationStore {
    path: PathBuf,
    config: ViolationStoreConfig,
    inner: Arc<Mutex<HashMap<String, AgentViolations>>>,
    persist_tx: Option<mpsc::SyncSender<PersistCommand>>,
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
        let config = config.normalized();
        let mut agents = load_from_disk(&path);
        normalize_loaded_agents(&mut agents, now_secs(), config.decay_secs);
        let inner = Arc::new(Mutex::new(agents));
        let (persist_tx, writer) = start_persistence_worker(path.clone(), Arc::clone(&inner));
        Self {
            path,
            config,
            inner,
            persist_tx,
            writer,
        }
    }

    /// Record a violation and return the agent's resulting status.
    ///
    /// Strike 3 (or `max_violations`) sets the auto-disable window. Persistence
    /// is debounced on a dedicated thread; the request path never performs disk I/O.
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
        if agent.len() > MAX_AGENT_ID_BYTES {
            tracing::warn!(
                agent_len = agent.len(),
                "Rejected oversized violation actor ID"
            );
            return self.capacity_status(now);
        }
        let status = {
            let mut guard = lock(&self.inner);
            if !guard.contains_key(agent) && !admit_actor(&mut guard, now, self.config.decay_secs) {
                tracing::warn!("Violation actor limit reached; failing closed for new actor");
                return self.capacity_status(now);
            }
            let entry = guard.entry(agent.to_string()).or_default();
            prune_decayed(entry, now, self.config.decay_secs);
            if entry.records.len() == MAX_RECORDS_PER_AGENT {
                entry.records.remove(0);
            }
            entry.records.push(ViolationRecord {
                timestamp_secs: now,
                reason: bounded_text(reason, MAX_REASON_BYTES),
                hook: bounded_text(hook, MAX_HOOK_BYTES),
                channel: bounded_text(channel, MAX_CHANNEL_BYTES),
            });
            let strikes = entry.records.len() as u32;
            if strikes >= self.config.max_violations && entry.disabled_until_secs.is_none() {
                entry.disabled_until_secs =
                    Some(now.saturating_add(self.config.disable_duration_secs));
            }
            status_of(entry, now, self.config.decay_secs)
        };
        self.schedule_persist();
        status
    }

    /// Current status for an agent (active violations only; expired disables clear).
    pub fn status(&self, agent: &str) -> ViolationStatus {
        let now = now_secs();
        let guard = lock(&self.inner);
        match guard.get(agent) {
            Some(entry) => status_of(entry, now, self.config.decay_secs),
            None => ViolationStatus::Active,
        }
    }

    /// Statuses for all tracked agents (admin endpoint).
    pub fn all_statuses(&self) -> HashMap<String, ViolationStatus> {
        let now = now_secs();
        let guard = lock(&self.inner);
        guard
            .iter()
            .map(|(agent, entry)| (agent.clone(), status_of(entry, now, self.config.decay_secs)))
            .collect()
    }

    /// Whether the agent is currently disabled (manual or auto, unexpired).
    pub fn is_disabled(&self, agent: &str) -> bool {
        matches!(self.status(agent), ViolationStatus::Disabled { .. })
    }

    /// Reset all violations and re-enable the agent.
    pub fn clear(&self, agent: &str) {
        {
            let mut guard = lock(&self.inner);
            guard.remove(agent);
        }
        self.persist_immediately();
    }

    /// Manually disable the agent until [`ViolationStore::manual_enable`] or
    /// [`ViolationStore::clear`].
    pub fn manual_disable(&self, agent: &str) {
        if agent.len() > MAX_AGENT_ID_BYTES {
            tracing::warn!(
                agent_len = agent.len(),
                "Rejected oversized manual-disable actor ID"
            );
            return;
        }
        {
            let mut guard = lock(&self.inner);
            if !guard.contains_key(agent)
                && !make_room_for_admin_actor(&mut guard, now_secs(), self.config.decay_secs)
            {
                tracing::warn!("Violation actor limit reached; manual disable was not recorded");
                return;
            }
            guard.entry(agent.to_string()).or_default().manual_disabled = true;
        }
        self.persist_immediately();
    }

    /// Re-enable a manually disabled agent (violations are kept).
    pub fn manual_enable(&self, agent: &str) {
        {
            let mut guard = lock(&self.inner);
            if let Some(entry) = guard.get_mut(agent) {
                entry.manual_disabled = false;
                entry.disabled_until_secs = None;
            }
        }
        self.persist_immediately();
    }

    fn capacity_status(&self, now: u64) -> ViolationStatus {
        ViolationStatus::Disabled {
            until_secs: now.saturating_add(self.config.disable_duration_secs),
        }
    }

    fn schedule_persist(&self) {
        let Some(tx) = &self.persist_tx else {
            persist_snapshot(&self.path, &self.inner);
            return;
        };
        match tx.try_send(PersistCommand::Dirty) {
            Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
            Err(mpsc::TrySendError::Disconnected(_)) => {
                tracing::warn!("Violation persistence worker stopped; persisting synchronously");
                persist_snapshot(&self.path, &self.inner);
            }
        }
    }

    fn persist_immediately(&self) {
        let Some(tx) = &self.persist_tx else {
            persist_snapshot(&self.path, &self.inner);
            return;
        };
        let (ack_tx, ack_rx) = mpsc::channel();
        if tx.send(PersistCommand::Flush(ack_tx)).is_err()
            || ack_rx.recv_timeout(PERSIST_ACK_TIMEOUT).is_err()
        {
            tracing::warn!("Violation persistence worker unavailable; persisting synchronously");
            persist_snapshot(&self.path, &self.inner);
        }
    }
}

impl Drop for ViolationStore {
    fn drop(&mut self) {
        let shutdown_sent = self
            .persist_tx
            .as_ref()
            .is_some_and(|tx| tx.send(PersistCommand::Shutdown).is_ok());
        let joined = self
            .writer
            .take()
            .is_none_or(|writer| writer.join().is_ok());
        if !shutdown_sent || !joined {
            persist_snapshot(&self.path, &self.inner);
        }
    }
}

fn lock(
    mutex: &Mutex<HashMap<String, AgentViolations>>,
) -> std::sync::MutexGuard<'_, HashMap<String, AgentViolations>> {
    // Poisoning means another thread panicked mid-update; the in-memory map is
    // still the best available state, so recover it rather than crash.
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn prune_decayed(entry: &mut AgentViolations, now: u64, decay_secs: u64) {
    entry
        .records
        .retain(|r| now.saturating_sub(r.timestamp_secs) < decay_secs);
    if entry.records.len() > MAX_RECORDS_PER_AGENT {
        entry
            .records
            .drain(..entry.records.len() - MAX_RECORDS_PER_AGENT);
    }
    // An expired automatic disable clears independently of strike decay so a
    // later violation can re-arm the disable window (ISSUE-008). Manual
    // disables carry no timestamp and are unaffected.
    if entry.disabled_until_secs.is_some_and(|until| until <= now) {
        entry.disabled_until_secs = None;
    }
    if entry.records.is_empty() && !entry.manual_disabled {
        entry.disabled_until_secs = None;
    }
}

fn active_record_count(entry: &AgentViolations, now: u64, decay_secs: u64) -> usize {
    entry
        .records
        .iter()
        .filter(|record| now.saturating_sub(record.timestamp_secs) < decay_secs)
        .count()
}

fn status_of(entry: &AgentViolations, now: u64, decay_secs: u64) -> ViolationStatus {
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
    match active_record_count(entry, now, decay_secs) {
        0 => ViolationStatus::Active,
        1 => ViolationStatus::Warning,
        _ => ViolationStatus::FinalWarning,
    }
}

fn admit_actor(agents: &mut HashMap<String, AgentViolations>, now: u64, decay_secs: u64) -> bool {
    prune_inactive_agents(agents, now, decay_secs);
    agents.len() < MAX_TRACKED_AGENTS
}

fn make_room_for_admin_actor(
    agents: &mut HashMap<String, AgentViolations>,
    now: u64,
    decay_secs: u64,
) -> bool {
    if admit_actor(agents, now, decay_secs) {
        return true;
    }
    let eviction = agents
        .iter()
        .filter(|(_, entry)| !entry.manual_disabled)
        .min_by_key(|(_, entry)| {
            entry
                .records
                .last()
                .map_or(0, |record| record.timestamp_secs)
        })
        .map(|(agent, _)| agent.clone());
    eviction.is_some_and(|agent| agents.remove(&agent).is_some())
}

fn prune_inactive_agents(agents: &mut HashMap<String, AgentViolations>, now: u64, decay_secs: u64) {
    agents.retain(|_, entry| {
        prune_decayed(entry, now, decay_secs);
        entry.manual_disabled || !entry.records.is_empty() || entry.disabled_until_secs.is_some()
    });
}

fn normalize_loaded_agents(
    agents: &mut HashMap<String, AgentViolations>,
    now: u64,
    decay_secs: u64,
) {
    prune_inactive_agents(agents, now, decay_secs);
    agents.retain(|agent, entry| {
        if agent.len() > MAX_AGENT_ID_BYTES {
            return false;
        }
        for record in &mut entry.records {
            record.reason = bounded_text(&record.reason, MAX_REASON_BYTES);
            record.hook = bounded_text(&record.hook, MAX_HOOK_BYTES);
            record.channel = bounded_text(&record.channel, MAX_CHANNEL_BYTES);
        }
        true
    });
    if agents.len() > MAX_TRACKED_AGENTS {
        let mut keys: Vec<_> = agents
            .iter()
            .map(|(agent, entry)| {
                (
                    agent.clone(),
                    entry.manual_disabled,
                    entry
                        .records
                        .last()
                        .map_or(0, |record| record.timestamp_secs),
                )
            })
            .collect();
        keys.sort_by_key(|(_, manual, timestamp)| (*manual, *timestamp));
        for (agent, _, _) in keys.into_iter().take(agents.len() - MAX_TRACKED_AGENTS) {
            agents.remove(&agent);
        }
    }
}

fn bounded_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn start_persistence_worker(
    path: PathBuf,
    inner: Arc<Mutex<HashMap<String, AgentViolations>>>,
) -> (
    Option<mpsc::SyncSender<PersistCommand>>,
    Option<JoinHandle<()>>,
) {
    let (tx, rx) = mpsc::sync_channel(1);
    let writer = std::thread::Builder::new()
        .name("violation-persist".to_string())
        .spawn(move || persistence_worker(&path, &inner, rx));
    match writer {
        Ok(writer) => (Some(tx), Some(writer)),
        Err(err) => {
            tracing::warn!(error = %err, "Failed to start violation persistence worker");
            (None, None)
        }
    }
}

fn persistence_worker(
    path: &PathBuf,
    inner: &Arc<Mutex<HashMap<String, AgentViolations>>>,
    rx: mpsc::Receiver<PersistCommand>,
) {
    while let Ok(command) = rx.recv() {
        match command {
            PersistCommand::Dirty if !debounce_and_persist(path, inner, &rx) => return,
            PersistCommand::Dirty => {}
            PersistCommand::Flush(ack) => {
                persist_snapshot(path, inner);
                let _ = ack.send(());
            }
            PersistCommand::Shutdown => {
                persist_snapshot(path, inner);
                return;
            }
        }
    }
    persist_snapshot(path, inner);
}

/// Returns false when the worker consumed a shutdown/disconnect signal.
fn debounce_and_persist(
    path: &PathBuf,
    inner: &Arc<Mutex<HashMap<String, AgentViolations>>>,
    rx: &mpsc::Receiver<PersistCommand>,
) -> bool {
    let deadline = Instant::now() + PERSIST_DEBOUNCE;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(PersistCommand::Dirty) => {}
            Ok(PersistCommand::Flush(ack)) => {
                persist_snapshot(path, inner);
                let _ = ack.send(());
                return true;
            }
            Ok(PersistCommand::Shutdown) => {
                persist_snapshot(path, inner);
                return false;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                persist_snapshot(path, inner);
                return true;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                persist_snapshot(path, inner);
                return false;
            }
        }
    }
}

fn load_from_disk(path: &PathBuf) -> HashMap<String, AgentViolations> {
    let content = match std::fs::metadata(path) {
        Ok(metadata) if metadata.len() > MAX_PERSISTED_BYTES => {
            tracing::warn!(path = %path.display(), size = metadata.len(), "Coordination violations file exceeds size limit, starting empty");
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
                tracing::warn!(path = %path.display(), error = %err, "Coordination violations file is invalid JSON, starting empty");
                HashMap::new()
            }
        },
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "Failed to read coordination violations file, starting empty");
            HashMap::new()
        }
    }
}

fn persist_snapshot(path: &PathBuf, inner: &Mutex<HashMap<String, AgentViolations>>) {
    let snapshot = lock(inner).clone();
    persist(path, &snapshot);
}

/// Atomic write: serialize to a temp file in the same directory, then rename.
fn persist(path: &PathBuf, agents: &HashMap<String, AgentViolations>) {
    let json = match serde_json::to_vec_pretty(&ViolationFile {
        agents: agents.clone(),
    }) {
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
        ViolationStore::new(
            unique_test_dir("store").join("violations.json"),
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
    fn test_violation_flood_is_bounded() {
        let store = test_store();
        let oversized = "x".repeat(MAX_REASON_BYTES * 2);
        for index in 0..(MAX_TRACKED_AGENTS + 10) {
            store.record_violation(&format!("agent-{index}"), &oversized, "hook", "channel");
        }
        for _ in 0..(MAX_RECORDS_PER_AGENT + 10) {
            store.record_violation("agent-0", &oversized, "hook", "channel");
        }

        let guard = lock(&store.inner);
        assert_eq!(guard.len(), MAX_TRACKED_AGENTS);
        let records = &guard["agent-0"].records;
        assert_eq!(records.len(), MAX_RECORDS_PER_AGENT);
        assert!(
            records
                .iter()
                .all(|record| record.reason.len() == MAX_REASON_BYTES)
        );
    }

    #[test]
    fn test_oversized_actor_fails_closed_without_storage() {
        let store = test_store();
        let actor = "a".repeat(MAX_AGENT_ID_BYTES + 1);
        assert!(matches!(
            store.record_violation(&actor, "reason", "hook", "channel"),
            ViolationStatus::Disabled { .. }
        ));
        assert!(!lock(&store.inner).contains_key(&actor));
    }
}
