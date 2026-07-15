use std::sync::Arc;

use lunarwing_skills::SkillTrust;
use lunarwing_skills::v2::{SkillMetrics, V2SkillMetadata, V2SkillSource};

use super::skill_feedback::{
    ACTIVE_SKILL_DOC_IDS_METADATA_KEY, MAX_EMITTED_SKILL_IDS, record_terminal_skill_usage,
};
use crate::runtime::messaging::ThreadOutcome;
use crate::traits::store::Store;
use crate::types::memory::{DocId, DocType, MemoryDoc};
use crate::types::project::ProjectId;
use crate::types::thread::{Thread, ThreadConfig, ThreadType};

fn skill_doc(project_id: ProjectId) -> MemoryDoc {
    skill_doc_for_user(project_id, "test-user")
}

fn skill_doc_for_user(project_id: ProjectId, user_id: &str) -> MemoryDoc {
    let metadata = V2SkillMetadata {
        name: "feedback-skill".into(),
        source: V2SkillSource::Extracted,
        trust: SkillTrust::Trusted,
        metrics: SkillMetrics::default(),
        ..serde_json::from_str::<V2SkillMetadata>("{}").unwrap()
    };
    let mut doc = MemoryDoc::new(
        project_id,
        user_id,
        DocType::Skill,
        "skill:feedback-skill",
        "feedback",
    );
    doc.metadata = serde_json::to_value(metadata).unwrap();
    doc
}

fn thread_with_ids(project_id: ProjectId, ids: &[DocId]) -> Thread {
    let mut thread = Thread::new(
        "feedback",
        ThreadType::Foreground,
        project_id,
        "test-user",
        ThreadConfig::default(),
    );
    thread.metadata.as_object_mut().unwrap().insert(
        ACTIVE_SKILL_DOC_IDS_METADATA_KEY.into(),
        serde_json::json!(ids.iter().map(|id| id.0.to_string()).collect::<Vec<_>>()),
    );
    thread
}

async fn metrics(store: &Arc<dyn Store>, doc_id: DocId) -> SkillMetrics {
    let doc = store.load_memory_doc(doc_id).await.unwrap().unwrap();
    serde_json::from_value::<V2SkillMetadata>(doc.metadata)
        .unwrap()
        .metrics
}

fn gate_paused() -> ThreadOutcome {
    ThreadOutcome::GatePaused {
        gate_name: "approval".into(),
        action_name: "test_tool".into(),
        call_id: "call-1".into(),
        parameters: serde_json::json!({}),
        resume_kind: crate::gate::ResumeKind::Approval {
            allow_always: false,
        },
        resume_output: None,
    }
}

async fn assert_feedback_case(
    outcome: ThreadOutcome,
    enabled: bool,
    duplicate_id: bool,
    expected: (u64, u64, u64),
    expect_ids: bool,
) {
    let project_id = ProjectId::new();
    let doc = skill_doc(project_id);
    let doc_id = doc.id;
    let store: Arc<dyn Store> = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
    let ids = if duplicate_id {
        vec![doc_id, doc_id]
    } else {
        vec![doc_id]
    };
    let mut thread = thread_with_ids(project_id, &ids);

    record_terminal_skill_usage(&mut thread, &outcome, &store, enabled).await;

    let metrics = metrics(&store, doc_id).await;
    assert_eq!(
        (
            metrics.usage_count,
            metrics.success_count,
            metrics.failure_count
        ),
        expected,
    );
    assert_eq!(
        thread
            .metadata
            .get(ACTIVE_SKILL_DOC_IDS_METADATA_KEY)
            .is_some(),
        expect_ids,
    );
}

#[tokio::test]
async fn completed_records_one_success_and_clears_ids() {
    assert_feedback_case(
        ThreadOutcome::Completed {
            response: Some("done".into()),
        },
        true,
        false,
        (1, 1, 0),
        false,
    )
    .await;
}

#[tokio::test]
async fn failed_records_one_failure_and_clears_ids() {
    assert_feedback_case(
        ThreadOutcome::Failed {
            error: "boom".into(),
        },
        true,
        false,
        (1, 0, 1),
        false,
    )
    .await;
}

#[tokio::test]
async fn max_iterations_records_one_failure_and_clears_ids() {
    assert_feedback_case(ThreadOutcome::MaxIterations, true, false, (1, 0, 1), false).await;
}

#[tokio::test]
async fn stopped_records_nothing_and_clears_ids() {
    assert_feedback_case(ThreadOutcome::Stopped, true, false, (0, 0, 0), false).await;
}

#[tokio::test]
async fn gate_paused_records_nothing_and_preserves_ids() {
    assert_feedback_case(gate_paused(), true, false, (0, 0, 0), true).await;
}

#[tokio::test]
async fn disabled_feedback_records_nothing_and_clears_terminal_ids() {
    assert_feedback_case(
        ThreadOutcome::Completed {
            response: Some("done".into()),
        },
        false,
        false,
        (0, 0, 0),
        false,
    )
    .await;
}

