//! Evidence validation and deterministic upserts, called under a store write lock.

use crate::traits::store::Store;
use crate::types::error::EngineError;
use crate::types::insight::{
    INSIGHT_RECORD_VERSION, InsightKind, InsightRecord, InsightRequest, MAX_INSIGHTS_PER_RUN,
};
use crate::types::memory::{DocType, MemoryDoc};
use crate::types::mission::MissionStatus;
use crate::types::thread::{MissionContext, Thread};

fn invalid(reason: &str) -> EngineError {
    EngineError::Store {
        reason: format!("insight rejected: {reason}"),
    }
}

pub fn is_insight(doc: &MemoryDoc) -> bool {
    doc.insight.is_some()
        || doc.title.starts_with("insight:")
        || doc.tags.iter().any(|tag| tag == "insight")
}

/// A quote is evidence of what was said, never proof that its claims are true.
pub fn recallable(doc: &MemoryDoc) -> bool {
    if !is_insight(doc) {
        return true;
    }
    doc.insight.as_ref().is_some_and(|record| {
        record.version == INSIGHT_RECORD_VERSION
            && record.kind == InsightKind::Explicit
            && !record.sources.is_empty()
            && record
                .sources
                .iter()
                .all(|source| !source.truncated && source.text == doc.content)
    })
}

/// Must run under the adapter's insight/doc write lock, shared with mission
/// pause/delete persistence and purge. Adapters without this contract fail closed.
pub async fn prepare_upsert(
    store: &dyn Store,
    thread: &Thread,
    requested: &MemoryDoc,
) -> Result<MemoryDoc, EngineError> {
    let MissionContext::ConversationInsights {
        batch: Some(batch),
        mission_id: Some(mission_id),
        run_id: Some(run_id),
        generation,
        ..
    } = &thread.config.mission_context
    else {
        return Err(invalid("missing host-owned source batch"));
    };
    let mission = store
        .load_mission(*mission_id)
        .await?
        .ok_or_else(|| invalid("mission was removed"))?;
    if mission.status != MissionStatus::Active
        || mission.deleted_at.is_some()
        || mission.insight_generation != *generation
        || !mission.thread_history.contains(run_id)
        || mission.project_id != thread.project_id
        || mission.user_id != thread.user_id
        || batch.project_id != thread.project_id
        || batch.user_id != thread.user_id
    {
        return Err(invalid(
            "mission is inactive, revoked, or belongs to a different scope",
        ));
    }
    if requested.doc_type != DocType::Lesson
        || requested.user_id != thread.user_id
        || requested.project_id != thread.project_id
        || requested.owner_id().is_shared()
        || crate::memory::retrieval::is_engine_maintenance(requested)
    {
        return Err(invalid("only private insight lessons are allowed"));
    }
    let request: InsightRequest = serde_json::from_value(
        requested
            .metadata
            .get("insight")
            .cloned()
            .unwrap_or_default(),
    )
    .map_err(|_| invalid("metadata.insight requires kind and complete source citations"))?;
    if request.evidence.is_empty() || request.evidence.len() > batch.sources.len() {
        return Err(invalid("missing or excessive evidence"));
    }
    let mut sources = Vec::new();
    for citation in &request.evidence {
        let source = batch
            .sources
            .iter()
            .find(|source| {
                source.thread_id == citation.thread_id && source.message_id == citation.message_id
            })
            .ok_or_else(|| invalid("citation is outside this batch"))?;
        if source.truncated || source.text != citation.quote {
            return Err(invalid("quote must be the entire original user input"));
        }
        let original = store
            .load_thread(source.thread_id)
            .await?
            .ok_or_else(|| invalid("source thread unavailable"))?;
        if original.project_id != thread.project_id
            || original.user_id != thread.user_id
            || !crate::runtime::insights::eligible_source_thread(&original)
            || !original.insight_inputs.contains(source)
        {
            return Err(invalid("source is not an eligible persisted user input"));
        }
        if request.kind == InsightKind::Explicit && requested.content != source.text {
            return Err(invalid(
                "explicit content must equal the complete user statement",
            ));
        }
        if !sources.contains(source) {
            sources.push(source.clone());
        }
    }
    sources.sort_by_key(|source| (source.timestamp, source.message_id));
    let docs = store
        .list_memory_docs(thread.project_id, &thread.user_id)
        .await?;
    let existing = docs.iter().find(|doc| {
        doc.insight.as_ref().is_some_and(|record| {
            record.kind == request.kind
                && if request.kind == InsightKind::Explicit {
                    doc.content == requested.content
                } else {
                    // One hypothesis per evidence set: paraphrasing cannot mint duplicates.
                    record.sources == sources
                }
        })
    });
    let already_counted = existing
        .and_then(|doc| doc.insight.as_ref())
        .is_some_and(|record| record.runs.contains(run_id));
    let count = docs
        .iter()
        .filter(|doc| {
            doc.insight
                .as_ref()
                .is_some_and(|record| record.runs.contains(run_id))
        })
        .count();
    if !already_counted && count >= MAX_INSIGHTS_PER_RUN {
        return Err(invalid("five-insight run budget exhausted"));
    }
    let mut doc = existing.cloned().unwrap_or_else(|| {
        MemoryDoc::new(
            thread.project_id,
            &thread.user_id,
            DocType::Lesson,
            "",
            &requested.content,
        )
    });
    // Model titles, tags, and arbitrary metadata are not a second assertion channel.
    doc.title = format!("insight:{:?}:{}", request.kind, doc.id.0);
    doc.tags = vec!["insight".into()];
    doc.metadata = serde_json::json!({});
    let mut record = doc.insight.take().unwrap_or(InsightRecord {
        version: INSIGHT_RECORD_VERSION,
        kind: request.kind,
        sources: Vec::new(),
        generated_by: thread.id,
        runs: Vec::new(),
    });
    for source in sources {
        if !record.sources.contains(&source) {
            record.sources.push(source);
        }
    }
    if !record.runs.contains(run_id) {
        record.runs.push(*run_id);
    }
    doc.insight = Some(record);
    doc.source_thread_id = Some(thread.id);
    doc.updated_at = chrono::Utc::now();
    Ok(doc)
}
