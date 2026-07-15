mod support;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::RwLock;
use uuid::Uuid;

use lunarwing::bridge::skill_migration::migrate_v1_skills;
use lunarwing::channels::{IncomingMessage, StatusUpdate};
use lunarwing_engine::{
    CapabilityLease, DocId, DocType, EngineError, LeaseId, MemoryDoc, Mission, MissionId,
    MissionStatus, Project, ProjectId, Step, Store, Thread, ThreadEvent, ThreadId, ThreadState,
};
use lunarwing_skills::v2::V2SkillMetadata;

use support::engine_v2_env::{ENGINE_V2_ENV_LOCK, EngineV2EnvGuard};
use support::test_rig::{TestRig, TestRigBuilder};
use support::trace_llm::{LlmTrace, TraceResponse, TraceStep};

const SKILL_NAME: &str = "engine-v2-selection-proof";
const USER_ID: &str = "default";
const ACTIVATION_TOKEN: &str = "lunarwing-engine-v2-skill-proof";
const CONTEXT_MARKER: &str = "SKILL_CONTEXT_PROOF";
const SKILL_BODY: &str = "Include SKILL_CONTEXT_PROOF in the model-visible guidance.\n";
const SKILL_FIXTURE: &str = r#"---
name: engine-v2-selection-proof
version: 1.0.0
description: Engine V2 skill-selection compatibility fixture
activation:
  keywords:
    - lunarwing-engine-v2-skill-proof
  max_context_tokens: 128
---

Include SKILL_CONTEXT_PROOF in the model-visible guidance.
"#;

#[derive(Debug, Deserialize)]
struct PersistedSkillFrontmatter {
    doc_type: String,
    title: String,
    metadata: V2SkillMetadata,
}

#[derive(Debug)]
struct PersistedSkillDoc {
    frontmatter: PersistedSkillFrontmatter,
    content: String,
}

fn terminal_trace(user_input: &str, response: &str) -> LlmTrace {
    LlmTrace::single_turn(
        "engine-v2-skill-selection-test",
        user_input,
        vec![TraceStep {
            request_hint: None,
            response: TraceResponse::Text {
                content: response.to_string(),
                input_tokens: 8,
                output_tokens: 4,
            },
            expected_tool_results: Vec::new(),
        }],
    )
}

fn gateway_message(thread_id: Uuid, content: &str) -> IncomingMessage {
    IncomingMessage::new("gateway", USER_ID, content)
        .with_thread(thread_id.to_string())
        .with_metadata(serde_json::json!({
            "thread_id": thread_id,
            "user_id": USER_ID,
        }))
}

async fn register_gateway_thread(rig: &TestRig, thread_id: Uuid) {
    let conversation = rig
        .database()
        .get_or_create_scoped_conversation("gateway", USER_ID, &thread_id.to_string())
        .await
        .expect("gateway conversation should be registered");
    assert_eq!(conversation, thread_id);
}

async fn build_skill_rig(user_input: &str, response: &str) -> TestRig {
    TestRigBuilder::new()
        .with_channel_name("gateway")
        .with_trace(terminal_trace(user_input, response))
        .with_skills()
        .with_seeded_skill("SKILL.md", SKILL_FIXTURE)
        .build()
        .await
}

async fn persisted_skill_docs(rig: &TestRig) -> Vec<PersistedSkillDoc> {
    let workspace = rig.workspace().expect("test rig should have a workspace");
    let paths = workspace
        .list_all()
        .await
        .expect("workspace paths should be readable");
    let mut docs = Vec::new();
    for path in paths {
        if !path.starts_with("engine/knowledge/skills/") || !path.ends_with(".md") {
            continue;
        }
        let document = workspace
            .read(&path)
            .await
            .unwrap_or_else(|error| panic!("read persisted skill {path}: {error}"));
        docs.push(parse_persisted_skill(&document.content));
    }
    docs
}

fn parse_persisted_skill(content: &str) -> PersistedSkillDoc {
    let without_open = content
        .strip_prefix("---\n")
        .expect("persisted skill should start with frontmatter");
    let (yaml, body) = without_open
        .split_once("\n---\n")
        .expect("persisted skill should close frontmatter");
    PersistedSkillDoc {
        frontmatter: serde_norway::from_str(yaml)
            .expect("persisted skill frontmatter should deserialize"),
        content: body.trim_start_matches('\n').to_string(),
    }
}

