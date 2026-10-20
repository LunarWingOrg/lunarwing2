#![cfg(all(feature = "postgres", feature = "integration"))]
//! WI#370: cron tick retention against real PostgreSQL FK semantics.
//!
//! `job_events.job_id` references `agent_jobs(id)` with NO `ON DELETE
//! CASCADE`, so retention must delete events before their row, and an event
//! for a missing job row must be refused by the FK itself — orphan or
//! cross-owner events cannot exist by schema, independent of supervisor
//! logic. This target runs in the `rust-test-postgres-integration` CI lane
//! (`DATABASE_URL` + pgvector service); the generic
//! `external_worker_integration` target is auto-discovered only by the
//! libSQL lane.

use std::sync::Arc;

use chrono::{Duration, Utc};
use uuid::Uuid;

use lunarwing::config::DatabaseConfig;
use lunarwing::db::Database;
use lunarwing::db::postgres::PgBackend;
use lunarwing::history::SandboxJobRecord;
use lunarwing::orchestrator::cron_worker::cron_tick_title_prefix;

async fn test_backend() -> Arc<dyn Database> {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://localhost/lunarwing_test".to_string());
    let backend = PgBackend::new(&DatabaseConfig::from_postgres_url(&url, 4))
        .await
        .expect("failed to connect to PostgreSQL test service");
    backend
        .run_migrations()
        .await
        .expect("failed to run PostgreSQL test migrations");
    Arc::new(backend)
}

/// One terminal cron tick row whose title carries the supervisor-authored
/// prompt marker for `mission`, created `age_secs` in the past.
fn tick_row(id: Uuid, owner: &str, mission: &str, age_secs: i64) -> SandboxJobRecord {
    let created = Utc::now() - Duration::seconds(age_secs);
    SandboxJobRecord {
        id,
        task: format!(
            "{}, firing on cron schedule 'seeded'",
            cron_tick_title_prefix(mission)
        ),
        status: "completed".to_string(),
        user_id: owner.to_string(),
        project_dir: format!("missions/{mission}"),
        success: Some(true),
        failure_reason: None,
        created_at: created,
        started_at: Some(created),
        completed_at: Some(created),
        credential_grants_json: "[]".to_string(),
        job_mode: "external:mock".to_string(),
    }
}

/// Per-test owner id: the CI integration database is shared, so every test
/// cleans exactly its own rows and never observes another test's data.
fn fresh_owner(label: &str) -> String {
    format!("cron370-{label}-{}", Uuid::new_v4().simple())
}

async fn seed_tick_with_events(
    db: &Arc<dyn Database>,
    id: Uuid,
    owner: &str,
    mission: &str,
    age: i64,
) {
    db.save_sandbox_job(&tick_row(id, owner, mission, age))
        .await
        .expect("seed tick row");
    db.save_job_event(
        id,
        "cron_tick",
        &serde_json::json!({ "mission_id": mission }),
    )
    .await
    .expect("seed cron_tick event");
    db.save_job_event(
        id,
        "cron_tick_end",
        &serde_json::json!({ "mission_id": mission, "outcome": "completed" }),
    )
    .await
    .expect("seed cron_tick_end event");
}

async fn cleanup_owner(db: &Arc<dyn Database>, owner: &str) {
    for job in db
        .list_sandbox_jobs_for_user(owner)
        .await
        .unwrap_or_default()
    {
        let _ = db.delete_sandbox_job_cascade(job.id).await;
    }
}

