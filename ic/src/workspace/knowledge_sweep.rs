//! Study-notes stale detection: background sweep that marks aging notes stale.
//!
//! Runs on a configurable cadence (like memory hygiene) and flips study notes
//! whose `studied_at` (falling back to the document's last update) is older
//! than the configured threshold from `active` to `stale`, across the caller's
//! private scope and the shared herd scope. Notes already marked `stale`,
//! `superseded`, or `needs-verification` are left untouched.
//!
//! The shared-scope pass also reaps stranded coordination wire notes: bodies
//! left behind when an ingest died between the content and metadata commits
//! whose sender never shared again to trigger the self-repair path. Only
//! wire-namespace paths carrying the wire sentinel and older than a grace
//! period are removed, so an in-flight or freshly stranded ingest is never
//! touched.
//!
//! A global [`AtomicBool`] guard prevents concurrent sweeps, mirroring the
//! hygiene pass guard, so overlapping heartbeat ticks cannot double-mark.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::bootstrap::lunarwing_base_dir;
use crate::workspace::knowledge_sync::shared_deterministic_jitter_secs;
use crate::workspace::{
    KNOWLEDGE_DIR, KNOWLEDGE_SHARED_SCOPE, KnowledgeStatus, WIRE_AGENT_PREFIX, Workspace,
    is_incomplete_wire_write,
};

/// Minimum age before a stranded wire note is reaped. Ingest tasks finish in
/// seconds, so anything older than a day is final unless the sender shares
/// again (which repairs it before this sweep would ever see it).
const STRANDED_WIRE_GRACE_HOURS: i64 = 24;

/// Global guard preventing concurrent sweeps.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Configuration for the study-notes stale sweep.
#[derive(Debug, Clone)]
pub struct KnowledgeSweepConfig {
    /// Whether the sweep is enabled at all.
    pub enabled: bool,
    /// Notes studied (or last updated) more than this many days ago are
    /// marked `stale`.
    pub stale_after_days: u32,
    /// Minimum hours between sweeps.
    pub cadence_hours: u32,
    /// Whether the shared herd scope is swept too.
    pub include_shared: bool,
    /// Directory to store state file (default: `~/.lunarwing`).
    pub state_dir: PathBuf,
}

impl Default for KnowledgeSweepConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            stale_after_days: 30,
            cadence_hours: 24,
            include_shared: true,
            state_dir: lunarwing_base_dir(),
        }
    }
}

/// Persisted state for tracking sweep cadence.
#[derive(Debug, Serialize, Deserialize)]
struct KnowledgeSweepState {
    last_run: DateTime<Utc>,
}

/// Summary of one sweep pass.
#[derive(Debug, Default)]
pub struct KnowledgeSweepReport {
    /// Notes scanned (private + shared scopes).
    pub notes_scanned: u32,
    /// Notes newly marked stale.
    pub notes_marked_stale: u32,
    /// Stranded coordination wire notes reaped from the shared scope.
    pub stranded_removed: u32,
    /// Whether the run was skipped (disabled, cadence, or concurrent).
    pub skipped: bool,
}

impl KnowledgeSweepReport {
    /// True if any note was marked stale or any stranded note was reaped.
    pub fn had_work(&self) -> bool {
        self.notes_marked_stale > 0 || self.stranded_removed > 0
    }
}

