//! Terminal feedback for skills activated by Engine V2 threads.

use std::collections::HashSet;
use std::sync::Arc;

use tracing::warn;

use crate::memory::SkillTracker;
use crate::runtime::messaging::ThreadOutcome;
use crate::traits::store::Store;
use crate::types::memory::DocId;
use crate::types::project::ProjectId;
use crate::types::thread::Thread;

pub(crate) const ACTIVE_SKILL_DOC_IDS_METADATA_KEY: &str =
    "lunarwing.skill_feedback.active_doc_ids";

/// Maximum number of unique skill IDs processed per terminal pass to bound
/// amplification from a potentially compromised orchestrator.
pub(crate) const MAX_EMITTED_SKILL_IDS: usize = 64;

/// Build the set of skill DocIds visible to a given thread scope (user-owned
/// and shared-owner docs in the same project). Returns `None` on lookup failure
/// so callers can fail closed.
pub(crate) async fn visible_skill_ids(
    store: &Arc<dyn Store>,
    project_id: ProjectId,
    user_id: &str,
) -> Option<HashSet<DocId>> {
    match store
        .list_memory_docs_with_shared(project_id, user_id)
        .await
    {
        Ok(docs) => Some(docs.into_iter().map(|doc| doc.id).collect()),
        Err(error) => {
            warn!(
                %error,
                "failing closed: scoped visibility lookup failed"
            );
            None
        }
    }
}

pub(crate) async fn record_terminal_skill_usage(
    thread: &mut Thread,
    outcome: &ThreadOutcome,
    store: &Arc<dyn Store>,
    enabled: bool,
) {
    if matches!(outcome, ThreadOutcome::GatePaused { .. }) {
        return;
    }

    let ids = thread
        .metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(ACTIVE_SKILL_DOC_IDS_METADATA_KEY));
    let Some(ids) = ids.and_then(|value| value.as_array().cloned()) else {
        return;
    };

    let success = match outcome {
        ThreadOutcome::Completed { .. } => Some(true),
        ThreadOutcome::Failed { .. } | ThreadOutcome::MaxIterations => Some(false),
        ThreadOutcome::Stopped | ThreadOutcome::GatePaused { .. } => None,
    };
    let Some(success) = success.filter(|_| enabled) else {
        return;
    };

    let Some(visible_ids) = visible_skill_ids(store, thread.project_id, &thread.user_id).await
    else {
        return;
    };

    let tracker = SkillTracker::new(Arc::clone(store));
    let mut seen = HashSet::new();
    for value in ids {
        let Some(raw_id) = value.as_str() else {
            warn!(thread_id = %thread.id, "ignoring non-string activated skill id");
            continue;
        };
        let Ok(uuid) = uuid::Uuid::parse_str(raw_id) else {
            warn!(thread_id = %thread.id, skill_id = %raw_id, "ignoring malformed activated skill id");
            continue;
        };
        let doc_id = DocId(uuid);
        if !seen.insert(doc_id) {
            continue;
        }
        if seen.len() > MAX_EMITTED_SKILL_IDS {
            break;
        }
        if !visible_ids.contains(&doc_id) {
            warn!(
                thread_id = %thread.id,
                skill_id = %raw_id,
                "ignoring activated skill id outside thread scope"
            );
            continue;
        }
        if let Err(error) = tracker.record_usage(doc_id, success).await {
            warn!(thread_id = %thread.id, skill_id = %raw_id, %error, "failed to record skill usage");
        }
    }
}
