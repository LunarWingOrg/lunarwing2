//! Opt-in additive refresh of tenant knowledge from a published Study Notes checkout.
//!
//! The host updater publishes a generation manifest only after a clean checkout
//! has fast-forwarded and validated. Tenant daemons poll that manifest on their
//! own cadence and import missing paths through the same parser as the one-shot
//! seed. Existing paths are never changed, so locally authored or edited notes
//! remain authoritative. This fallback reports Git-bounded additive freshness;
//! it is not an exact mirror of the hosted Study Notes database.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::workspace::Workspace;
use crate::workspace::knowledge_seed::{
    KnowledgeImportLimits, KnowledgeSeedReport, run_additive_knowledge_import,
};

const STATE_SCHEMA_VERSION: u32 = 1;
const GENERATION_SCHEMA_VERSION: u32 = 1;
const SYNC_STATE_FILE: &str = "knowledge_sync_state.json";
const SYNC_LOCK_FILE: &str = "knowledge_sync.lock";
pub const GENERATION_MANIFEST_RELATIVE_PATH: &str = ".git/lunarwing-study-notes-generation.json";
const MIN_LOOP_DELAY_SECS: u64 = 5;
const INITIAL_BACKOFF_SECS: u64 = 60;
const MAX_BACKOFF_SECS: u64 = 6 * 60 * 60;
const MAX_MANIFEST_BYTES: u64 = 4_096;

/// Configuration for checkout-backed additive knowledge refresh.
#[derive(Debug, Clone)]
pub struct KnowledgeSyncConfig {
    /// Whether periodic refresh is enabled.
    pub enabled: bool,
    /// Published lw2studymcp checkout to scan.
    pub source_home: PathBuf,
    /// Directory containing cadence state and the process lock.
    pub state_dir: PathBuf,
    /// Normal interval between generation checks.
    pub cadence_hours: u32,
    /// Maximum new notes imported in one bounded pass.
    pub max_records_per_run: u32,
    /// Maximum source bytes imported in one bounded pass.
    pub max_bytes_per_run: usize,
}

/// Machine-readable result from one automatic or operator-triggered pass.
#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeSyncReport {
    pub mode: &'static str,
    pub successful: bool,
    pub skipped: bool,
    pub skip_reason: Option<&'static str>,
    pub error_class: Option<&'static str>,
    pub source_revision: Option<String>,
    pub notes_found: u32,
    pub notes_added: u32,
    pub notes_existing: u32,
    pub notes_deferred: u32,
    pub notes_failed: u32,
    pub bytes_added: usize,
    pub next_run_after_secs: u64,
}

impl KnowledgeSyncReport {
    fn skipped(reason: &'static str, next_run_after_secs: u64) -> Self {
        Self {
            mode: "additive",
            successful: true,
            skipped: true,
            skip_reason: Some(reason),
            error_class: None,
            source_revision: None,
            notes_found: 0,
            notes_added: 0,
            notes_existing: 0,
            notes_deferred: 0,
            notes_failed: 0,
            bytes_added: 0,
            next_run_after_secs,
        }
    }

    fn failed(error_class: &'static str, next_run_after_secs: u64) -> Self {
        Self {
            mode: "additive",
            successful: false,
            skipped: false,
            skip_reason: None,
            error_class: Some(error_class),
            source_revision: None,
            notes_found: 0,
            notes_added: 0,
            notes_existing: 0,
            notes_deferred: 0,
            notes_failed: 0,
            bytes_added: 0,
            next_run_after_secs,
        }
    }

    fn from_import(
        revision: String,
        import: KnowledgeSeedReport,
        next_run_after_secs: u64,
    ) -> Self {
        Self {
            mode: "additive",
            successful: import.notes_failed == 0,
            skipped: false,
            skip_reason: None,
            error_class: (import.notes_failed > 0).then_some("note_import_failed"),
            source_revision: Some(revision),
            notes_found: import.notes_found,
            notes_added: import.notes_seeded,
            notes_existing: import.notes_skipped_existing,
            notes_deferred: import.notes_deferred,
            notes_failed: import.notes_failed,
            bytes_added: import.bytes_seeded,
            next_run_after_secs,
        }
    }
}