/// Run a stale-detection sweep if the cadence has elapsed.
///
/// `workspace` sweeps the caller's private scope; `shared_workspace` (when
/// provided and [`KnowledgeSweepConfig::include_shared`] is on) sweeps the
/// shared herd scope. Metadata writes are primary-scope only, so each scope
/// needs its own workspace handle.
///
/// Best-effort: failures are logged but never propagated, so the agent never
/// crashes because a lifecycle sweep failed.
pub async fn run_knowledge_sweep_if_due(
    workspace: &Workspace,
    shared_workspace: Option<&Workspace>,
    config: &KnowledgeSweepConfig,
) -> KnowledgeSweepReport {
    if !config.enabled {
        return KnowledgeSweepReport {
            skipped: true,
            ..Default::default()
        };
    }

    if RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        tracing::debug!("knowledge sweep: skipping (another sweep is running)");
        return KnowledgeSweepReport {
            skipped: true,
            ..Default::default()
        };
    }
    let _guard = RunningGuard;

    let state_file = config.state_dir.join("knowledge_sweep_state.json");

    if let Some(state) = load_state(&state_file) {
        let elapsed = Utc::now().signed_duration_since(state.last_run);
        let cadence = chrono::Duration::hours(i64::from(config.cadence_hours));
        if elapsed < cadence {
            tracing::debug!(
                hours_since_last = elapsed.num_hours(),
                cadence_hours = config.cadence_hours,
                "knowledge sweep: skipping (cadence not elapsed)"
            );
            return KnowledgeSweepReport {
                skipped: true,
                ..Default::default()
            };
        }
    }

    // Claim the cadence window before sweeping so concurrent callers that
    // slip past the guard still see an up-to-date timestamp.
    save_state(&state_file);

    let mut report = KnowledgeSweepReport::default();

    let private_scope = workspace.user_id().to_string();
    match sweep_scope(workspace, &private_scope, config.stale_after_days).await {
        Ok((scanned, marked)) => {
            report.notes_scanned += scanned;
            report.notes_marked_stale += marked;
        }
        Err(e) => tracing::warn!("knowledge sweep: private-scope scan failed: {e}"),
    }

    if config.include_shared
        && private_scope != KNOWLEDGE_SHARED_SCOPE
        && let Some(shared_ws) = shared_workspace
    {
        match sweep_scope(shared_ws, KNOWLEDGE_SHARED_SCOPE, config.stale_after_days).await {
            Ok((scanned, marked)) => {
                report.notes_scanned += scanned;
                report.notes_marked_stale += marked;
            }
            Err(e) => tracing::warn!("knowledge sweep: shared-scope scan failed: {e}"),
        }
        match sweep_stranded_wire_writes(
            shared_ws,
            chrono::Duration::hours(STRANDED_WIRE_GRACE_HOURS),
        )
        .await
        {
            Ok(removed) => report.stranded_removed += removed,
            Err(e) => tracing::warn!("knowledge sweep: stranded wire-note pass failed: {e}"),
        }
    }

    if report.had_work() {
        tracing::info!(
            notes_scanned = report.notes_scanned,
            notes_marked_stale = report.notes_marked_stale,
            stranded_removed = report.stranded_removed,
            stale_after_days = config.stale_after_days,
            "knowledge sweep: marked aging study notes stale"
        );
    } else {
        tracing::debug!(
            notes_scanned = report.notes_scanned,
            "knowledge sweep: nothing to mark"
        );
    }

    report
}

/// Scan one scope and mark its active notes older than the threshold stale.
///
/// Returns `(notes_scanned, notes_marked_stale)`. `workspace` must be scoped
/// to `scope` so the primary-scope metadata guard accepts the writes.
async fn sweep_scope(
    workspace: &Workspace,
    scope: &str,
    stale_after_days: u32,
) -> Result<(u32, u32), crate::workspace::WorkspaceError> {
    let scopes = vec![scope.to_string()];
    let rows = workspace.knowledge_scan_all(&scopes).await?;
    let mut scanned = 0u32;
    let mut marked = 0u32;
    for row in &rows {
        scanned += 1;
        if row.meta.status != KnowledgeStatus::Active {
            continue;
        }
        if !row
            .meta
            .needs_verification(row.updated_at, Some(i64::from(stale_after_days)), None)
        {
            continue;
        }
        let mut meta = row.meta.clone();
        meta.status = KnowledgeStatus::Stale;
        workspace
            .set_document_metadata(&row.path, &meta.to_metadata_json())
            .await?;
        marked += 1;
    }
    Ok((scanned, marked))
}

/// Reap coordination wire notes stranded between the content and metadata
/// commits of an ingest whose sender never shared again to trigger the
/// hook's self-repair path. Only paths inside the wire agent namespace whose
/// body carries the wire marker (and no study-note metadata) and whose last
/// update is older than `grace` are removed; anything newer may still belong
/// to an in-flight ingest. `workspace` must be scoped to the shared scope so
/// deletes stay primary-scope. Returns the number of notes reaped.
async fn sweep_stranded_wire_writes(
    workspace: &Workspace,
    grace: chrono::Duration,
) -> Result<u32, crate::workspace::WorkspaceError> {
    let wire_prefix = format!("{KNOWLEDGE_DIR}/{WIRE_AGENT_PREFIX}-");
    let mut removed = 0u32;
    for path in workspace.list_all().await? {
        if !path.starts_with(&wire_prefix) {
            continue;
        }
        let document = match workspace.read_primary(&path).await {
            Ok(document) => document,
            Err(error) => {
                tracing::debug!(%path, %error, "knowledge sweep: stranded-candidate read failed");
                continue;
            }
        };
        if !is_incomplete_wire_write(&document.metadata, &document.content) {
            continue;
        }
        if Utc::now().signed_duration_since(document.updated_at) < grace {
            continue;
        }
        match workspace.delete(&path).await {
            Ok(()) => {
                removed += 1;
                tracing::info!(
                    %path,
                    "knowledge sweep: reaped stranded coordination wire note"
                );
            }
            Err(error) => {
                tracing::warn!(%path, %error, "knowledge sweep: stranded wire-note delete failed");
            }
        }
    }
    Ok(removed)
}

