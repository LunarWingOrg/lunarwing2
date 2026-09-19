//! Deterministic, project/user-scoped source selection for insight batches.

use crate::types::insight::{
    INSIGHT_BATCH_THREADS, InsightBatch, InsightCursor, MAX_INSIGHT_SOURCE_CHARS,
    MAX_INSIGHT_THREAD_CHARS,
};
use crate::types::project::ProjectId;
use crate::types::thread::{Thread, ThreadState, ThreadType};

pub(crate) fn eligible_source_thread(thread: &Thread) -> bool {
    thread.thread_type == ThreadType::Foreground
        && thread.state == ThreadState::Done
        && thread.completed_at.is_some()
        && thread.parent_id.is_none()
        && !thread.config.mission_child
        && !thread.config.mission_context.is_conversation_insights()
        && !thread.insight_inputs.is_empty()
        && thread.insight_inputs.iter().all(|source| {
            source.thread_id == thread.id
                && !source.truncated
                && !source.text.trim().is_empty()
                && source.text.chars().count() <= MAX_INSIGHT_SOURCE_CHARS
        })
        && thread
            .insight_inputs
            .iter()
            .map(|source| source.text.chars().count())
            .sum::<usize>()
            <= MAX_INSIGHT_THREAD_CHARS
}

pub(crate) fn select_batch(
    threads: &[Thread],
    project_id: ProjectId,
    user_id: &str,
    after: Option<&InsightCursor>,
) -> Option<InsightBatch> {
    let mut eligible: Vec<_> = threads
        .iter()
        .filter_map(|thread| {
            if thread.project_id != project_id
                || thread.user_id != user_id
                || !eligible_source_thread(thread)
            {
                return None;
            }
            let cursor = InsightCursor {
                completed_at: thread.completed_at?,
                thread_id: thread.id,
            };
            after
                .is_none_or(|after| cursor.is_after(after))
                .then_some((cursor, thread))
        })
        .collect();
    eligible.sort_by_key(|(cursor, _)| (cursor.completed_at, cursor.thread_id.0));
    if eligible.len() < INSIGHT_BATCH_THREADS {
        return None;
    }
    eligible.truncate(INSIGHT_BATCH_THREADS);
    let cursor = eligible.last()?.0.clone();
    let sources: Vec<_> = eligible
        .into_iter()
        .flat_map(|(_, thread)| thread.insight_inputs.iter().cloned())
        .collect();
    let mut ids: Vec<_> = sources
        .iter()
        .map(|source| source.message_id.to_string())
        .collect();
    ids.sort();
    let content_hash = format!("insights:v2:{}:{}", project_id.0, ids.join(","));
    Some(InsightBatch {
        project_id,
        user_id: user_id.into(),
        sources,
        cursor,
        content_hash,
    })
}