#[derive(Debug, Deserialize)]
struct PublishedGeneration {
    schema_version: u32,
    source_revision: String,
    published_at: DateTime<Utc>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct KnowledgeSyncState {
    schema_version: u32,
    completed_source_revision: Option<String>,
    last_attempt_at: Option<DateTime<Utc>>,
    last_success_at: Option<DateTime<Utc>>,
    consecutive_failures: u32,
    last_error_class: Option<String>,
    notes_added_last_run: u32,
    notes_deferred_last_run: u32,
}

struct SyncLock {
    _file: File,
}

impl SyncLock {
    fn acquire(state_dir: &Path) -> Result<Self, std::fs::TryLockError> {
        std::fs::create_dir_all(state_dir).map_err(std::fs::TryLockError::Error)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(state_dir.join(SYNC_LOCK_FILE))
            .map_err(std::fs::TryLockError::Error)?;
        file.try_lock()?;
        Ok(Self { _file: file })
    }
}

/// Run one additive refresh pass.
///
/// A changed published generation bypasses the normal cadence. `force` bypasses
/// both the cadence and unchanged-generation short-circuit, which is used by the
/// operator CLI. Failures retain the prior completed revision so replay remains
/// idempotent.
pub async fn run_knowledge_sync_once(
    workspace: &Workspace,
    config: &KnowledgeSyncConfig,
    force: bool,
) -> KnowledgeSyncReport {
    let cadence_secs = u64::from(config.cadence_hours).saturating_mul(3_600);
    if !config.enabled {
        return KnowledgeSyncReport::skipped("disabled", cadence_secs);
    }
    let _lock = match SyncLock::acquire(&config.state_dir) {
        Ok(lock) => lock,
        Err(std::fs::TryLockError::WouldBlock) => {
            return KnowledgeSyncReport::skipped("already_running", MIN_LOOP_DELAY_SECS);
        }
        Err(std::fs::TryLockError::Error(error)) => {
            tracing::warn!("knowledge sync: could not acquire process lock: {error}");
            return KnowledgeSyncReport::failed("lock_unavailable", INITIAL_BACKOFF_SECS);
        }
    };

    let mut state = match load_state(&config.state_dir.join(SYNC_STATE_FILE)) {
        Ok(state) => state.unwrap_or_default(),
        Err(error) => {
            tracing::warn!("knowledge sync: state is unreadable: {error}");
            return KnowledgeSyncReport::failed("invalid_state", INITIAL_BACKOFF_SECS);
        }
    };
    let generation = match load_generation(&config.source_home) {
        Ok(generation) => generation,
        Err(error) => {
            tracing::warn!("knowledge sync: published generation unavailable: {error}");
            return record_failure(config, state, "source_unavailable", None);
        }
    };

    if !force && state.completed_source_revision.as_deref() == Some(&generation.source_revision) {
        return KnowledgeSyncReport::skipped("source_unchanged", cadence_secs);
    }

    state.schema_version = STATE_SCHEMA_VERSION;
    state.last_attempt_at = Some(Utc::now());
    if let Err(error) = save_state(&config.state_dir.join(SYNC_STATE_FILE), &state) {
        tracing::warn!("knowledge sync: failed to record attempt: {error}");
        return KnowledgeSyncReport::failed("state_write_failed", INITIAL_BACKOFF_SECS);
    }

    let import = run_additive_knowledge_import(
        workspace,
        &config.source_home,
        KnowledgeImportLimits {
            max_new_notes: config.max_records_per_run,
            max_new_bytes: config.max_bytes_per_run,
        },
    )
    .await;
    let after = match load_generation(&config.source_home) {
        Ok(after) => after,
        Err(error) => {
            tracing::warn!("knowledge sync: generation disappeared during scan: {error}");
            return record_failure(config, state, "source_changed", Some(import));
        }
    };
    if after.source_revision != generation.source_revision
        || after.published_at != generation.published_at
    {
        tracing::warn!("knowledge sync: source generation changed during additive scan");
        return record_failure(config, state, "source_changed", Some(import));
    }
    if import.skipped {
        // A healthy skip (e.g. no notes dir yet) is not a failure: it must not
        // grow the failure/backoff counters.
        state.notes_added_last_run = import.notes_seeded;
        state.notes_deferred_last_run = import.notes_deferred;
        if let Err(error) = save_state(&config.state_dir.join(SYNC_STATE_FILE), &state) {
            tracing::warn!("knowledge sync: failed to save skipped pass: {error}");
        }
        let mut report = KnowledgeSyncReport::skipped("import_skipped", cadence_secs);
        report.source_revision = Some(generation.source_revision);
        report.notes_found = import.notes_found;
        report.notes_added = import.notes_seeded;
        report.notes_existing = import.notes_skipped_existing;
        report.notes_deferred = import.notes_deferred;
        report.notes_failed = import.notes_failed;
        report.bytes_added = import.bytes_seeded;
        return report;
    }
    if import.notes_failed > 0 {
        return record_failure(config, state, "note_import_failed", Some(import));
    }

    let has_more = import.notes_deferred > 0;
    state.last_success_at = Some(Utc::now());
    state.consecutive_failures = 0;
    state.last_error_class = None;
    state.notes_added_last_run = import.notes_seeded;
    state.notes_deferred_last_run = import.notes_deferred;
    if !has_more {
        state.completed_source_revision = Some(generation.source_revision.clone());
    }
    if let Err(error) = save_state(&config.state_dir.join(SYNC_STATE_FILE), &state) {
        tracing::warn!("knowledge sync: failed to save successful pass: {error}");
        return KnowledgeSyncReport::failed("state_write_failed", INITIAL_BACKOFF_SECS);
    }

    let delay = if has_more {
        MIN_LOOP_DELAY_SECS
    } else {
        cadence_secs
    };
    KnowledgeSyncReport::from_import(generation.source_revision, import, delay)
}

/// Run the independent daemon cadence until the process runtime shuts down.
pub async fn run_knowledge_sync_loop(workspace: Arc<Workspace>, config: KnowledgeSyncConfig) {
    let initial_delay = initial_delay_secs(&config, workspace.user_id());
    tokio::time::sleep(Duration::from_secs(initial_delay)).await;

    loop {
        let report = run_knowledge_sync_once(&workspace, &config, false).await;
        if report.successful && report.notes_added > 0 {
            tracing::info!(
                mode = report.mode,
                source_revision = report.source_revision.as_deref().unwrap_or("unknown"),
                notes_added = report.notes_added,
                notes_existing = report.notes_existing,
                notes_deferred = report.notes_deferred,
                "knowledge sync: additive refresh complete"
            );
        } else if !report.successful {
            tracing::warn!(
                error_class = report.error_class.unwrap_or("unknown"),
                retry_after_secs = report.next_run_after_secs,
                "knowledge sync: additive refresh failed; serving last good corpus"
            );
        }
        tokio::time::sleep(Duration::from_secs(
            report.next_run_after_secs.max(MIN_LOOP_DELAY_SECS),
        ))
        .await;
    }
}

fn initial_delay_secs(config: &KnowledgeSyncConfig, identity: &str) -> u64 {
    let cadence_secs = u64::from(config.cadence_hours).saturating_mul(3_600);
    let jitter_cap = (cadence_secs / 10).clamp(MIN_LOOP_DELAY_SECS, 300);
    let jitter = deterministic_jitter_secs(identity, jitter_cap);
    let Ok(Some(state)) = load_state(&config.state_dir.join(SYNC_STATE_FILE)) else {
        return jitter;
    };
    let scheduled = if state.consecutive_failures > 0 {
        state.last_attempt_at.map(|attempt| {
            (
                attempt,
                failure_delay_secs(config, state.consecutive_failures),
            )
        })
    } else {
        state.last_success_at.map(|success| (success, cadence_secs))
    };
    let Some((anchor, interval)) = scheduled else {
        return jitter;
    };
    let elapsed = Utc::now().signed_duration_since(anchor).num_seconds();
    let Ok(elapsed) = u64::try_from(elapsed) else {
        return interval;
    };
    if elapsed >= interval {
        jitter
    } else {
        interval - elapsed
    }
}

fn load_generation(source_home: &Path) -> Result<PublishedGeneration, String> {
    let path = source_home.join(GENERATION_MANIFEST_RELATIVE_PATH);
    let metadata =
        std::fs::symlink_metadata(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(format!("{} exceeds size limit", path.display()));
    }
    let raw =
        std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let generation: PublishedGeneration =
        serde_json::from_str(&raw).map_err(|error| format!("{}: {error}", path.display()))?;
    if generation.schema_version != GENERATION_SCHEMA_VERSION {
        return Err(format!(
            "{} has unsupported schema version {}",
            path.display(),
            generation.schema_version
        ));
    }
    if !(7..=128).contains(&generation.source_revision.len())
        || !generation
            .source_revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(format!("{} has invalid source revision", path.display()));
    }
    if !source_home.join("agent-notes-by-agent").is_dir() {
        return Err(format!(
            "{} has no agent-notes-by-agent directory",
            source_home.display()
        ));
    }
    Ok(generation)
}

