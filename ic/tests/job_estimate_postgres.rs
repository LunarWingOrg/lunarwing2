#![cfg(all(feature = "postgres", feature = "integration"))]
//! PostgreSQL coverage for the WI#318 C1 local-job duration estimate.
//!
//! `db/libsql/mod.rs` proves the same round trip on libSQL. This is the
//! PostgreSQL half of that acceptance line: the estimate is written with the
//! job row and read back through both the job record and the agent-job list
//! projection that feeds `GET /api/jobs`.
//!
//! Set `DATABASE_URL` to an isolated test database with LunarWing migrations
//! available. Local runs skip without it; CI must provide it.

use std::sync::Arc;
use std::time::Duration;

use lunarwing::config::DatabaseConfig;
use lunarwing::context::JobContext;
use lunarwing::db::{Database, connect_from_config};

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

#[tokio::test]
async fn estimated_duration_round_trips_through_postgres() {
    let Some(db) = postgres_database().await else {
        assert!(
            std::env::var_os("CI").is_none(),
            "CI must configure DATABASE_URL for the PostgreSQL estimate test"
        );
        eprintln!("skipping: DATABASE_URL is not configured for PostgreSQL integration tests");
        return;
    };
    let user = format!("test-user-est-{}", uuid::Uuid::new_v4());

    let mut ctx = JobContext::with_user(&user, "Timed Job", "A timed job");
    ctx.estimated_duration = Some(Duration::from_secs(90));
    db.save_job(&ctx).await.expect("save job with estimate");

    let loaded = db
        .get_job(ctx.job_id)
        .await
        .expect("load job")
        .expect("saved job exists");
    assert_eq!(loaded.estimated_duration, Some(Duration::from_secs(90)));

    let listed = db
        .list_agent_jobs_for_user(&user)
        .await
        .expect("list agent jobs for the user");
    let record = listed
        .iter()
        .find(|record| record.id == ctx.job_id)
        .expect("saved job listed");
    assert_eq!(record.estimated_duration_secs, Some(90));
}