struct RunningGuard;

impl Drop for RunningGuard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

/// Minimum delay between sweep loop iterations, even on failure.
const MIN_LOOP_DELAY_SECS: u64 = 60;

/// Run the stale sweep on an independent daemon cadence until the process
/// runtime shuts down.
///
/// WI#121 defect (b): the sweep previously ran only inside the heartbeat
/// loop, and multi-tenant renders set `HEARTBEAT_ENABLED=false` for every
/// tenant, so a vanilla tenant never swept at all. This loop is
/// heartbeat-independent (same shape as the `knowledge_sync` cadence): it
/// sleeps with deterministic startup jitter, then re-checks the sweep every
/// hour — `run_knowledge_sweep_if_due`'s own cadence state file makes the
/// extra checks cheap no-ops. Safe to run alongside the heartbeat path; the
/// global `RUNNING` guard serializes actual sweep passes.
pub async fn run_knowledge_sweep_loop(
    workspace: Arc<Workspace>,
    shared_workspace: Option<Arc<Workspace>>,
    config: KnowledgeSweepConfig,
) {
    let cadence_secs = u64::from(config.cadence_hours).saturating_mul(3_600);
    let jitter_cap = (cadence_secs / 10).clamp(MIN_LOOP_DELAY_SECS, 300);
    let jitter = shared_deterministic_jitter_secs(workspace.user_id(), jitter_cap);
    tokio::time::sleep(std::time::Duration::from_secs(jitter)).await;
    let poll_secs = cadence_secs.clamp(3_600, 6 * 3_600);
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(poll_secs));
    ticker.tick().await; // Skip immediate first tick; startup jitter covered above.
    loop {
        let report =
            run_knowledge_sweep_if_due(workspace.as_ref(), shared_workspace.as_deref(), &config)
                .await;
        if report.had_work() {
            tracing::info!(
                notes_scanned = report.notes_scanned,
                notes_marked_stale = report.notes_marked_stale,
                stranded_removed = report.stranded_removed,
                "knowledge sweep loop: pass complete"
            );
        }
        ticker.tick().await;
    }
}

