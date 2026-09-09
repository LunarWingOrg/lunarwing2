//! Operator-triggered additive knowledge refresh.

use std::sync::Arc;

use crate::workspace::Workspace;

/// Run the same bounded additive pass used by the daemon cadence runner.
pub async fn run_knowledge_refresh_command(
    config_path: Option<&std::path::Path>,
    json: bool,
) -> anyhow::Result<()> {
    let config = crate::config::Config::from_env_with_toml(config_path)
        .await
        .map_err(|error| anyhow::anyhow!("{error:#}"))?;
    let sync = config
        .knowledge_sync
        .to_manual_workspace_config()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "LW2STUDYMCP_HOME is required; configure a published Study Notes checkout first"
            )
        })?;
    let session = crate::llm::create_session_manager(config.llm.session.clone()).await;
    let embeddings = config
        .embeddings
        .create_provider(&config.llm.lunarwing_cloud.base_url, session);
    let db: Arc<dyn crate::db::Database> = crate::db::connect_from_config(&config.database)
        .await
        .map_err(|error| anyhow::anyhow!("{error:#}"))?;
    let mut workspace = Workspace::new_with_db(&config.owner_id, db);
    if let Some(provider) = embeddings {
        workspace = workspace.with_embeddings_cached(
            provider,
            crate::workspace::EmbeddingCacheConfig {
                max_entries: config.embeddings.cache_size,
            },
        );
    }
    let report =
        crate::workspace::knowledge_sync::run_knowledge_sync_once(&workspace, &sync, true).await;

    if json {
        println!("{}", serde_json::to_string(&report)?);
    } else if report.skipped {
        println!(
            "Knowledge refresh skipped: {}",
            report.skip_reason.unwrap_or("unknown")
        );
    } else {
        println!(
            "Knowledge refresh (mode={} revision={}): added={}, existing={}, deferred={}, failed={}, bytes={}",
            report.mode,
            report.source_revision.as_deref().unwrap_or("unknown"),
            report.notes_added,
            report.notes_existing,
            report.notes_deferred,
            report.notes_failed,
            report.bytes_added,
        );
    }

    if !report.successful {
        anyhow::bail!(
            "knowledge refresh failed: {}",
            report.error_class.unwrap_or("unknown")
        );
    }
    Ok(())
}
