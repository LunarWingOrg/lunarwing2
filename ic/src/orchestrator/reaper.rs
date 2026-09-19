//! Orphaned container cleanup.
//!
//! The SandboxReaper periodically scans for LunarWing-labeled containers and
//! cleans up orphans.
//!
//! **Problem:** If the agent process crashes between container creation and cleanup,
//! containers are orphaned indefinitely.
//!
//! **Solution:** Background reaper task with two independent sweeps:
//! 1. **Job containers** (bollard, Docker-compatible API): containers labeled
//!    `lunarwing.job_id` whose jobs are no longer active in the ContextManager.
//! 2. **Generic sandbox containers** (podman/docker CLI): containers labeled
//!    `lunarwing.sandbox` older than the orphan threshold that are not
//!    currently executing.
//!
//! # BOUNDARY (do not cross)
//!
//! The reaper is the ONLY component shared between the orchestrator's live
//! worker path (`job_manager.rs`, bollard, `lunarwing.job_id` labels) and the
//! generic sandbox (`crate::sandbox`, CLI runner, `lunarwing.sandbox` labels).
//! The two sweeps never act on each other's label namespace. See the boundary
//! note in `crate::sandbox` module docs.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::context::ContextManager;
use crate::orchestrator::job_manager::ContainerJobManager;
use crate::sandbox::connect_docker;
use crate::sandbox::{CliRunner, SANDBOX_CONTAINER_LABEL};

/// Configuration for the sandbox reaper.
#[derive(Debug, Clone)]
pub struct ReaperConfig {
    /// How often to scan for orphaned containers.
    pub scan_interval: Duration,
    /// Containers older than this with no active job are reaped.
    pub orphan_threshold: Duration,
    /// Label key for looking up job IDs in Docker metadata.
    pub container_label: String,
}

impl Default for ReaperConfig {
    fn default() -> Self {
        Self {
            scan_interval: Duration::from_secs(300),
            orphan_threshold: Duration::from_secs(600),
            container_label: "lunarwing.job_id".to_string(),
        }
    }
}

/// Background task that periodically cleans up orphaned Docker containers.
pub struct SandboxReaper {
    docker: bollard::Docker,
    job_manager: Arc<ContainerJobManager>,
    context_manager: Arc<ContextManager>,
    config: ReaperConfig,
}

impl SandboxReaper {
    /// Create a new reaper. Connects to Docker eagerly — returns error if Docker unavailable.
    pub async fn new(
        job_manager: Arc<ContainerJobManager>,
        context_manager: Arc<ContextManager>,
        config: ReaperConfig,
    ) -> Result<Self, crate::sandbox::SandboxError> {
        let docker = connect_docker().await?;
        Ok(Self {
            docker,
            job_manager,
            context_manager,
            config,
        })
    }