fn load_state(path: &Path) -> Result<Option<KnowledgeSyncState>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let state: KnowledgeSyncState =
        serde_json::from_str(&raw).map_err(|error| format!("{}: {error}", path.display()))?;
    if state.schema_version != STATE_SCHEMA_VERSION {
        return Err(format!(
            "{} has unsupported schema version {}",
            path.display(),
            state.schema_version
        ));
    }
    Ok(Some(state))
}

fn save_state(path: &Path, state: &KnowledgeSyncState) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err("knowledge sync state path has no parent".to_string());
    };
    std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    let json = serde_json::to_vec_pretty(state).map_err(|error| error.to_string())?;
    let nonce = Utc::now().timestamp_nanos_opt().unwrap_or_default();
    let temp = parent.join(format!(
        ".{SYNC_STATE_FILE}.{}.{nonce}.tmp",
        std::process::id()
    ));
    let mut file = File::create(&temp).map_err(|error| format!("{}: {error}", temp.display()))?;
    file.write_all(&json)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("{}: {error}", temp.display()))?;
    std::fs::rename(&temp, path).map_err(|error| format!("{}: {error}", path.display()))?;
    if let Ok(parent_file) = File::open(parent) {
        let _ = parent_file.sync_all();
    }
    Ok(())
}

fn record_failure(
    config: &KnowledgeSyncConfig,
    mut state: KnowledgeSyncState,
    error_class: &'static str,
    import: Option<KnowledgeSeedReport>,
) -> KnowledgeSyncReport {
    state.schema_version = STATE_SCHEMA_VERSION;
    state.last_attempt_at = Some(Utc::now());
    state.consecutive_failures = state.consecutive_failures.saturating_add(1);
    state.last_error_class = Some(error_class.to_string());
    if let Some(import) = &import {
        state.notes_added_last_run = import.notes_seeded;
        state.notes_deferred_last_run = import.notes_deferred;
    }
    let delay = failure_delay_secs(config, state.consecutive_failures);
    if let Err(error) = save_state(&config.state_dir.join(SYNC_STATE_FILE), &state) {
        tracing::warn!("knowledge sync: failed to save failure state: {error}");
    }
    let mut report = KnowledgeSyncReport::failed(error_class, delay);
    if let Some(import) = import {
        report.notes_found = import.notes_found;
        report.notes_added = import.notes_seeded;
        report.notes_existing = import.notes_skipped_existing;
        report.notes_deferred = import.notes_deferred;
        report.notes_failed = import.notes_failed;
        report.bytes_added = import.bytes_seeded;
    }
    report
}

