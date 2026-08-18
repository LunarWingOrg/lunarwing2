#![cfg(all(feature = "postgres", feature = "integration"))]
//! PostgreSQL integration coverage for coordination knowledge ingestion.
//!
//! Set `DATABASE_URL` to an isolated test database with LunarWing migrations
//! available. The test skips when the variable is absent.

use std::sync::Arc;
use std::time::Duration;

use lunarwing::config::DatabaseConfig;
use lunarwing::db::{Database, connect_from_config};
use lunarwing::hooks::coordination::CoordinationState;
use lunarwing::hooks::coordination_knowledge::{
    KnowledgeShareHook, KnowledgeShareLimits, sender_agent_slug,
};
use lunarwing::hooks::{Hook, HookEvent, HookOutcome};
use lunarwing::workspace::{
    KnowledgeFilter, KnowledgeNoteMeta, KnowledgeStatus, Workspace, note_path,
};
use uuid::Uuid;

const ROOM: &str = "coord@conference.example";
const SENDER: &str = "coord@conference.example/postgres-tester";

fn inbound(content: String) -> HookEvent {
    HookEvent::Inbound {
        user_id: "workspace-owner".to_string(),
        sender_id: Some(SENDER.to_string()),
        channel: "xmpp".to_string(),
        content,
        thread_id: None,
        policy_scope: Some(ROOM.to_string()),
    }
}

async fn postgres_database() -> Option<Arc<dyn Database>> {
    let url = std::env::var("DATABASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())?;
    let config = DatabaseConfig::from_postgres_url(&url, 2);
    Some(
        connect_from_config(&config)
            .await
            .expect("connect to configured PostgreSQL test database"),
    )
}

async fn await_note(workspace: &Workspace, path: &str) -> KnowledgeNoteMeta {
    for _ in 0..500 {
        if let Ok(document) = workspace.read_primary(path).await
            && let Some(meta) = KnowledgeNoteMeta::from_metadata_json(&document.metadata)
        {
            return meta;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("coordination knowledge note was not committed before timeout");
}

#[tokio::test]
async fn coordination_knowledge_round_trips_through_postgres() {
    let Some(db) = postgres_database().await else {
        eprintln!("skipping: DATABASE_URL is not configured for PostgreSQL integration tests");
        return;
    };
    let unique = Uuid::new_v4().to_string();
    let scope = format!("coord-knowledge-test-{unique}");
    let topic = format!("postgres-{unique}");
    let path = note_path(&sender_agent_slug(SENDER), &topic);
    let workspace = Arc::new(Workspace::new_with_db(scope.clone(), db));
    let state = CoordinationState::new(vec![ROOM.to_string()], Vec::new(), None);
    let hook = KnowledgeShareHook::new(
        state,
        Arc::clone(&workspace),
        KnowledgeShareLimits {
            publishers: vec![SENDER.to_string()],
            ..KnowledgeShareLimits::default()
        },
    );
    let finding = "PostgreSQL coordination sentinel verifies metadata and search round trips";

    let outcome = hook
        .execute(
            &inbound(format!("KNOW: {topic} | {finding}")),
            &Default::default(),
        )
        .await
        .expect("fail-open knowledge hook executes");
    assert!(matches!(outcome, HookOutcome::Continue { modified: None }));

    let meta = await_note(&workspace, &path).await;
    assert_eq!(meta.topic, topic);
    assert_eq!(meta.status, KnowledgeStatus::NeedsVerification);
    assert_eq!(meta.submitted_by.as_deref(), Some(SENDER));
    assert!(meta.tags.iter().any(|tag| tag == "coordination"));

    let scopes = vec![scope];
    let hits = workspace
        .knowledge_search(
            &scopes,
            "PostgreSQL coordination sentinel",
            &KnowledgeFilter::default(),
            10,
        )
        .await
        .expect("search PostgreSQL knowledge chunks");
    assert!(
        hits.iter().any(|hit| hit.path == path && hit.meta == meta),
        "ingested note must be searchable with its metadata"
    );

    let versions = workspace
        .knowledge_topic_versions(&scopes, &topic)
        .await
        .expect("read PostgreSQL topic versions");
    assert!(
        versions
            .iter()
            .any(|document| document.path == path && document.content.contains(finding)),
        "topic lookup must return the ingested note body"
    );

    workspace
        .delete(&path)
        .await
        .expect("clean up PostgreSQL coordination knowledge fixture");
}