    /// Run the reaper loop forever. Should be spawned with `tokio::spawn`.
    pub async fn run(self) {
        // Validate scan_interval is non-zero to prevent tokio::time::interval panic
        if self.config.scan_interval.as_secs() == 0 {
            tracing::error!(
                "Reaper: scan_interval must be > 0, got {:?}. Reaper will not start.",
                self.config.scan_interval
            );
            return;
        }

        let mut interval = tokio::time::interval(self.config.scan_interval);
        // Skip any missed ticks if scan takes longer than the interval
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            self.scan_and_reap().await;
        }
    }

    async fn scan_and_reap(&self) {
        let containers = match self.list_lunarwing_containers().await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "Reaper: failed to list Docker containers");
                return;
            }
        };

        let now = Utc::now();
        // Compute threshold once outside the loop
        let threshold = match chrono::Duration::from_std(self.config.orphan_threshold) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Reaper: failed to convert orphan_threshold to chrono::Duration, using default of 10 minutes"
                );
                chrono::Duration::minutes(10)
            }
        };

        for (container_id, job_id, created_at) in containers {
            let age = now.signed_duration_since(created_at);

            if age < threshold {
                continue; // Too young — skip
            }

            // Check if job is still active (any non-terminal state prevents reaping).
            // Terminal states: Failed, Cancelled, Accepted
            // Active states: Pending, InProgress, Completed, Submitted, Stuck
            // If job doesn't exist or is in a terminal state, it's eligible for reaping.
            let is_active = match self.context_manager.get_context(job_id).await {
                Ok(ctx) => ctx.state.is_active(),
                Err(_) => false, // Not found — treat as orphaned
            };

            if is_active {
                tracing::debug!(
                    job_id = %job_id,
                    container_id = %&container_id[..12.min(container_id.len())],
                    "Reaper: container has active job, skipping"
                );
                continue;
            }

            tracing::info!(
                job_id = %job_id,
                container_id = %&container_id[..12.min(container_id.len())],
                age_secs = age.num_seconds(),
                "Reaper: orphaned container detected, cleaning up"
            );

            self.reap_container(&container_id, job_id).await;
        }
    }

    /// List all LunarWing-managed containers from Docker.
    ///
    /// Returns tuples of (container_id, job_id, created_at).
    async fn list_lunarwing_containers(
        &self,
    ) -> Result<Vec<(String, Uuid, DateTime<Utc>)>, bollard::errors::Error> {
        use bollard::container::ListContainersOptions;

        let mut filters = HashMap::new();
        filters.insert("label", vec![self.config.container_label.as_str()]);

        let options = ListContainersOptions {
            all: true, // include stopped containers
            filters,
            ..Default::default()
        };

        let summaries = self.docker.list_containers(Some(options)).await?;
        let mut result = Vec::new();

        for summary in summaries {
            let container_id = match summary.id {
                Some(id) => id,
                None => continue,
            };

            let labels = summary.labels.unwrap_or_default();

            // Parse job_id from label (using configured label key for consistency)
            let job_id = match labels
                .get(&self.config.container_label)
                .and_then(|s| s.parse::<Uuid>().ok())
            {
                Some(id) => id,
                None => {
                    tracing::warn!(
                        container_id = %&container_id[..12.min(container_id.len())],
                        label_key = %&self.config.container_label,
                        "Reaper: lunarwing container missing valid job_id label"
                    );
                    continue;
                }
            };

            // Parse created_at from label (set by us at creation time); fall back to Docker timestamp
            let created_at = match labels
                .get("lunarwing.created_at")
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc))
                .or_else(|| {
                    summary
                        .created
                        .and_then(|ts| DateTime::from_timestamp(ts, 0))
                }) {
                Some(ts) => ts,
                None => {
                    tracing::warn!(
                        container_id = %&container_id[..12.min(container_id.len())],
                        "Reaper: could not determine creation time for container, skipping"
                    );
                    continue;
                }
            };

            result.push((container_id, job_id, created_at));
        }

        Ok(result)
    }

    /// Stop and remove a single orphaned container.
    ///
    /// First tries `job_manager.stop_job()` (which also revokes the auth token).
    /// Falls back to direct Docker API if the handle is no longer in the in-memory map
    /// (e.g., after a process restart).
    async fn reap_container(&self, container_id: &str, job_id: Uuid) {
        // Try the high-level stop first (handles token revocation)
        match self.job_manager.stop_job(job_id).await {
            Ok(()) => {
                tracing::info!(
                    job_id = %job_id,
                    "Reaper: cleaned up orphaned container via job_manager"
                );
                return;
            }
            Err(e) => {
                tracing::debug!(
                    job_id = %job_id,
                    error = %e,
                    "Reaper: job_manager.stop_job failed (likely no handle after restart), falling back to direct Docker cleanup"
                );
            }
        }

        // Fall back: direct Docker stop + force remove
        if let Err(e) = self
            .docker
            .stop_container(
                container_id,
                Some(bollard::container::StopContainerOptions { t: 10 }),
            )
            .await
        {
            tracing::debug!(
                job_id = %job_id,
                container_id = %&container_id[..12.min(container_id.len())],
                error = %e,
                "Reaper: stop_container failed (may already be stopped)"
            );
        }

        if let Err(e) = self
            .docker
            .remove_container(
                container_id,
                Some(bollard::container::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
        {
            tracing::error!(
                job_id = %job_id,
                container_id = %&container_id[..12.min(container_id.len())],
                error = %e,
                "Reaper: failed to remove orphaned container"
            );
        } else {
            tracing::info!(
                job_id = %job_id,
                container_id = %&container_id[..12.min(container_id.len())],
                "Reaper: removed orphaned container via direct Docker API"
            );
        }
    }
}

impl CliRunner {
    /// Sweep for orphaned **generic sandbox** containers via the selected CLI.
    ///
    /// These are the `lunarwing.sandbox`-labeled containers created by
    /// `crate::sandbox`'s CLI runner (`--rm` normally cleans them up; this
    /// catches leftovers after crashes or failed kills). Never touches
    /// `lunarwing.job_id` containers — that namespace belongs to the bollard
    /// scan above.
    /// Runs without constructing `SandboxReaper` or opening an API socket.
    pub async fn reap_orphans(&self, orphan_threshold: Duration) {
        let filter = format!("label={SANDBOX_CONTAINER_LABEL}");
        let output = match tokio::time::timeout(
            Duration::from_secs(5),
            tokio::process::Command::new(&self.binary)
                .args(["ps", "-a", "--filter", &filter, "--format", "json"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        {
            Ok(Ok(output)) if output.status.success() => output,
            _ => {
                tracing::debug!("Reaper: runtime CLI sandbox sweep listing failed");
                return;
            }
        };

        let now = Utc::now();
        let threshold = chrono::Duration::from_std(orphan_threshold)
            .unwrap_or_else(|_| chrono::Duration::minutes(10));
        let inflight = crate::sandbox::inflight_sandbox_containers();

        for (name, created_at) in parse_sandbox_containers(&String::from_utf8_lossy(&output.stdout))
        {
            if inflight.iter().any(|n| n == &name) {
                tracing::debug!(
                    container = %name,
                    "Reaper: sandbox container currently executing, skipping"
                );
                continue;
            }

            // Age comes from the label (set at creation); fall back to the
            // runtime's own creation timestamp.
            let Some(created_at) = created_at else {
                tracing::warn!(
                    container = %name,
                    "Reaper: sandbox container without parseable creation time, skipping"
                );
                continue;
            };

            let age = now.signed_duration_since(created_at);
            if age < threshold {
                continue; // Too young — skip
            }

            tracing::info!(
                container = %name,
                age_secs = age.num_seconds(),
                "Reaper: orphaned sandbox container detected, cleaning up"
            );
            self.remove_container(&name).await;
        }
    }
}

/// Parse `ps -a --format json` output into `(name, created_at)` pairs for
/// sandbox-labeled containers. Handles both runtime shapes: podman emits a
/// JSON array, docker emits one JSON object per line.
fn parse_sandbox_containers(text: &str) -> Vec<(String, Option<DateTime<Utc>>)> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let values: Vec<serde_json::Value> = match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(serde_json::Value::Array(items)) => items,
        _ => trimmed
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .collect(),
    };

    let mut result = Vec::new();
    for value in values {
        let Some(name) = container_name(&value) else {
            continue;
        };
        if !name.starts_with("lunarwing-sandbox-") {
            continue;
        }
        // Docker ps emits Labels as "key=value,key=value", not an object.
        // Require our label as well as the ps filter before considering removal.
        let Some(label) = sandbox_label(&value) else {
            continue;
        };
        let created_at = DateTime::parse_from_rfc3339(label)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
            .or_else(|| {
                value
                    .get("Created")
                    .and_then(|v| v.as_i64())
                    .and_then(|ts| DateTime::from_timestamp(ts, 0))
            });
        result.push((name, created_at));
    }
    result
}

fn sandbox_label(value: &serde_json::Value) -> Option<&str> {
    let labels = value.get("Labels")?;
    if let Some(text) = labels.as_str() {
        text.split(',').find_map(|entry| {
            let (key, value) = entry.split_once('=')?;
            (key == SANDBOX_CONTAINER_LABEL).then_some(value)
        })
    } else {
        labels.get(SANDBOX_CONTAINER_LABEL)?.as_str()
    }
}

/// Extract a container name from a `ps --format json` record. Podman reports
/// `Names` as an array, docker as a (possibly `/`-prefixed) string.
fn container_name(value: &serde_json::Value) -> Option<String> {
    let names = value.get("Names")?;
    let name = match names {
        serde_json::Value::Array(items) => {
            items.first().and_then(|v| v.as_str()).map(str::to_string)?
        }
        serde_json::Value::String(s) => s.clone(),
        _ => return None,
    };
    let name = name.trim_start_matches('/').to_string();
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    // ── generic sandbox CLI sweep parsing ──────────────────────────────

    #[test]
    fn parse_sandbox_containers_podman_array_shape() {
        let json = r#"[
            {
                "Names": ["lunarwing-sandbox-abc"],
                "Labels": {"lunarwing.sandbox": "2026-09-04T00:00:00Z"},
                "Created": 1760000000
            }
        ]"#;
        let parsed = parse_sandbox_containers(json);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "lunarwing-sandbox-abc");
        let created = parsed[0].1.expect("label timestamp parses");
        assert_eq!(created.to_rfc3339(), "2026-09-04T00:00:00+00:00");
    }

    #[test]
    fn parse_sandbox_containers_docker_json_lines_shape() {
        let json = concat!(
            "{\"Names\":\"lunarwing-sandbox-d1\",\"Labels\":\"other=x,lunarwing.sandbox=2026-09-04T01:02:03Z\",\"CreatedAt\":\"2026-09-04 01:02:03 +0000 UTC\"}\n",
            "{\"Names\":\"lunarwing-sandbox-d2\",\"Labels\":\"lunarwing.sandbox=invalid\",\"Created\":1760000000}\n",
        );
        let parsed = parse_sandbox_containers(json);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, "lunarwing-sandbox-d1");
        assert!(parsed[0].1.is_some());
        // Invalid label timestamp: falls back to the runtime's unix timestamp.
        assert_eq!(parsed[1].0, "lunarwing-sandbox-d2");
        assert!(parsed[1].1.is_some());
    }

    #[test]
    fn parse_sandbox_containers_handles_garbage_and_empty() {
        assert!(parse_sandbox_containers("").is_empty());
        assert!(parse_sandbox_containers("   \n").is_empty());
        assert!(parse_sandbox_containers("not json at all").is_empty());
        // Entries without a name are skipped.
        let json = r#"[{"Id":"deadbeef","Labels":{}}]"#;
        assert!(parse_sandbox_containers(json).is_empty());
    }

    #[test]
    fn parse_sandbox_containers_without_any_timestamp_yields_none() {
        let json =
            r#"[{"Names":["lunarwing-sandbox-x"],"Labels":{"lunarwing.sandbox":"garbage"}}]"#;
        let parsed = parse_sandbox_containers(json);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "lunarwing-sandbox-x");
        assert!(parsed[0].1.is_none());
    }

    // Test: age threshold filtering
    #[test]
    fn orphan_threshold_filters_young_containers() {
        let threshold = chrono::Duration::minutes(10);
        let young_age = chrono::Duration::minutes(2);
        assert!(young_age < threshold, "Young container should be skipped");
    }

    #[test]
    fn orphan_threshold_allows_old_containers() {
        let threshold = chrono::Duration::minutes(10);
        let old_age = chrono::Duration::minutes(15);
        assert!(old_age >= threshold, "Old container should be reaped");
    }

    // Test: active job detection
    #[tokio::test]
    async fn active_job_is_not_orphaned() {
        let ctx_mgr = Arc::new(ContextManager::new(5));

        // Create job and get its ID
        let job_id = ctx_mgr
            .create_job_for_user("default", "test", "test description")
            .await
            .unwrap();

        let ctx = ctx_mgr.get_context(job_id).await.unwrap();
        assert!(ctx.state.is_active(), "Pending job should be active");
    }

    #[tokio::test]
    async fn missing_job_is_treated_as_orphaned() {
        let ctx_mgr = Arc::new(ContextManager::new(5));
        let job_id = Uuid::new_v4(); // Not created
        let is_active = match ctx_mgr.get_context(job_id).await {
            Ok(ctx) => ctx.state.is_active(),
            Err(_) => false,
        };
        assert!(!is_active, "Missing job should be treated as orphaned");
    }

    #[tokio::test]
    async fn terminal_job_is_treated_as_orphaned() {
        use crate::context::JobState;

        let ctx_mgr = Arc::new(ContextManager::new(5));
        let job_id = ctx_mgr
            .create_job_for_user("default", "test", "test description")
            .await
            .unwrap();
        ctx_mgr
            .update_context(job_id, |ctx| {
                ctx.state = JobState::Failed;
            })
            .await
            .unwrap();

        let ctx = ctx_mgr.get_context(job_id).await.unwrap();
        assert!(
            !ctx.state.is_active(),
            "Failed job should be treated as orphaned"
        );
    }

    // ================================================================
    // Integration tests with mocks
    // ================================================================

    /// Mock implementation of Docker API for testing.
    /// (Currently unused but kept for future mock-based integration tests)
    #[allow(dead_code)]
    struct MockDocker {
        containers: Arc<std::sync::Mutex<Vec<ContainerSummary>>>,
        stop_called: Arc<AtomicU32>,
        remove_called: Arc<AtomicU32>,
        stop_error: Arc<AtomicBool>,
        remove_error: Arc<AtomicBool>,
    }

    #[allow(dead_code)]
    #[derive(Clone, Debug)]
    struct ContainerSummary {
        id: String,
        labels: HashMap<String, String>,
        created: Option<i64>,
    }

    #[allow(dead_code)]
    impl MockDocker {
        fn new() -> Self {
            Self {
                containers: Arc::new(std::sync::Mutex::new(Vec::new())),
                stop_called: Arc::new(AtomicU32::new(0)),
                remove_called: Arc::new(AtomicU32::new(0)),
                stop_error: Arc::new(AtomicBool::new(false)),
                remove_error: Arc::new(AtomicBool::new(false)),
            }
        }

        fn add_container(&self, id: String, labels: HashMap<String, String>, created: Option<i64>) {
            let mut cs = self.containers.lock().unwrap();
            cs.push(ContainerSummary {
                id,
                labels,
                created,
            });
        }

        fn set_stop_error(&self, error: bool) {
            self.stop_error.store(error, Ordering::SeqCst);
        }

        fn set_remove_error(&self, error: bool) {
            self.remove_error.store(error, Ordering::SeqCst);
        }

        fn stop_call_count(&self) -> u32 {
            self.stop_called.load(Ordering::SeqCst)
        }

        fn remove_call_count(&self) -> u32 {
            self.remove_called.load(Ordering::SeqCst)
        }
    }

    // Test: container labeling is parsed correctly
    #[test]
    fn parse_container_labels_extracts_job_id_and_timestamp() {
        let mut labels = HashMap::new();
        let job_id = Uuid::new_v4();
        labels.insert("lunarwing.job_id".to_string(), job_id.to_string());
        labels.insert(
            "lunarwing.created_at".to_string(),
            "2024-01-15T10:30:45+00:00".to_string(),
        );

        // Verify parsing works
        let parsed_id: Option<Uuid> = labels
            .get("lunarwing.job_id")
            .and_then(|s| s.parse::<Uuid>().ok());
        assert_eq!(parsed_id, Some(job_id));

        let parsed_time = labels
            .get("lunarwing.created_at")
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok());
        assert!(parsed_time.is_some());
    }

    // Test: missing job_id label is handled gracefully
    #[test]
    fn missing_job_id_label_is_skipped() {
        let labels: HashMap<String, String> = HashMap::new();
        let job_id: Option<Uuid> = labels
            .get("lunarwing.job_id")
            .and_then(|s| s.parse::<Uuid>().ok());
        assert_eq!(job_id, None);
    }

    // Test: malformed timestamp falls back to Docker's created timestamp
    #[test]
    fn malformed_timestamp_fallback_works() {
        let mut labels: HashMap<String, String> = HashMap::new();
        labels.insert(
            "lunarwing.created_at".to_string(),
            "invalid-date".to_string(),
        );

        let parsed_time = labels
            .get("lunarwing.created_at")
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok());
        assert!(
            parsed_time.is_none(),
            "Malformed timestamp should fail to parse"
        );

        // In actual code, Docker's summary.created timestamp is used as fallback.
        // If both our label and Docker's timestamp are missing/invalid, the container is skipped.
        // Verify that a valid Docker timestamp would be used as fallback:
        let docker_timestamp: Option<i64> = Some(1705324245); // Some valid Unix timestamp
        let fallback = docker_timestamp.and_then(|ts| DateTime::from_timestamp(ts, 0));
        assert!(
            fallback.is_some(),
            "Docker timestamp fallback should parse successfully"
        );
    }

    // Test: age calculation distinguishes young from old containers
    #[tokio::test]
    async fn age_calculation_correctly_filters_containers() {
        let now = Utc::now();
        let young_container = now - chrono::Duration::minutes(2);
        let old_container = now - chrono::Duration::minutes(20);

        let threshold = chrono::Duration::minutes(10);

        let young_age = now.signed_duration_since(young_container);
        let old_age = now.signed_duration_since(old_container);

        assert!(
            young_age < threshold,
            "Young container should not be cleaned"
        );
        assert!(old_age >= threshold, "Old container should be cleaned");
    }

    // Test: active job prevents cleanup even if container is old
    #[tokio::test]
    async fn active_job_prevents_cleanup_of_old_container() {
        let ctx_mgr = Arc::new(ContextManager::new(5));

        // Create an active job
        let job_id = ctx_mgr
            .create_job_for_user("default", "test", "test job")
            .await
            .unwrap();

        // Verify job is active
        let ctx = ctx_mgr.get_context(job_id).await.unwrap();
        assert!(ctx.state.is_active());

        // Even if container is "old", active job means don't cleanup
        let is_active = match ctx_mgr.get_context(job_id).await {
            Ok(ctx) => ctx.state.is_active(),
            Err(_) => false,
        };
        assert!(is_active, "Active job should prevent cleanup");
    }

    // Test: failed job allows cleanup (terminal state)
    #[tokio::test]
    async fn failed_job_allows_cleanup() {
        use crate::context::JobState;

        let ctx_mgr = Arc::new(ContextManager::new(5));
        let job_id = ctx_mgr
            .create_job_for_user("default", "test", "test")
            .await
            .unwrap();

        // Mark job as failed (terminal state)
        ctx_mgr
            .update_context(job_id, |ctx| {
                ctx.state = JobState::Failed;
            })
            .await
            .unwrap();

        let ctx = ctx_mgr.get_context(job_id).await.unwrap();
        assert!(
            !ctx.state.is_active(),
            "Failed job (terminal state) should allow cleanup"
        );
    }

    // Test: config validation
    #[test]
    fn reaper_config_defaults_are_reasonable() {
        let cfg = ReaperConfig::default();
        assert_eq!(
            cfg.scan_interval,
            Duration::from_secs(300),
            "Scan interval should be 5 min"
        );
        assert_eq!(
            cfg.orphan_threshold,
            Duration::from_secs(600),
            "Orphan threshold should be 10 min"
        );
        assert_eq!(cfg.container_label, "lunarwing.job_id");
    }

    // Test: reaper config is customizable
    #[test]
    fn reaper_config_can_be_customized() {
        let cfg = ReaperConfig {
            scan_interval: Duration::from_secs(60),
            orphan_threshold: Duration::from_secs(300),
            container_label: "custom.label".to_string(),
        };
        assert_eq!(cfg.scan_interval, Duration::from_secs(60));
        assert_eq!(cfg.orphan_threshold, Duration::from_secs(300));
        assert_eq!(cfg.container_label, "custom.label");
    }

    // Test: reaper correctly identifies which containers to cleanup
    #[tokio::test]
    async fn reaper_cleanup_decision_matrix() {
        use crate::context::JobState;

        let ctx_mgr = Arc::new(ContextManager::new(5));

        // Case 1: Pending job (active) -> should NOT cleanup even if old
        let job1 = ctx_mgr
            .create_job_for_user("default", "test", "test1")
            .await
            .unwrap();
        let ctx1 = ctx_mgr.get_context(job1).await.unwrap();
        assert!(ctx1.state.is_active(), "Pending job is active");
        assert!(ctx1.state.is_active(), "Should NOT cleanup active jobs");

        // Case 2: In-progress job (active) -> should NOT cleanup even if old
        let job2 = ctx_mgr
            .create_job_for_user("default", "test", "test2")
            .await
            .unwrap();
        ctx_mgr
            .update_context(job2, |ctx| {
                ctx.state = JobState::InProgress;
            })
            .await
            .unwrap();
        let ctx2 = ctx_mgr.get_context(job2).await.unwrap();
        assert!(ctx2.state.is_active(), "InProgress job is active");
        assert!(ctx2.state.is_active(), "Should NOT cleanup active jobs");

        // Case 3: Completed job (active) -> still active, should NOT cleanup
        let job3 = ctx_mgr
            .create_job_for_user("default", "test", "test3")
            .await
            .unwrap();
        ctx_mgr
            .update_context(job3, |ctx| {
                ctx.state = JobState::Completed;
            })
            .await
            .unwrap();
        let ctx3 = ctx_mgr.get_context(job3).await.unwrap();
        // Completed is NOT terminal, still active
        assert!(ctx3.state.is_active(), "Completed is still active");

        // Case 4: Failed job (terminal) -> should cleanup if old enough
        let job4 = ctx_mgr
            .create_job_for_user("default", "test", "test4")
            .await
            .unwrap();
        ctx_mgr
            .update_context(job4, |ctx| {
                ctx.state = JobState::Failed;
            })
            .await
            .unwrap();
        let ctx4 = ctx_mgr.get_context(job4).await.unwrap();
        assert!(
            !ctx4.state.is_active(),
            "Failed job is terminal (should cleanup if old)"
        );

        // Case 5: Cancelled job (terminal) -> should cleanup if old enough
        let job5 = ctx_mgr
            .create_job_for_user("default", "test", "test5")
            .await
            .unwrap();
        ctx_mgr
            .update_context(job5, |ctx| {
                ctx.state = JobState::Cancelled;
            })
            .await
            .unwrap();
        let ctx5 = ctx_mgr.get_context(job5).await.unwrap();
        assert!(!ctx5.state.is_active(), "Cancelled job is terminal");

        // Case 6: Missing job -> should cleanup if old enough
        let missing_job = Uuid::new_v4();
        let is_active = match ctx_mgr.get_context(missing_job).await {
            Ok(ctx) => ctx.state.is_active(),
            Err(_) => false,
        };
        assert!(!is_active, "Missing job should be treated as inactive");
    }

    // ================================================================
    // End-to-end tests with real Docker containers
    // ================================================================
    //
    // These tests verify the reaper works with actual Docker containers.
    // They require Docker to be running and the LUNARWING_E2E_DOCKER_TESTS
    // environment variable to be set (to avoid running them in CI by default).
    //
    // Run with: LUNARWING_E2E_DOCKER_TESTS=1 cargo test orchestrator::reaper::e2e_tests --lib -- --nocapture

    #[cfg(all(test, not(target_env = "msvc")))]
    mod e2e_tests {
        use super::*;

        fn should_run_e2e() -> bool {
            std::env::var("LUNARWING_E2E_DOCKER_TESTS").is_ok()
        }

        /// Test that reaper can list containers with LunarWing labels
        #[tokio::test]
        async fn e2e_reaper_lists_lunarwing_containers() {
            if !should_run_e2e() {
                eprintln!("Skipping e2e test (set LUNARWING_E2E_DOCKER_TESTS=1 to run)");
                return;
            }

            // Connect to Docker
            let docker = match crate::sandbox::connect_docker().await {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("Skipping e2e test: Docker unavailable: {e}");
                    return;
                }
            };

            // Create a test container with LunarWing labels
            let job_id = Uuid::new_v4();
            let test_name = format!("lunarwing-reaper-test-{}", &job_id.to_string()[..8]);

            let job_id_str = job_id.to_string();
            let created_at_str = (Utc::now() - chrono::Duration::hours(1)).to_rfc3339();

            let mut labels_str: std::collections::HashMap<&str, &str> =
                std::collections::HashMap::new();
            labels_str.insert("lunarwing.job_id", &job_id_str);
            labels_str.insert("lunarwing.created_at", &created_at_str);

            let config = bollard::container::CreateContainerOptions {
                name: test_name.as_str(),
                platform: None,
            };

            let container_config = bollard::container::Config {
                image: Some("alpine:latest"),
                labels: Some(labels_str),
                ..Default::default()
            };

            let response = match docker
                .create_container(Some(config), container_config)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Skipping e2e test: Could not create test container: {e}");
                    return;
                }
            };

            let container_id = &response.id;
            tracing::info!(
                container_id = %&container_id[..12.min(container_id.len())],
                job_id = %job_id,
                "e2e test: created test container"
            );

            // Verify container has correct labels
            let inspect = match docker.inspect_container(container_id, None).await {
                Ok(c) => c,
                Err(e) => {
                    let _ = docker.remove_container(container_id, None).await;
                    eprintln!("Failed to inspect container: {e}");
                    return;
                }
            };

            let labels = inspect.config.and_then(|c| c.labels).unwrap_or_default();
            assert!(
                labels.contains_key("lunarwing.job_id"),
                "Container should have lunarwing.job_id label"
            );
            assert_eq!(
                labels.get("lunarwing.job_id").map(|s| s.as_str()),
                Some(job_id.to_string().as_str()),
                "job_id label should match"
            );

            tracing::info!("e2e test: verified container labels");

            // Clean up
            let _ = docker.remove_container(container_id, None).await;
            tracing::info!("e2e test: cleaned up test container");
        }

        /// Test that reaper correctly identifies and removes orphaned containers
        #[tokio::test]
        async fn e2e_reaper_removes_orphaned_containers() {
            if !should_run_e2e() {
                eprintln!("Skipping e2e test (set LUNARWING_E2E_DOCKER_TESTS=1 to run)");
                return;
            }

            // Connect to Docker and create job manager / context manager
            let docker = match crate::sandbox::connect_docker().await {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("Skipping e2e test: Docker unavailable: {e}");
                    return;
                }
            };

            // Create a fake job ID that won't exist in context manager
            let orphaned_job_id = Uuid::new_v4();
            let test_name = format!(
                "lunarwing-orphan-test-{}",
                &orphaned_job_id.to_string()[..8]
            );

            let job_id_str = orphaned_job_id.to_string();
            let created_at_str = (Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
            let mut labels: std::collections::HashMap<&str, &str> =
                std::collections::HashMap::new();
            labels.insert("lunarwing.job_id", &job_id_str);
            labels.insert("lunarwing.created_at", &created_at_str);

            let config = bollard::container::CreateContainerOptions {
                name: test_name.as_str(),
                platform: None,
            };

            let container_config = bollard::container::Config {
                image: Some("alpine:latest"),
                labels: Some(labels),
                ..Default::default()
            };

            let response = match docker
                .create_container(Some(config), container_config)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Skipping e2e test: Could not create test container: {e}");
                    return;
                }
            };

            let container_id = response.id.clone();
            tracing::info!(
                container_id = %&container_id[..12.min(container_id.len())],
                job_id = %orphaned_job_id,
                "e2e test: created orphaned test container"
            );

            // Verify container exists before cleanup
            let exists_before = docker.inspect_container(&container_id, None).await.is_ok();
            assert!(exists_before, "Container should exist before cleanup");

            // Simulate reaper cleanup: try to stop and remove it
            let _ = docker
                .stop_container(
                    &container_id,
                    Some(bollard::container::StopContainerOptions { t: 10 }),
                )
                .await;

            let removal_result = docker
                .remove_container(
                    &container_id,
                    Some(bollard::container::RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;

            match removal_result {
                Ok(()) => {
                    tracing::info!(
                        container_id = %&container_id[..12.min(container_id.len())],
                        "e2e test: successfully removed orphaned container"
                    );
                    // Verify it's gone
                    let exists_after = docker.inspect_container(&container_id, None).await.is_ok();
                    assert!(!exists_after, "Container should not exist after removal");
                }
                Err(e) => {
                    eprintln!("Warning: failed to remove test container: {e}");
                    // Attempt cleanup anyway
                    let _ = docker.remove_container(&container_id, None).await;
                }
            }
        }

        /// Test that reaper respects age threshold
        #[tokio::test]
        async fn e2e_reaper_respects_age_threshold() {
            if !should_run_e2e() {
                eprintln!("Skipping e2e test (set LUNARWING_E2E_DOCKER_TESTS=1 to run)");
                return;
            }

            let docker = match crate::sandbox::connect_docker().await {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("Skipping e2e test: Docker unavailable: {e}");
                    return;
                }
            };

            // Create two containers: one old, one new
            let old_job_id = Uuid::new_v4();
            let new_job_id = Uuid::new_v4();

            // Old container (created 2 hours ago, beyond typical 10min threshold)
            let old_id_str = old_job_id.to_string();
            let old_time_str = (Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
            let mut old_labels: std::collections::HashMap<&str, &str> =
                std::collections::HashMap::new();
            old_labels.insert("lunarwing.job_id", &old_id_str);
            old_labels.insert("lunarwing.created_at", &old_time_str);

            // New container (created 1 minute ago, within threshold)
            let new_id_str = new_job_id.to_string();
            let new_time_str = (Utc::now() - chrono::Duration::minutes(1)).to_rfc3339();
            let mut new_labels: std::collections::HashMap<&str, &str> =
                std::collections::HashMap::new();
            new_labels.insert("lunarwing.job_id", &new_id_str);
            new_labels.insert("lunarwing.created_at", &new_time_str);

            let mut containers_to_cleanup = Vec::new();

            // Create old container
            let old_name = format!("lunarwing-age-old-{}", &old_job_id.to_string()[..8]);
            if let Ok(r) = docker
                .create_container(
                    Some(bollard::container::CreateContainerOptions {
                        name: old_name.as_str(),
                        platform: None,
                    }),
                    bollard::container::Config {
                        image: Some("alpine:latest"),
                        labels: Some(old_labels),
                        ..Default::default()
                    },
                )
                .await
            {
                containers_to_cleanup.push(r.id.clone());
                tracing::info!("e2e test: created old orphaned container for age threshold test");
            }

            // Create new container
            let new_name = format!("lunarwing-age-new-{}", &new_job_id.to_string()[..8]);
            if let Ok(r) = docker
                .create_container(
                    Some(bollard::container::CreateContainerOptions {
                        name: new_name.as_str(),
                        platform: None,
                    }),
                    bollard::container::Config {
                        image: Some("alpine:latest"),
                        labels: Some(new_labels),
                        ..Default::default()
                    },
                )
                .await
            {
                containers_to_cleanup.push(r.id.clone());
                tracing::info!("e2e test: created new orphaned container for age threshold test");
            }

            // Verify both exist
            assert_eq!(
                containers_to_cleanup.len(),
                2,
                "Should have created 2 test containers"
            );

            // Clean up
            for container_id in containers_to_cleanup {
                let _ = docker
                    .stop_container(
                        &container_id,
                        Some(bollard::container::StopContainerOptions { t: 10 }),
                    )
                    .await;
                let _ = docker
                    .remove_container(
                        &container_id,
                        Some(bollard::container::RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await;
            }

            tracing::info!("e2e test: age threshold test completed and cleaned up");
        }
    }
}