#[tokio::test]
async fn matching_skill_is_migrated_selected_and_injected() {
    let _lock = ENGINE_V2_ENV_LOCK.lock().await;
    let env = EngineV2EnvGuard::enable(Some("gateway"));
    lunarwing::bridge::reset_engine_state().await;

    let rig = build_skill_rig(ACTIVATION_TOKEN, "skill-match-ok").await;
    let thread_id = Uuid::new_v4();
    register_gateway_thread(&rig, thread_id).await;
    rig.send_incoming(gateway_message(thread_id, ACTIVATION_TOKEN))
        .await;

    let responses = rig.wait_for_responses(1, Duration::from_secs(15)).await;
    assert_eq!(responses.len(), 1, "one terminal response expected");
    assert_eq!(responses[0].content, "skill-match-ok");

    let matching_docs: Vec<PersistedSkillDoc> = persisted_skill_docs(&rig)
        .await
        .into_iter()
        .filter(|doc| doc.frontmatter.metadata.name == SKILL_NAME)
        .collect();
    assert_eq!(
        matching_docs.len(),
        1,
        "fixture should migrate exactly once; persisted={matching_docs:?}"
    );
    let doc = matching_docs
        .first()
        .expect("one matching persisted skill should exist");
    assert_eq!(doc.frontmatter.doc_type, "Skill");
    assert_eq!(doc.frontmatter.title, format!("skill:{SKILL_NAME}"));
    assert_eq!(doc.content, SKILL_BODY);
    assert_eq!(
        doc.frontmatter.metadata.activation.keywords,
        vec![ACTIVATION_TOKEN.to_string()]
    );
    assert_eq!(doc.frontmatter.metadata.activation.max_context_tokens, 128);
    assert_eq!(
        doc.frontmatter.metadata.content_hash,
        lunarwing_skills::compute_hash(SKILL_BODY)
    );

    let statuses = rig.captured_status_events();
    let requests = rig.captured_llm_requests();
    let activations: Vec<Vec<String>> = statuses
        .iter()
        .filter_map(|status| match status {
            StatusUpdate::SkillActivated { skill_names } => Some(skill_names.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        activations,
        vec![vec![SKILL_NAME.to_string()]],
        "statuses={statuses:?}"
    );

    assert!(
        requests.iter().flatten().any(|message| {
            message.content.contains("## Active Skills") && message.content.contains(CONTEXT_MARKER)
        }),
        "selected skill body should be visible to the model"
    );

    rig.shutdown_and_wait().await;
    env.cleanup().await;
}

#[tokio::test]
async fn non_matching_skill_is_not_selected_or_injected() {
    let _lock = ENGINE_V2_ENV_LOCK.lock().await;
    let env = EngineV2EnvGuard::enable(Some("gateway"));
    lunarwing::bridge::reset_engine_state().await;

    let user_input = "quartz-neutral-skill-miss-7391";
    let rig = build_skill_rig(user_input, "skill-miss-ok").await;
    let thread_id = Uuid::new_v4();
    register_gateway_thread(&rig, thread_id).await;
    rig.send_incoming(gateway_message(thread_id, user_input))
        .await;

    let responses = rig.wait_for_responses(1, Duration::from_secs(15)).await;
    assert_eq!(responses.len(), 1, "one terminal response expected");
    assert_eq!(responses[0].content, "skill-miss-ok");
    assert!(
        !rig.captured_status_events()
            .iter()
            .any(|status| matches!(status, StatusUpdate::SkillActivated { .. })),
        "non-matching input should not activate a skill"
    );
    assert!(
        !rig.captured_llm_requests()
            .iter()
            .flatten()
            .any(|message| message.content.contains(CONTEXT_MARKER)),
        "non-matching skill body should not be visible to the model"
    );

    rig.shutdown_and_wait().await;
    env.cleanup().await;
}

#[tokio::test]
async fn seeded_skill_filenames_reject_unsafe_paths() {
    for filename in ["../SKILL.md", "nested/SKILL.md", "/tmp/SKILL.md"] {
        let filename_owned = filename.to_string();
        let result = tokio::spawn(async move {
            TestRigBuilder::new()
                .with_seeded_skill(filename_owned, SKILL_FIXTURE)
                .build()
                .await
        })
        .await;
        let join_error = match result {
            Ok(rig) => {
                rig.shutdown_and_wait().await;
                panic!("unsafe seeded skill filename was accepted: {filename}");
            }
            Err(error) => error,
        };
        assert!(join_error.is_panic());
        let payload = join_error.into_panic();
        let panic_message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .expect("panic payload should be a string");
        assert_eq!(
            panic_message,
            format!("invalid seeded skill filename: {filename}")
        );
    }
}

#[derive(Default)]
struct MemoryOnlyStore {
    docs: RwLock<Vec<MemoryDoc>>,
}

fn unsupported<T>() -> Result<T, EngineError> {
    Err(EngineError::Store {
        reason: "operation is outside the migration test".to_string(),
    })
}

#[async_trait]
impl Store for MemoryOnlyStore {
    async fn save_thread(&self, _: &Thread) -> Result<(), EngineError> {
        unsupported()
    }

    async fn load_thread(&self, _: ThreadId) -> Result<Option<Thread>, EngineError> {
        unsupported()
    }

    async fn list_threads(&self, _: ProjectId, _: &str) -> Result<Vec<Thread>, EngineError> {
        unsupported()
    }

    async fn update_thread_state(&self, _: ThreadId, _: ThreadState) -> Result<(), EngineError> {
        unsupported()
    }

    async fn save_step(&self, _: &Step) -> Result<(), EngineError> {
        unsupported()
    }

    async fn load_steps(&self, _: ThreadId) -> Result<Vec<Step>, EngineError> {
        unsupported()
    }

    async fn append_events(&self, _: &[ThreadEvent]) -> Result<(), EngineError> {
        unsupported()
    }

    async fn load_events(&self, _: ThreadId) -> Result<Vec<ThreadEvent>, EngineError> {
        unsupported()
    }

    async fn save_project(&self, _: &Project) -> Result<(), EngineError> {
        unsupported()
    }

    async fn load_project(&self, _: ProjectId) -> Result<Option<Project>, EngineError> {
        unsupported()
    }

    async fn save_memory_doc(&self, doc: &MemoryDoc) -> Result<(), EngineError> {
        let mut docs = self.docs.write().await;
        docs.retain(|existing| existing.id != doc.id);
        docs.push(doc.clone());
        Ok(())
    }

    async fn load_memory_doc(&self, id: DocId) -> Result<Option<MemoryDoc>, EngineError> {
        Ok(self
            .docs
            .read()
            .await
            .iter()
            .find(|doc| doc.id == id)
            .cloned())
    }

    async fn list_memory_docs(
        &self,
        project_id: ProjectId,
        user_id: &str,
    ) -> Result<Vec<MemoryDoc>, EngineError> {
        Ok(self
            .docs
            .read()
            .await
            .iter()
            .filter(|doc| doc.project_id == project_id && doc.user_id == user_id)
            .cloned()
            .collect())
    }

    async fn save_lease(&self, _: &CapabilityLease) -> Result<(), EngineError> {
        unsupported()
    }

    async fn load_active_leases(&self, _: ThreadId) -> Result<Vec<CapabilityLease>, EngineError> {
        unsupported()
    }

    async fn revoke_lease(&self, _: LeaseId, _: &str) -> Result<(), EngineError> {
        unsupported()
    }

    async fn save_mission(&self, _: &Mission) -> Result<(), EngineError> {
        unsupported()
    }

    async fn load_mission(&self, _: MissionId) -> Result<Option<Mission>, EngineError> {
        unsupported()
    }

    async fn list_missions(&self, _: ProjectId, _: &str) -> Result<Vec<Mission>, EngineError> {
        unsupported()
    }

    async fn update_mission_status(
        &self,
        _: MissionId,
        _: MissionStatus,
    ) -> Result<(), EngineError> {
        unsupported()
    }
}

#[tokio::test]
async fn skill_migration_is_idempotent_for_unchanged_content() {
    let skills_dir = tempfile::tempdir().expect("skills temp dir should be created");
    std::fs::write(skills_dir.path().join("SKILL.md"), SKILL_FIXTURE)
        .expect("skill fixture should be written");
    let mut registry = lunarwing_skills::SkillRegistry::new(skills_dir.path().to_path_buf());
    let discovered = registry.discover_all().await;
    assert_eq!(discovered, vec![SKILL_NAME.to_string()]);

    let project_id = ProjectId::new();
    let store: Arc<dyn Store> = Arc::new(MemoryOnlyStore::default());
    let first_count = migrate_v1_skills(&registry, &store, project_id)
        .await
        .expect("first migration should succeed");
    let before = store
        .list_shared_memory_docs(project_id)
        .await
        .expect("migrated docs should be listable");
    let second_count = migrate_v1_skills(&registry, &store, project_id)
        .await
        .expect("second migration should succeed");
    let after = store
        .list_shared_memory_docs(project_id)
        .await
        .expect("migrated docs should remain listable");

    assert_eq!(first_count, 1);
    assert_eq!(second_count, 0);
    assert_eq!(before.len(), 1);
    assert_eq!(after.len(), before.len());
    assert_eq!(after[0].doc_type, DocType::Skill);
    assert_eq!(after[0].content, SKILL_BODY);
}