/// The FK refuses an event for a job row that does not exist, and the
/// cascade delete removes a tick row together with its events so the FK
/// never dangles — the two invariants retention depends on (WI#370:
/// "deleted anchors must never produce orphan events").
#[tokio::test]
async fn retention_delete_removes_events_first_and_fk_refuses_orphans() {
    let db = test_backend().await;
    let owner = fresh_owner("fk");
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let c = Uuid::new_v4();
    for (id, age) in [(a, 300), (b, 200), (c, 100)] {
        seed_tick_with_events(&db, id, &owner, "m", age).await;
    }

    // Orphan events are impossible by schema: a lifecycle event keyed to a
    // job row that does not exist is refused by the FK, so a deleted (or
    // never-dispatched) anchor can never accumulate orphan events no matter
    // what the supervisor does.
    let orphan = db
        .save_job_event(Uuid::new_v4(), "cron_paused", &serde_json::json!({}))
        .await;
    assert!(
        orphan.is_err(),
        "job_events.job_id FK must refuse an event for a missing job row"
    );

    // Cascade delete removes the row AND its events in the FK-safe order.
    assert!(
        db.delete_sandbox_job_cascade(b)
            .await
            .expect("cascade delete"),
        "the row exists and must be deleted"
    );
    assert!(db.get_sandbox_job(b).await.expect("row read").is_none());
    assert!(
        db.list_job_events(b, None)
            .await
            .expect("events read")
            .is_empty(),
        "the deleted row must not leave orphan job_events"
    );
    for kept in [a, c] {
        assert!(db.get_sandbox_job(kept).await.expect("row read").is_some());
        assert_eq!(
            db.list_job_events(kept, None)
                .await
                .expect("events read")
                .len(),
            2,
            "kept rows keep their events"
        );
    }

    // Re-deleting a deleted row reports false (idempotent, no error).
    assert!(
        !db.delete_sandbox_job_cascade(b).await.expect("re-delete"),
        "deleting a missing row must report false"
    );

    cleanup_owner(&db, &owner).await;
}

/// The mission tick listing is exact: newest first, this owner only, this
/// mission only, this worker's job_mode only — ordinary jobs, other
/// missions' ticks, and other owners' rows are never returned, so the
/// retention sweep built on it cannot delete them (WI#370).
#[tokio::test]
async fn mission_tick_listing_is_scoped_and_newest_first() {
    let db = test_backend().await;
    let owner1 = fresh_owner("scope1");
    let owner2 = fresh_owner("scope2");
    let m_old = Uuid::new_v4();
    let m_new = Uuid::new_v4();
    let other_mission = Uuid::new_v4();
    let ordinary = Uuid::new_v4();
    seed_tick_with_events(&db, m_old, &owner1, "m", 300).await;
    seed_tick_with_events(&db, m_new, &owner1, "m", 100).await;
    seed_tick_with_events(&db, other_mission, &owner1, "m-other", 200).await;
    // Same mission + worker, but a different owner's tick.
    seed_tick_with_events(&db, Uuid::new_v4(), &owner2, "m", 150).await;
    // Same owner + worker, ordinary task-mode title.
    db.save_sandbox_job(&SandboxJobRecord {
        id: ordinary,
        task: "Ordinary task-mode job".to_string(),
        status: "completed".to_string(),
        user_id: owner1.clone(),
        project_dir: String::new(),
        success: Some(true),
        failure_reason: None,
        created_at: Utc::now(),
        started_at: None,
        completed_at: None,
        credential_grants_json: "[]".to_string(),
        job_mode: "external:mock".to_string(),
    })
    .await
    .expect("seed ordinary row");

    let ticks = db
        .list_cron_mission_ticks(&owner1, "external:mock", &cron_tick_title_prefix("m"))
        .await
        .expect("mission tick listing");
    let ids: Vec<Uuid> = ticks.iter().map(|t| t.id).collect();
    assert_eq!(
        ids,
        vec![m_new, m_old],
        "exactly this owner's mission-m ticks, newest first"
    );

    // A mission-id prefix of another mission never matches (exact segment).
    let ticks = db
        .list_cron_mission_ticks(&owner1, "external:mock", &cron_tick_title_prefix("m2"))
        .await
        .expect("mission tick listing");
    assert!(ticks.is_empty(), "mission 'm' must not match prefix 'm2'");

    cleanup_owner(&db, &owner1).await;
    cleanup_owner(&db, &owner2).await;
}
