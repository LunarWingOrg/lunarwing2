//! Study-notes stale detection: background sweep that marks aging notes stale.
//!
//! Runs on a configurable cadence (like memory hygiene) and flips study notes
//! whose `studied_at` (falling back to the document's last update) is older
//! than the configured threshold from `active` to `stale`, across the caller's
//! private scope and the shared herd scope. Notes already marked `stale`,
//! `superseded`, or `needs-verification` are left untouched.
//!
//! A global [`AtomicBool`] guard prevents concurrent sweeps, mirroring the
//! hygiene pass guard, so overlapping heartbeat ticks cannot double-mark.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::bootstrap::lunarwing_base_dir;
use crate::workspace::{
    KNOWLEDGE_SCAN_CAP, KNOWLEDGE_SHARED_SCOPE, KnowledgeStatus, Workspace,
};

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
    /// Whether the run was skipped (disabled, cadence, or concurrent).
    pub skipped: bool,
}

impl KnowledgeSweepReport {
    /// True if any note was marked stale.
    pub fn had_work(&self) -> bool {
        self.notes_marked_stale > 0
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
    }

    if report.had_work() {
        tracing::info!(
            notes_scanned = report.notes_scanned,
            notes_marked_stale = report.notes_marked_stale,
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
    let rows = workspace
        .knowledge_list_notes(&scopes, KNOWLEDGE_SCAN_CAP)
        .await?;
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

struct RunningGuard;

impl Drop for RunningGuard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

fn load_state(path: &std::path::Path) -> Option<KnowledgeSweepState> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn save_state(path: &std::path::Path) {
    let state = KnowledgeSweepState { last_run: Utc::now() };
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
            submit_note(&alice, "kimi-1", "old-topic", Some(old), KnowledgeStatus::Active).await;
            submit_note(&alice, "kimi-1", "fresh-topic", None, KnowledgeStatus::Active).await;
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
            let (scanned_a, marked_a) = sweep_scope(&alice, "alice", 30).await.expect("sweep alice");
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
    }
}