#[tokio::test]
async fn duplicate_ids_are_recorded_once() {
    assert_feedback_case(
        ThreadOutcome::Completed {
            response: Some("done".into()),
        },
        true,
        true,
        (1, 1, 0),
        false,
    )
    .await;
}

#[tokio::test]
async fn gate_pause_then_completion_records_exactly_once() {
    let project_id = ProjectId::new();
    let doc = skill_doc(project_id);
    let doc_id = doc.id;
    let store: Arc<dyn Store> = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
    let mut thread = thread_with_ids(project_id, &[doc_id]);

    record_terminal_skill_usage(&mut thread, &gate_paused(), &store, true).await;
    record_terminal_skill_usage(
        &mut thread,
        &ThreadOutcome::Completed {
            response: Some("done".into()),
        },
        &store,
        true,
    )
    .await;
    record_terminal_skill_usage(
        &mut thread,
        &ThreadOutcome::Completed {
            response: Some("done again".into()),
        },
        &store,
        true,
    )
    .await;

    let metrics = metrics(&store, doc_id).await;
    assert_eq!(
        (
            metrics.usage_count,
            metrics.success_count,
            metrics.failure_count
        ),
        (1, 1, 0),
    );
    assert!(
        thread
            .metadata
            .get(ACTIVE_SKILL_DOC_IDS_METADATA_KEY)
            .is_none()
    );
}

// ── Security: scoped visibility and amplification cap ─────────────────

#[tokio::test]
async fn wrong_project_skill_id_is_rejected() {
    let thread_project = ProjectId::new();
    let other_project = ProjectId::new();

    // Skill doc belongs to *other_project* — not visible to the thread.
    let doc = skill_doc(other_project);
    let doc_id = doc.id;
    let store: Arc<dyn Store> = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
    let mut thread = thread_with_ids(thread_project, &[doc_id]);

    record_terminal_skill_usage(
        &mut thread,
        &ThreadOutcome::Completed {
            response: Some("done".into()),
        },
        &store,
        true,
    )
    .await;

    // Metrics must be unchanged — the ID was silently rejected.
    let m = metrics(&store, doc_id).await;
    assert_eq!((m.usage_count, m.success_count, m.failure_count), (0, 0, 0));
    // IDs are still cleared on terminal outcome.
    assert!(
        thread
            .metadata
            .get(ACTIVE_SKILL_DOC_IDS_METADATA_KEY)
            .is_none()
    );
}

#[tokio::test]
async fn wrong_user_same_project_skill_id_is_rejected() {
    let project_id = ProjectId::new();

    // Skill owned by a *different user* in the same project — not shared owner.
    let doc = skill_doc_for_user(project_id, "attacker-user");
    let doc_id = doc.id;
    let store: Arc<dyn Store> = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
    let mut thread = thread_with_ids(project_id, &[doc_id]);

    record_terminal_skill_usage(
        &mut thread,
        &ThreadOutcome::Completed {
            response: Some("done".into()),
        },
        &store,
        true,
    )
    .await;

    let m = metrics(&store, doc_id).await;
    assert_eq!((m.usage_count, m.success_count, m.failure_count), (0, 0, 0));
}

#[tokio::test]
async fn shared_owner_skill_is_allowed() {
    let project_id = ProjectId::new();

    // Shared-owner skills must be visible to the thread's user.
    let doc = skill_doc_for_user(project_id, "__shared__");
    let doc_id = doc.id;
    let store: Arc<dyn Store> = Arc::new(crate::tests::InMemoryStore::with_docs(vec![doc]));
    let mut thread = thread_with_ids(project_id, &[doc_id]);

    record_terminal_skill_usage(
        &mut thread,
        &ThreadOutcome::Completed {
            response: Some("done".into()),
        },
        &store,
        true,
    )
    .await;

    let m = metrics(&store, doc_id).await;
    assert_eq!((m.usage_count, m.success_count, m.failure_count), (1, 1, 0));
}

#[tokio::test]
async fn processing_caps_at_max_emitted_skill_ids() {
    let project_id = ProjectId::new();
    // Create MAX + 5 docs, all valid skill docs visible to the thread.
    let mut docs: Vec<MemoryDoc> = Vec::new();
    let mut doc_ids: Vec<DocId> = Vec::new();
    for _ in 0..(MAX_EMITTED_SKILL_IDS + 5) {
        let doc = skill_doc(project_id);
        doc_ids.push(doc.id);
        docs.push(doc);
    }
    let store: Arc<dyn Store> = Arc::new(crate::tests::InMemoryStore::with_docs(docs));
    let mut thread = thread_with_ids(project_id, &doc_ids);

    record_terminal_skill_usage(
        &mut thread,
        &ThreadOutcome::Completed {
            response: Some("done".into()),
        },
        &store,
        true,
    )
    .await;

    // Only the first MAX unique IDs should have been recorded.
    let mut recorded = 0u64;
    for doc_id in &doc_ids {
        let m = metrics(&store, *doc_id).await;
        if m.usage_count > 0 {
            recorded += 1;
        }
    }
    assert_eq!(recorded, MAX_EMITTED_SKILL_IDS as u64);
}
