#![cfg(all(feature = "postgres", feature = "integration"))]
//! PostgreSQL regression coverage for goal-25 stress finding F1:
//! concurrent appends to the SAME document must not surface 500s.
//!
//! Before the fix, both appenders re-indexed the document concurrently and
//! their DELETE+INSERT chunk replacement raced on the `unique_chunk_per_doc`
//! index, failing the loser with "Document chunking failed: Insert failed".
//! The repository now serializes chunk replacement per document with a
//! transaction-scoped advisory lock.
//!
//! Set `DATABASE_URL` to an isolated test database with LunarWing migrations
//! available. The test skips when the variable is absent.

use std::sync::Arc;

use lunarwing::config::DatabaseConfig;
use lunarwing::db::{Database, connect_from_config};
use lunarwing::workspace::Workspace;
use uuid::Uuid;

async fn postgres_database() -> Option<Arc<dyn Database>> {
    let url = std::env::var("DATABASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())?;
    let config = DatabaseConfig::from_postgres_url(&url, 8);
    Some(
        connect_from_config(&config)
            .await
            .expect("connect to configured PostgreSQL test database"),
    )
}

#[tokio::test]
async fn concurrent_appends_to_same_document_all_land() {
    let Some(db) = postgres_database().await else {
        eprintln!("skipping: DATABASE_URL is not configured for PostgreSQL integration tests");
        return;
    };
    let unique = Uuid::new_v4().simple().to_string();
    let scope = format!("conc-append-test-{unique}");
    let workspace = Arc::new(Workspace::new_with_db(scope.clone(), db));
    let path = format!("stress/concurrent/{unique}.md");

    const APPENDERS: usize = 40;
    let mut handles = Vec::with_capacity(APPENDERS);
    for i in 0..APPENDERS {
        let workspace = Arc::clone(&workspace);
        let path = path.clone();
        handles.push(tokio::spawn(async move {
            workspace
                .append(&path, &format!("append-marker-{i:02}"))
                .await
        }));
    }

    let mut failures = Vec::new();
    for (i, handle) in handles.into_iter().enumerate() {
        // A losing append previously surfaced as an Err here (chunk index
        // conflict); every append must now succeed.
        if let Err(e) = handle.await.expect("append task panicked") {
            failures.push(format!("appender {i}: {e}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{}/{} concurrent appends failed: {:?}",
        failures.len(),
        APPENDERS,
        failures
    );

    // Content fidelity: every marker present exactly once, no lost appends.
    let doc = workspace
        .read_primary(&path)
        .await
        .expect("appended document readable");
    for i in 0..APPENDERS {
        let marker = format!("append-marker-{i:02}");
        let count = doc.content.matches(&marker).count();
        assert_eq!(count, 1, "marker {marker} appeared {count} times");
    }

    // Search index consistency: chunk replacement raced safely, so a fresh
    // FTS query for any marker must hit the document.
    let hits = workspace
        .search("append-marker", 10)
        .await
        .expect("search after concurrent appends");
    assert!(
        hits.iter().any(|r| r.document_path == path),
        "appended document missing from search results"
    );
}