fn failure_backoff_secs(consecutive_failures: u32) -> u64 {
    let shift = consecutive_failures.saturating_sub(1).min(16);
    INITIAL_BACKOFF_SECS
        .saturating_mul(1_u64 << shift)
        .min(MAX_BACKOFF_SECS)
}

fn failure_delay_secs(config: &KnowledgeSyncConfig, consecutive_failures: u32) -> u64 {
    let backoff = failure_backoff_secs(consecutive_failures);
    let jitter_cap = (backoff / 10).max(1);
    let identity = config.source_home.to_string_lossy();
    backoff
        .saturating_add(deterministic_jitter_secs(&identity, jitter_cap))
        .min(MAX_BACKOFF_SECS)
}

fn deterministic_jitter_secs(identity: &str, cap: u64) -> u64 {
    let hash = identity.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
        hash.wrapping_mul(0x100000001b3) ^ u64::from(byte)
    });
    hash % cap.max(1)
}

/// Deterministic startup jitter shared by the knowledge cadence loops, so
/// tenants on one host stagger their background passes.
pub fn shared_deterministic_jitter_secs(identity: &str, cap: u64) -> u64 {
    deterministic_jitter_secs(identity, cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_generation(home: &Path, revision: &str, published_at: &str) {
        let manifest = home.join(GENERATION_MANIFEST_RELATIVE_PATH);
        std::fs::create_dir_all(manifest.parent().expect("manifest parent")).expect("git dir");
        std::fs::write(
            manifest,
            serde_json::json!({
                "schema_version": 1,
                "source_revision": revision,
                "published_at": published_at,
            })
            .to_string(),
        )
        .expect("manifest");
    }

    #[test]
    fn backoff_is_bounded() {
        assert_eq!(failure_backoff_secs(1), 60);
        assert_eq!(failure_backoff_secs(2), 120);
        assert_eq!(failure_backoff_secs(30), MAX_BACKOFF_SECS);
    }

    #[test]
    fn jitter_is_stable_and_bounded() {
        assert_eq!(
            deterministic_jitter_secs("alice", 300),
            deterministic_jitter_secs("alice", 300)
        );
        assert!(deterministic_jitter_secs("alice", 300) < 300);
    }

    #[test]
    fn startup_delay_resumes_persisted_cadence_and_backoff() {
        let state_dir = tempfile::tempdir().expect("state");
        let config = KnowledgeSyncConfig {
            enabled: true,
            source_home: PathBuf::from("/source/alice"),
            state_dir: state_dir.path().to_path_buf(),
            cadence_hours: 1,
            max_records_per_run: 1,
            max_bytes_per_run: 1,
        };
        let mut state = KnowledgeSyncState {
            schema_version: STATE_SCHEMA_VERSION,
            last_success_at: Some(Utc::now()),
            ..Default::default()
        };
        save_state(&state_dir.path().join(SYNC_STATE_FILE), &state).expect("success state");
        assert!(initial_delay_secs(&config, "alice") > 3_500);

        state.last_attempt_at = Some(Utc::now());
        state.consecutive_failures = 2;
        save_state(&state_dir.path().join(SYNC_STATE_FILE), &state).expect("failure state");
        let delay = initial_delay_secs(&config, "alice");
        assert!(delay >= failure_backoff_secs(2));
        assert!(delay <= failure_delay_secs(&config, 2));
    }

    #[test]
    fn generation_requires_valid_revision_and_notes_tree() {
        let fixture = tempfile::tempdir().expect("fixture");
        write_generation(fixture.path(), "not-a-revision", "2026-09-09T00:00:00Z");
        assert!(load_generation(fixture.path()).is_err());
        std::fs::create_dir(fixture.path().join("agent-notes-by-agent")).expect("notes dir");
        write_generation(fixture.path(), "abcdef1234567", "2026-09-09T00:00:00Z");
        assert!(load_generation(fixture.path()).is_ok());
    }

    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn published_generations_refresh_additively_in_bounded_passes() {
        let (db, _db_dir) = crate::testing::test_db().await;
        let workspace = Workspace::new_with_db("alice", db);
        let fixture = tempfile::tempdir().expect("fixture");
        let notes = fixture.path().join("agent-notes-by-agent/rarity");
        std::fs::create_dir_all(&notes).expect("notes");
        for topic in ["a", "b"] {
            std::fs::write(
                notes.join(format!("{topic}.md")),
                format!("# {topic}\n\nsource {topic}\n"),
            )
            .expect("note");
        }
        write_generation(fixture.path(), "aaaaaaaaaaaa", "2026-09-09T00:00:00Z");
        let state_dir = tempfile::tempdir().expect("state");
        let config = KnowledgeSyncConfig {
            enabled: true,
            source_home: fixture.path().to_path_buf(),
            state_dir: state_dir.path().to_path_buf(),
            cadence_hours: 6,
            max_records_per_run: 1,
            max_bytes_per_run: 1_024,
        };

        let first = run_knowledge_sync_once(&workspace, &config, false).await;
        assert!(first.successful);
        assert_eq!(first.notes_added, 1);
        assert_eq!(first.notes_deferred, 1);
        let second = run_knowledge_sync_once(&workspace, &config, false).await;
        assert!(second.successful);
        assert_eq!(second.notes_added, 1);
        assert_eq!(second.notes_deferred, 0);
        let unchanged = run_knowledge_sync_once(&workspace, &config, false).await;
        assert!(unchanged.skipped);
        assert_eq!(unchanged.skip_reason, Some("source_unchanged"));

        workspace
            .write("study-notes/rarity/a.md", "# Local\n\noperator edit\n")
            .await
            .expect("operator edit");
        std::fs::write(notes.join("c.md"), "# c\n\nsource c\n").expect("new note");
        write_generation(fixture.path(), "bbbbbbbbbbbb", "2026-09-09T01:00:00Z");
        let refreshed = run_knowledge_sync_once(&workspace, &config, false).await;
        assert!(refreshed.successful);
        assert_eq!(refreshed.notes_added, 1);
        let local = workspace
            .read_primary("study-notes/rarity/a.md")
            .await
            .expect("local note");
        assert!(local.content.contains("operator edit"));
        assert!(
            workspace
                .read_primary("study-notes/rarity/c.md")
                .await
                .is_ok()
        );
    }
}