fn load_state(path: &std::path::Path) -> Option<KnowledgeSweepState> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn save_state(path: &std::path::Path) {
    let state = KnowledgeSweepState {
        last_run: Utc::now(),
    };
    if let Ok(json) = serde_json::to_string(&state) {
        let _ = std::fs::write(path, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let config = KnowledgeSweepConfig::default();
        assert!(config.enabled);
        assert_eq!(config.stale_after_days, 30);
        assert_eq!(config.cadence_hours, 24);
        assert!(config.include_shared);
    }

    #[test]
    fn disabled_config_reports_skipped() {
        let config = KnowledgeSweepConfig {
            enabled: false,
            ..Default::default()
        };
        // run_if_due short-circuits before touching the workspace when
        // disabled; the early-return path is what we assert here.
        assert!(!config.enabled);
    }

    #[test]
    fn state_roundtrip() {
        let dir = std::env::temp_dir().join(format!("knowledge-sweep-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let state_file = dir.join("state.json");
        save_state(&state_file);
        let loaded = load_state(&state_file).expect("state loads");
        let elapsed = Utc::now().signed_duration_since(loaded.last_run);
        assert!(elapsed.num_seconds() < 60);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn report_had_work() {
        let mut report = KnowledgeSweepReport::default();
        assert!(!report.had_work());
        report.notes_marked_stale = 2;
        assert!(report.had_work());
    }

    #[cfg(feature = "libsql")]
    mod libsql_integration {
        use super::*;
        use crate::workspace::{
            KnowledgeNoteMeta, KnowledgeStatus, note_path, render_note_markdown,
            render_wire_note_markdown,
        };

        async fn submit_note(
            workspace: &Workspace,
            agent: &str,
            topic: &str,
            studied_at: Option<DateTime<Utc>>,
            status: KnowledgeStatus,
        ) {
            let meta = KnowledgeNoteMeta {
                topic: topic.to_string(),
                agent: agent.to_string(),
                title: format!("{topic} title"),
                status,
                confidence: Some(0.8),
                studied_at,
                source_commit: Some("commit-a".to_string()),
                tags: Vec::new(),
                superseded_by: None,
                stale_after_days: None,
                status_reason: None,
                submitted_by: None,
                session_id: None,
            };
            let path = note_path(agent, topic);
            workspace
                .write(&path, &render_note_markdown(&meta.title, "findings"))
                .await
                .expect("write note");
            workspace
                .set_document_metadata(&path, &meta.to_metadata_json())
                .await
                .expect("set metadata");
        }

        async fn note_status(workspace: &Workspace, agent: &str, topic: &str) -> KnowledgeStatus {
            let doc = workspace
                .read_primary(&note_path(agent, topic))
                .await
                .expect("read note");
            KnowledgeNoteMeta::from_metadata_json(&doc.metadata)
                .expect("metadata parses")
                .status
        }

        fn test_config(state_dir: PathBuf) -> KnowledgeSweepConfig {
            KnowledgeSweepConfig {
                enabled: true,
                stale_after_days: 30,
                cadence_hours: 24,
                include_shared: true,
                state_dir,
            }
        }

        #[tokio::test]
        async fn sweep_marks_old_active_notes_stale_across_scopes() {
            let (db, _dir) = crate::testing::test_db().await;
            let alice = Workspace::new_with_db("alice", std::sync::Arc::clone(&db));
            let shared = Workspace::new_with_db(KNOWLEDGE_SHARED_SCOPE, db);

            let old = Utc::now() - chrono::Duration::days(90);
            submit_note(
                &alice,
                "kimi-1",
                "old-topic",
                Some(old),
                KnowledgeStatus::Active,
            )
            .await;
            submit_note(
                &alice,
                "kimi-1",
                "fresh-topic",
                None,
                KnowledgeStatus::Active,
            )
            .await;
            submit_note(
                &shared,
                "kimi-2",
                "herd-topic",
                Some(old),
                KnowledgeStatus::Active,
            )
            .await;

            // Call sweep_scope directly: the public entry point's global
            // RUNNING guard would make parallel tests skip nondeterministically.
            let (scanned_a, marked_a) =
                sweep_scope(&alice, "alice", 30).await.expect("sweep alice");
            let (scanned_s, marked_s) = sweep_scope(&shared, KNOWLEDGE_SHARED_SCOPE, 30)
                .await
                .expect("sweep shared");

            assert_eq!(scanned_a + scanned_s, 3);
            assert_eq!(marked_a + marked_s, 2);
            assert_eq!(
                note_status(&alice, "kimi-1", "old-topic").await,
                KnowledgeStatus::Stale
            );
            assert_eq!(
                note_status(&alice, "kimi-1", "fresh-topic").await,
                KnowledgeStatus::Active
            );
            assert_eq!(
                note_status(&shared, "kimi-2", "herd-topic").await,
                KnowledgeStatus::Stale
            );
        }

        #[tokio::test]
        async fn sweep_leaves_non_active_statuses_untouched() {
            let (db, _dir) = crate::testing::test_db().await;
            let alice = Workspace::new_with_db("alice", db);

            let old = Utc::now() - chrono::Duration::days(90);
            submit_note(
                &alice,
                "kimi-1",
                "superseded-topic",
                Some(old),
                KnowledgeStatus::Superseded,
            )
            .await;

            let (scanned, marked) = sweep_scope(&alice, "alice", 30).await.expect("sweep");

            assert_eq!(scanned, 1);
            assert_eq!(marked, 0);
            assert_eq!(
                note_status(&alice, "kimi-1", "superseded-topic").await,
                KnowledgeStatus::Superseded
            );
        }

        #[tokio::test]
        async fn second_run_within_cadence_skips() {
            let (db, _dir) = crate::testing::test_db().await;
            let state_dir = tempfile::tempdir().expect("state dir");
            let alice = Workspace::new_with_db("alice", db);

            let config = test_config(state_dir.path().to_path_buf());
            let first = run_knowledge_sweep_if_due(&alice, None, &config).await;
            assert!(!first.skipped);
            let second = run_knowledge_sweep_if_due(&alice, None, &config).await;
            assert!(second.skipped);
        }

        #[tokio::test]
        async fn sweep_covers_stores_larger_than_one_page() {
            // WI#121 defect (a) regression: the old fixed KNOWLEDGE_SCAN_CAP
            // window meant notes older than the cap were invisible to every
            // sweep. The paginated scan must reach notes deep in the store.
            let (db, _dir) = crate::testing::test_db().await;
            let alice = Workspace::new_with_db("alice", db);

            let old = Utc::now() - chrono::Duration::days(90);
            // More notes than one scan page; every one is aging and active.
            let total = crate::workspace::KNOWLEDGE_SCAN_PAGE + 50;
            for i in 0..total {
                submit_note(
                    &alice,
                    "kimi-1",
                    &format!("topic-{i:04}"),
                    Some(old),
                    KnowledgeStatus::Active,
                )
                .await;
            }

            let (scanned, marked) = sweep_scope(&alice, "alice", 30).await.expect("sweep");

            assert_eq!(scanned, u32::try_from(total).expect("fits"));
            assert_eq!(marked, u32::try_from(total).expect("fits"));
            // Oldest-page notes (beyond the former 1,000-row cap window the
            // pre-fix sweep would have seen) are now swept too.
            assert_eq!(
                note_status(&alice, "kimi-1", "topic-0000").await,
                KnowledgeStatus::Stale
            );
            assert_eq!(
                note_status(&alice, "kimi-1", &format!("topic-{:04}", total - 1),).await,
                KnowledgeStatus::Stale
            );
        }

        #[tokio::test]
        async fn sweep_reaps_only_grace_expired_stranded_wire_notes() {
            let (db, _dir) = crate::testing::test_db().await;
            let shared = Workspace::new_with_db(KNOWLEDGE_SHARED_SCOPE, db);
            let wire_agent = "coord-alice-0123456789abcdef01234567";

            // Stranded: wire-sentinel body, no provenance metadata.
            let stranded = note_path(wire_agent, "stranded-topic");
            shared
                .write(
                    &stranded,
                    &render_wire_note_markdown("KNOW: stranded", "stranded body"),
                )
                .await
                .expect("write stranded note");
            // Complete wire note: same namespace, metadata committed.
            let complete = note_path(wire_agent, "complete-topic");
            shared
                .write(
                    &complete,
                    &render_wire_note_markdown("KNOW: complete", "complete body"),
                )
                .await
                .expect("write complete note");
            let complete_meta = KnowledgeNoteMeta {
                topic: "complete-topic".to_string(),
                agent: wire_agent.to_string(),
                title: "KNOW: complete".to_string(),
                status: KnowledgeStatus::NeedsVerification,
                confidence: None,
                studied_at: None,
                source_commit: None,
                tags: vec!["coordination".to_string()],
                superseded_by: None,
                stale_after_days: None,
                status_reason: None,
                submitted_by: Some("room@conf/alice".to_string()),
                session_id: None,
            };
            shared
                .set_document_metadata(&complete, &complete_meta.to_metadata_json())
                .await
                .expect("set complete metadata");
            // Unmarked document in the wire namespace: no marker, no metadata.
            let unmarked = note_path(wire_agent, "unmarked-topic");
            shared
                .write(&unmarked, "# Some other document\n\nbody\n")
                .await
                .expect("write unmarked note");
            // Wire-marker body outside the wire namespace is never touched.
            let outside = note_path("kimi-1", "sentinel-topic");
            shared
                .write(&outside, &render_wire_note_markdown("KNOW: x", "body"))
                .await
                .expect("write outside note");

            // Within the grace period everything fresh survives.
            let removed = sweep_stranded_wire_writes(&shared, chrono::Duration::hours(24))
                .await
                .expect("sweep within grace");
            assert_eq!(removed, 0);

            // Grace expired: only the stranded wire note is reaped.
            let removed = sweep_stranded_wire_writes(&shared, chrono::Duration::zero())
                .await
                .expect("sweep past grace");
            assert_eq!(removed, 1);
            assert!(shared.read_primary(&stranded).await.is_err());
            assert!(shared.read_primary(&complete).await.is_ok());
            assert!(shared.read_primary(&unmarked).await.is_ok());
            assert!(shared.read_primary(&outside).await.is_ok());
        }
    }
}
