//! Self-repair for stuck jobs and broken tools.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::context::{ContextManager, JobState};
use crate::db::Database;
use crate::error::RepairError;
use crate::tools::{BuildRequirement, Language, SoftwareBuilder, SoftwareType, ToolRegistry};

/// A job that has been detected as stuck.
#[derive(Debug, Clone)]
pub struct StuckJob {
    pub job_id: Uuid,
    pub last_activity: DateTime<Utc>,
    pub stuck_duration: Duration,
    pub last_error: Option<String>,
    pub repair_attempts: u32,
}

/// A tool that has been detected as broken.
#[derive(Debug, Clone)]
pub struct BrokenTool {
    pub name: String,
    pub failure_count: u32,
    pub last_error: Option<String>,
    pub first_failure: DateTime<Utc>,
    pub last_failure: DateTime<Utc>,
    pub last_build_result: Option<serde_json::Value>,
    pub repair_attempts: u32,
}

/// Result of a repair attempt.
#[derive(Debug)]
pub enum RepairResult {
    /// Repair was successful.
    Success { message: String },
    /// Repair failed but can be retried.
    Retry { message: String },
    /// Repair failed permanently.
    Failed { message: String },
    /// Manual intervention required.
    ManualRequired { message: String },
}

/// Trait for self-repair implementations.
#[async_trait]
pub trait SelfRepair: Send + Sync {
    /// Detect stuck jobs.
    async fn detect_stuck_jobs(&self) -> Vec<StuckJob>;

    /// Attempt to repair a stuck job.
    async fn repair_stuck_job(&self, job: &StuckJob) -> Result<RepairResult, RepairError>;

    /// Detect broken tools.
    async fn detect_broken_tools(&self) -> Vec<BrokenTool>;

    /// Attempt to repair a broken tool.
    async fn repair_broken_tool(&self, tool: &BrokenTool) -> Result<RepairResult, RepairError>;
}

/// Default self-repair implementation.
pub struct DefaultSelfRepair {
    context_manager: Arc<ContextManager>,
    /// Jobs in `InProgress` longer than this are treated as stuck.
    stuck_threshold: Duration,
    max_repair_attempts: u32,
    store: Option<Arc<dyn Database>>,
    builder: Option<Arc<dyn SoftwareBuilder>>,
    tools: Option<Arc<ToolRegistry>>,
    /// Tools that already surfaced a terminal `ManualRequired` alert this
    /// boot. ManualRequired is terminal (work items #73/#76): log/alert once,
    /// then stop retrying instead of retrying every check interval. This
    /// covers both exhaustion (`repair_attempts >= max`) and outcomes that
    /// do not consume an attempt (e.g. builder disabled), which would
    /// otherwise loop forever (work item #76).
    manual_alerts: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl DefaultSelfRepair {
    /// Create a new self-repair instance.
    pub fn new(
        context_manager: Arc<ContextManager>,
        stuck_threshold: Duration,
        max_repair_attempts: u32,
    ) -> Self {
        Self {
            context_manager,
            stuck_threshold,
            max_repair_attempts,
            store: None,
            builder: None,
            tools: None,
            manual_alerts: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// Add a Store for tool failure tracking.
    pub fn with_store(mut self, store: Arc<dyn Database>) -> Self {
        self.store = Some(store);
        self
    }

    /// Add a Builder and ToolRegistry for automatic tool repair.
    pub fn with_builder(
        mut self,
        builder: Arc<dyn SoftwareBuilder>,
        tools: Arc<ToolRegistry>,
    ) -> Self {
        self.builder = Some(builder);
        self.tools = Some(tools);
        self
    }

    /// Split out tools in a terminal repair state.
    ///
    /// ManualRequired is terminal (work items #73/#76): a terminal tool is
    /// surfaced exactly once per boot so the caller logs and alerts on it,
    /// then it is filtered out of every subsequent detection cycle instead
    /// of being retried (and logged) every check interval forever. A tool
    /// is terminal when its attempts are exhausted OR when a repair already
    /// returned `ManualRequired` this boot without consuming an attempt
    /// (e.g. builder disabled — see `note_manual_required`).
    fn filter_terminal_tools(&self, tools: Vec<BrokenTool>) -> Vec<BrokenTool> {
        let mut detected = Vec::with_capacity(tools.len());
        let mut alerts = self
            .manual_alerts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for tool in tools {
            let terminal =
                tool.repair_attempts >= self.max_repair_attempts || alerts.contains(&tool.name);
            if !terminal {
                detected.push(tool);
                continue;
            }
            if alerts.insert(tool.name.clone()) {
                tracing::warn!(
                    tool = %tool.name,
                    repair_attempts = tool.repair_attempts,
                    "Tool repair attempts exhausted; manual intervention required \
                     (suppressing further repair attempts this boot)"
                );
                detected.push(tool);
            }
        }
        detected
    }

    /// Record a terminal `ManualRequired` outcome for a tool this boot
    /// (work item #76). Outcomes that do not consume a repair attempt —
    /// builder/store unavailable — would otherwise be retried every check
    /// interval forever, because `filter_terminal_tools` keys terminal
    /// state on the attempt counter. Marking here makes every
    /// `ManualRequired` terminal per boot regardless of cause.
    fn note_manual_required(&self, tool_name: &str) {
        let mut alerts = self
            .manual_alerts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        alerts.insert(tool_name.to_string());
    }
}

#[async_trait]
impl SelfRepair for DefaultSelfRepair {
    async fn detect_stuck_jobs(&self) -> Vec<StuckJob> {
        let stuck_ids = self
            .context_manager
            .find_stuck_jobs_with_threshold(Some(self.stuck_threshold))
            .await;
        let mut stuck_jobs = Vec::new();

        for job_id in stuck_ids {
            if let Ok(ctx) = self.context_manager.get_context(job_id).await
                && matches!(ctx.state, JobState::Stuck | JobState::InProgress)
            {
                // WI#325: cron tick jobs carry their own heartbeat watchdog in
                // the cron supervisor (default 900s, well above the self-repair
                // stuck threshold). Repairing them here would double-cancel the
                // tick and spam the owner with "Job ... was stuck" notices for a
                // recovery the supervisor is already performing.
                if ctx.metadata.get("cron_mode") == Some(&serde_json::Value::Bool(true)) {
                    continue;
                }
                // InProgress jobs detected by threshold need to be transitioned
                // to Stuck before they can be repaired (attempt_recovery requires
                // Stuck state). These jobs already passed the threshold check in
                // find_stuck_jobs_with_threshold, so skip the duration filter below.
                let just_transitioned = ctx.state == JobState::InProgress;
                if just_transitioned {
                    let reason = "exceeded stuck_threshold";
                    let transition = self
                        .context_manager
                        .update_context(job_id, |ctx| ctx.mark_stuck(reason))
                        .await;
                    match transition {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            tracing::warn!(
                                job = %job_id,
                                "Failed to mark InProgress job as Stuck: {}",
                                e
                            );
                            continue;
                        }
                        Err(e) => {
                            tracing::warn!(
                                job = %job_id,
                                "Failed to transition InProgress job to Stuck: {}",
                                e
                            );
                            continue;
                        }
                    }
                }

                // Re-fetch context after potential InProgress->Stuck transition
                // so that stuck_since picks up the new transition timestamp.
                let ctx = match self.context_manager.get_context(job_id).await {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                // Use the timestamp of the most recent Stuck transition, not started_at.
                // A job that ran for hours before becoming stuck should not immediately
                // exceed the threshold — we measure from when it actually became stuck.
                let stuck_since = ctx
                    .transitions
                    .iter()
                    .rev()
                    .find(|t| t.to == JobState::Stuck)
                    .map(|t| t.timestamp);

                let stuck_duration = stuck_since
                    .map(|ts| {
                        let duration = Utc::now().signed_duration_since(ts);
                        Duration::from_secs(duration.num_seconds().max(0) as u64)
                    })
                    .unwrap_or_default();

                // Only report already-Stuck jobs that have been stuck long enough.
                // Jobs just transitioned from InProgress skip this check — they
                // were already vetted by find_stuck_jobs_with_threshold.
                if !just_transitioned && stuck_duration < self.stuck_threshold {
                    continue;
                }

                stuck_jobs.push(StuckJob {
                    job_id,
                    last_activity: stuck_since.unwrap_or(ctx.created_at),
                    stuck_duration,
                    last_error: None,
                    repair_attempts: ctx.repair_attempts,
                });
            }
        }

        stuck_jobs
    }

    async fn repair_stuck_job(&self, job: &StuckJob) -> Result<RepairResult, RepairError> {
        // Check if we've exceeded max repair attempts
        if job.repair_attempts >= self.max_repair_attempts {
            return Ok(RepairResult::ManualRequired {
                message: format!(
                    "Job {} has exceeded maximum repair attempts ({})",
                    job.job_id, self.max_repair_attempts
                ),
            });
        }

        // Try to recover the job.
        // If the job is still InProgress (detected via stuck_threshold), transition
        // it to Stuck first so that attempt_recovery() can move it back to InProgress.
        let result = self
            .context_manager
            .update_context(job.job_id, |ctx| {
                if ctx.state == JobState::InProgress {
                    ctx.transition_to(JobState::Stuck, Some("exceeded stuck_threshold".into()))?;
                }
                ctx.attempt_recovery()
            })
            .await;

        match result {
            Ok(Ok(())) => {
                tracing::info!("Successfully recovered job {}", job.job_id);
                Ok(RepairResult::Success {
                    message: format!("Job {} recovered and will be retried", job.job_id),
                })
            }
            Ok(Err(e)) => {
                tracing::warn!("Failed to recover job {}: {}", job.job_id, e);
                Ok(RepairResult::Retry {
                    message: format!("Recovery attempt failed: {}", e),
                })
            }
            Err(e) => Err(RepairError::Failed {
                target_type: "job".to_string(),
                target_id: job.job_id,
                reason: e.to_string(),
            }),
        }
    }

    async fn detect_broken_tools(&self) -> Vec<BrokenTool> {
        let Some(ref store) = self.store else {
            return vec![];
        };

        // Threshold: 5 failures before considering a tool broken
        match store.get_broken_tools(5).await {
            Ok(tools) => {
                if !tools.is_empty() {
                    tracing::info!("Detected {} broken tools needing repair", tools.len());
                }
                self.filter_terminal_tools(tools)
            }
            Err(e) => {
                tracing::warn!("Failed to detect broken tools: {}", e);
                vec![]
            }
        }
    }

    async fn repair_broken_tool(&self, tool: &BrokenTool) -> Result<RepairResult, RepairError> {
        let Some(ref builder) = self.builder else {
            self.note_manual_required(&tool.name);
            return Ok(RepairResult::ManualRequired {
                message: format!("Builder not available for repairing tool '{}'", tool.name),
            });
        };

        let Some(ref store) = self.store else {
            self.note_manual_required(&tool.name);
            return Ok(RepairResult::ManualRequired {
                message: "Store not available for tracking repair".to_string(),
            });
        };

        // Check repair attempt limit
        if tool.repair_attempts >= self.max_repair_attempts {
            self.note_manual_required(&tool.name);
            return Ok(RepairResult::ManualRequired {
                message: format!(
                    "Tool '{}' exceeded max repair attempts ({})",
                    tool.name, self.max_repair_attempts
                ),
            });
        }

        // Self-verify before burning a repair cycle (work item #73): a
        // builtin Rust tool ships inside the daemon binary, so a WASM
        // rebuild can never "repair" it — the recorded failures were
        // runtime/parameter errors, not a missing artifact. When the tool
        // is registered and loadable, clear its broken flag instead of
        // rebuilding (memory_read was flagged broken while fully functional).
        if let Some(ref tools) = self.tools
            && let Some(registered) = tools.get(&tool.name).await
            && !registered.is_wasm_tool()
        {
            if let Err(e) = store.mark_tool_repaired(&tool.name).await {
                tracing::warn!("Failed to clear broken flag for builtin tool: {}", e);
            }
            return Ok(RepairResult::Success {
                message: format!(
                    "Tool '{}' is a registered builtin (not rebuildable); broken flag cleared",
                    tool.name
                ),
            });
        }

        tracing::info!(
            "Attempting to repair tool '{}' (attempt {})",
            tool.name,
            tool.repair_attempts + 1
        );

        // Increment repair attempts
        if let Err(e) = store.increment_repair_attempts(&tool.name).await {
            tracing::warn!("Failed to increment repair attempts: {}", e);
        }

        // Create BuildRequirement for repair
        let requirement = BuildRequirement {
            name: tool.name.clone(),
            description: format!(
                "Repair broken WASM tool.\n\n\
                 Tool name: {}\n\
                 Previous error: {}\n\
                 Failure count: {}\n\n\
                 Analyze the error, fix the implementation, and rebuild.",
                tool.name,
                tool.last_error.as_deref().unwrap_or("Unknown error"),
                tool.failure_count
            ),
            software_type: SoftwareType::WasmTool,
            language: Language::Rust,
            input_spec: None,
            output_spec: None,
            dependencies: vec![],
            capabilities: vec!["http".to_string(), "workspace".to_string()],
        };

        // Attempt to build/repair
        match builder.build(&requirement).await {
            Ok(result) if result.success => {
                tracing::info!(
                    "Successfully rebuilt tool '{}' after {} iterations",
                    tool.name,
                    result.iterations
                );

                // Mark as repaired in database
                if let Err(e) = store.mark_tool_repaired(&tool.name).await {
                    tracing::warn!("Failed to mark tool as repaired: {}", e);
                }

                if result.registered {
                    tracing::info!("Repaired tool '{}' auto-registered by builder", tool.name);
                }

                Ok(RepairResult::Success {
                    message: format!(
                        "Tool '{}' repaired successfully after {} iterations",
                        tool.name, result.iterations
                    ),
                })
            }
            Ok(result) => {
                // Build completed but failed
                tracing::warn!(
                    "Repair build for '{}' completed but failed: {:?}",
                    tool.name,
                    result.error
                );
                Ok(RepairResult::Retry {
                    message: format!(
                        "Repair attempt {} for '{}' failed: {}",
                        tool.repair_attempts + 1,
                        tool.name,
                        result.error.unwrap_or_else(|| "Unknown error".to_string())
                    ),
                })
            }
            Err(e) => {
                tracing::error!("Repair build for '{}' errored: {}", tool.name, e);
                Ok(RepairResult::Retry {
                    message: format!("Repair build error: {}", e),
                })
            }
        }
    }
}

/// Background repair task that periodically checks for and repairs issues.
pub struct RepairTask {
    repair: Arc<dyn SelfRepair>,
    check_interval: Duration,
}

impl RepairTask {
    /// Create a new repair task.
    pub fn new(repair: Arc<dyn SelfRepair>, check_interval: Duration) -> Self {
        Self {
            repair,
            check_interval,
        }
    }

    /// Run the repair task.
    pub async fn run(&self) {
        loop {
            tokio::time::sleep(self.check_interval).await;

            // Check for stuck jobs
            let stuck_jobs = self.repair.detect_stuck_jobs().await;
            for job in stuck_jobs {
                match self.repair.repair_stuck_job(&job).await {
                    Ok(RepairResult::Success { message }) => {
                        tracing::info!(job = %job.job_id, status = "success", "Stuck job repair completed: {}", message);
                    }
                    Ok(RepairResult::Retry { message }) => {
                        tracing::debug!(job = %job.job_id, status = "retry", "Stuck job repair needs retry: {}", message);
                    }
                    Ok(RepairResult::Failed { message }) => {
                        tracing::error!(job = %job.job_id, status = "failed", "Stuck job repair failed: {}", message);
                    }
                    Ok(RepairResult::ManualRequired { message }) => {
                        tracing::warn!(job = %job.job_id, status = "manual", "Stuck job repair requires manual intervention: {}", message);
                    }
                    Err(e) => {
                        tracing::error!(job = %job.job_id, "Stuck job repair error: {}", e);
                    }
                }
            }

            // Check for broken tools
            let broken_tools = self.repair.detect_broken_tools().await;
            for tool in broken_tools {
                match self.repair.repair_broken_tool(&tool).await {
                    Ok(result) => {
                        tracing::debug!(tool = %tool.name, status = "completed", "Tool repair completed: {:?}", result);
                    }
                    Err(e) => {
                        tracing::error!(tool = %tool.name, "Tool repair error: {}", e);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_repair_result_variants() {
        let success = RepairResult::Success {
            message: "OK".to_string(),
        };
        assert!(matches!(success, RepairResult::Success { .. }));

        let manual = RepairResult::ManualRequired {
            message: "Help needed".to_string(),
        };
        assert!(matches!(manual, RepairResult::ManualRequired { .. }));
    }

    // === QA Plan - Self-repair stuck job tests ===

    #[tokio::test]
    async fn detect_no_stuck_jobs_when_all_healthy() {
        let cm = Arc::new(ContextManager::new(10));

        // Create a job and leave it Pending (not stuck).
        cm.create_job("Job 1", "desc").await.unwrap();

        let repair = DefaultSelfRepair::new(cm, Duration::from_secs(60), 3);
        let stuck = repair.detect_stuck_jobs().await;
        assert!(stuck.is_empty());
    }

    #[tokio::test]
    async fn detect_stuck_job_finds_stuck_state() {
        let cm = Arc::new(ContextManager::new(10));
        let job_id = cm.create_job("Stuck job", "desc").await.unwrap();

        // Transition to InProgress, then to Stuck.
        cm.update_context(job_id, |ctx| ctx.transition_to(JobState::InProgress, None))
            .await
            .unwrap()
            .unwrap();
        cm.update_context(job_id, |ctx| {
            ctx.transition_to(JobState::Stuck, Some("timed out".to_string()))
        })
        .await
        .unwrap()
        .unwrap();

        // Use zero threshold so the just-stuck job is detected immediately.
        let repair = DefaultSelfRepair::new(cm, Duration::from_secs(0), 3);
        let stuck = repair.detect_stuck_jobs().await;
        assert_eq!(stuck.len(), 1);
        assert_eq!(stuck[0].job_id, job_id);
    }

    /// WI#325: cron tick jobs are supervised by the cron mission supervisor's
    /// own heartbeat watchdog; self-repair must leave them alone.
    #[tokio::test]
    async fn detect_stuck_jobs_skips_cron_mode_jobs() {
        let cm = Arc::new(ContextManager::new(10));
        let cron_job = cm.create_job("Cron tick", "desc").await.unwrap();
        let normal_job = cm.create_job("Normal job", "desc").await.unwrap();

        for (job_id, cron_mode) in [(cron_job, true), (normal_job, false)] {
            cm.update_context(job_id, |ctx| ctx.transition_to(JobState::InProgress, None))
                .await
                .unwrap()
                .unwrap();
            if cron_mode {
                cm.update_context(job_id, |ctx| {
                    ctx.metadata = serde_json::json!({"cron_mode": true});
                    Ok(())
                })
                .await
                .unwrap();
            }
            cm.update_context(job_id, |ctx| {
                ctx.transition_to(JobState::Stuck, Some("idle".to_string()))
            })
            .await
            .unwrap()
            .unwrap();
        }

        let repair = DefaultSelfRepair::new(cm, Duration::from_secs(0), 3);
        let stuck = repair.detect_stuck_jobs().await;
        assert_eq!(stuck.len(), 1);
        assert_eq!(stuck[0].job_id, normal_job);
    }

    #[tokio::test]
    async fn repair_stuck_job_succeeds_within_limit() {
        let cm = Arc::new(ContextManager::new(10));
        let job_id = cm.create_job("Repairable", "desc").await.unwrap();

        // Move to InProgress -> Stuck.
        cm.update_context(job_id, |ctx| ctx.transition_to(JobState::InProgress, None))
            .await
            .unwrap()
            .unwrap();
        cm.update_context(job_id, |ctx| ctx.transition_to(JobState::Stuck, None))
            .await
            .unwrap()
            .unwrap();

        let repair = DefaultSelfRepair::new(Arc::clone(&cm), Duration::from_secs(60), 3);

        let stuck_job = StuckJob {
            job_id,
            last_activity: Utc::now(),
            stuck_duration: Duration::from_secs(120),
            last_error: None,
            repair_attempts: 0,
        };

        let result = repair.repair_stuck_job(&stuck_job).await.unwrap();
        assert!(
            matches!(result, RepairResult::Success { .. }),
            "Expected Success, got: {:?}",
            result
        );

        // Job should be back to InProgress after recovery.
        let ctx = cm.get_context(job_id).await.unwrap();
        assert_eq!(ctx.state, JobState::InProgress);
    }

    #[tokio::test]
    async fn repair_stuck_job_returns_manual_when_limit_exceeded() {
        let cm = Arc::new(ContextManager::new(10));
        let job_id = cm.create_job("Unrepairable", "desc").await.unwrap();

        let repair = DefaultSelfRepair::new(cm, Duration::from_secs(60), 2);

        let stuck_job = StuckJob {
            job_id,
            last_activity: Utc::now(),
            stuck_duration: Duration::from_secs(300),
            last_error: Some("persistent failure".to_string()),
            repair_attempts: 2, // == max
        };

        let result = repair.repair_stuck_job(&stuck_job).await.unwrap();
        assert!(
            matches!(result, RepairResult::ManualRequired { .. }),
            "Expected ManualRequired, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn detect_and_repair_in_progress_job_via_threshold() {
        let cm = Arc::new(ContextManager::new(10));
        let job_id = cm.create_job("Long running", "desc").await.unwrap();

        // Transition to InProgress.
        cm.update_context(job_id, |ctx| ctx.transition_to(JobState::InProgress, None))
            .await
            .unwrap()
            .unwrap();

        // Backdate started_at to simulate a job running for 10 minutes.
        cm.update_context(job_id, |ctx| {
            ctx.started_at = Some(Utc::now() - chrono::Duration::seconds(600));
        })
        .await
        .unwrap();

        // Use a 5-minute threshold so the 10-minute job is detected.
        let repair = DefaultSelfRepair::new(Arc::clone(&cm), Duration::from_secs(300), 3);

        // detect_stuck_jobs should find it and transition InProgress -> Stuck.
        let stuck = repair.detect_stuck_jobs().await;
        assert_eq!(stuck.len(), 1);
        assert_eq!(stuck[0].job_id, job_id);

        // After detection the job should now be in Stuck state.
        let ctx = cm.get_context(job_id).await.unwrap();
        assert_eq!(ctx.state, JobState::Stuck);

        // Repair should recover it: Stuck -> InProgress.
        let result = repair.repair_stuck_job(&stuck[0]).await.unwrap();
        assert!(
            matches!(result, RepairResult::Success { .. }),
            "Expected Success, got: {:?}",
            result
        );

        // Job should be back to InProgress after recovery.
        let ctx = cm.get_context(job_id).await.unwrap();
        assert_eq!(ctx.state, JobState::InProgress);
    }

    #[tokio::test]
    async fn detect_broken_tools_returns_empty_without_store() {
        let cm = Arc::new(ContextManager::new(10));
        let repair = DefaultSelfRepair::new(cm, Duration::from_secs(60), 3);

        // No store configured, should return empty.
        let broken = repair.detect_broken_tools().await;
        assert!(broken.is_empty());
    }

    #[tokio::test]
    async fn repair_broken_tool_returns_manual_without_builder() {
        let cm = Arc::new(ContextManager::new(10));
        let repair = DefaultSelfRepair::new(cm, Duration::from_secs(60), 3);

        let broken = BrokenTool {
            name: "test-tool".to_string(),
            failure_count: 10,
            last_error: Some("crash".to_string()),
            first_failure: Utc::now(),
            last_failure: Utc::now(),
            last_build_result: None,
            repair_attempts: 0,
        };

        let result = repair.repair_broken_tool(&broken).await.unwrap();
        assert!(
            matches!(result, RepairResult::ManualRequired { .. }),
            "Expected ManualRequired without builder, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn detect_stuck_jobs_filters_by_threshold() {
        let cm = Arc::new(ContextManager::new(10));
        let job_id = cm.create_job("Stuck job", "desc").await.unwrap();

        // Transition to InProgress, then to Stuck.
        cm.update_context(job_id, |ctx| ctx.transition_to(JobState::InProgress, None))
            .await
            .unwrap()
            .unwrap();
        cm.update_context(job_id, |ctx| {
            ctx.transition_to(JobState::Stuck, Some("timed out".to_string()))
        })
        .await
        .unwrap()
        .unwrap();

        // Use a very large threshold (1 hour). Job just became stuck, so
        // stuck_duration < threshold. It should be filtered out.
        let repair = DefaultSelfRepair::new(cm, Duration::from_secs(3600), 3);
        let stuck = repair.detect_stuck_jobs().await;
        assert!(
            stuck.is_empty(),
            "Job stuck for <1s should be filtered by 1h threshold"
        );
    }

    #[tokio::test]
    async fn detect_stuck_jobs_includes_when_over_threshold() {
        let cm = Arc::new(ContextManager::new(10));
        let job_id = cm.create_job("Stuck job", "desc").await.unwrap();

        // Transition to InProgress, then to Stuck.
        cm.update_context(job_id, |ctx| ctx.transition_to(JobState::InProgress, None))
            .await
            .unwrap()
            .unwrap();
        cm.update_context(job_id, |ctx| {
            ctx.transition_to(JobState::Stuck, Some("timed out".to_string()))
        })
        .await
        .unwrap()
        .unwrap();

        // Use a zero threshold -- any stuck duration should be included.
        let repair = DefaultSelfRepair::new(cm, Duration::from_secs(0), 3);
        let stuck = repair.detect_stuck_jobs().await;
        assert_eq!(stuck.len(), 1, "Job should be detected with zero threshold");
        assert_eq!(stuck[0].job_id, job_id);
    }

    /// Regression: stuck_duration must be measured from the Stuck transition,
    /// not from started_at. A job that ran for 2 hours before becoming stuck
    /// should NOT immediately exceed a 5-minute threshold.
    #[tokio::test]
    async fn stuck_duration_measured_from_stuck_transition_not_started_at() {
        let cm = Arc::new(ContextManager::new(10));
        let job_id = cm.create_job("Long runner", "desc").await.unwrap();

        // Transition to InProgress (sets started_at to now).
        cm.update_context(job_id, |ctx| ctx.transition_to(JobState::InProgress, None))
            .await
            .unwrap()
            .unwrap();

        // Backdate started_at to 2 hours ago to simulate a long-running job.
        cm.update_context(job_id, |ctx| {
            ctx.started_at = Some(Utc::now() - chrono::Duration::hours(2));
            Ok::<(), crate::error::Error>(())
        })
        .await
        .unwrap()
        .unwrap();

        // Now transition to Stuck (stuck transition timestamp is ~now).
        cm.update_context(job_id, |ctx| {
            ctx.transition_to(JobState::Stuck, Some("wedged".into()))
        })
        .await
        .unwrap()
        .unwrap();

        // With a 5-minute threshold, the job JUST became stuck — should NOT be detected.
        let repair = DefaultSelfRepair::new(cm, Duration::from_secs(300), 3);
        let stuck = repair.detect_stuck_jobs().await;
        assert!(
            stuck.is_empty(),
            "Job stuck for <1s should not exceed 5min threshold, \
             but stuck_duration was computed from started_at (2h ago)"
        );
    }

    /// Mock SoftwareBuilder that returns a successful build result.
    #[cfg(feature = "libsql")]
    struct MockBuilder {
        build_count: std::sync::atomic::AtomicU32,
    }

    #[cfg(feature = "libsql")]
    impl MockBuilder {
        fn new() -> Self {
            Self {
                build_count: std::sync::atomic::AtomicU32::new(0),
            }
        }

        fn builds(&self) -> u32 {
            self.build_count.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    #[cfg(feature = "libsql")]
    #[async_trait]
    impl crate::tools::SoftwareBuilder for MockBuilder {
        async fn analyze(
            &self,
            _description: &str,
        ) -> Result<crate::tools::BuildRequirement, crate::error::ToolError> {
            Ok(crate::tools::BuildRequirement {
                name: "mock-tool".to_string(),
                description: "mock".to_string(),
                software_type: crate::tools::SoftwareType::WasmTool,
                language: crate::tools::Language::Rust,
                input_spec: None,
                output_spec: None,
                dependencies: vec![],
                capabilities: vec![],
            })
        }

        async fn build(
            &self,
            requirement: &crate::tools::BuildRequirement,
        ) -> Result<crate::tools::BuildResult, crate::error::ToolError> {
            self.build_count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(crate::tools::BuildResult {
                build_id: Uuid::new_v4(),
                requirement: requirement.clone(),
                artifact_path: std::path::PathBuf::from("/tmp/mock.wasm"),
                logs: vec![],
                success: true,
                error: None,
                started_at: Utc::now(),
                completed_at: Utc::now(),
                iterations: 1,
                validation_warnings: vec![],
                tests_passed: 1,
                tests_failed: 0,
                registered: true,
            })
        }

        async fn repair(
            &self,
            _result: &crate::tools::BuildResult,
            _error: &str,
        ) -> Result<crate::tools::BuildResult, crate::error::ToolError> {
            unimplemented!("not needed for this test")
        }
    }

    /// E2E test: stuck job detected -> repaired -> transitions back to InProgress,
    /// and broken tool detected -> builder invoked -> tool marked repaired.
    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn e2e_stuck_job_repair_and_tool_rebuild() {
        // --- Setup ---
        let cm = Arc::new(ContextManager::new(10));
        let job_id = cm.create_job("E2E stuck job", "desc").await.unwrap();

        // Transition job: Pending -> InProgress -> Stuck
        cm.update_context(job_id, |ctx| ctx.transition_to(JobState::InProgress, None))
            .await
            .unwrap()
            .unwrap();
        cm.update_context(job_id, |ctx| {
            ctx.transition_to(JobState::Stuck, Some("deadlocked".to_string()))
        })
        .await
        .unwrap()
        .unwrap();

        // Create a mock builder and a real test database (for store)
        let builder = Arc::new(MockBuilder::new());
        let tools = Arc::new(ToolRegistry::new());
        let (db, _tmp_dir) = crate::testing::test_db().await;

        // Create self-repair with zero threshold (detect immediately),
        // wired with store, builder, and tools.
        let repair = DefaultSelfRepair::new(Arc::clone(&cm), Duration::from_secs(0), 3)
            .with_store(Arc::clone(&db))
            .with_builder(
                Arc::clone(&builder) as Arc<dyn crate::tools::SoftwareBuilder>,
                tools,
            );

        // --- Phase 1: Detect and repair stuck job ---
        let stuck_jobs = repair.detect_stuck_jobs().await;
        assert_eq!(stuck_jobs.len(), 1, "Should detect the stuck job");
        assert_eq!(stuck_jobs[0].job_id, job_id);

        let result = repair.repair_stuck_job(&stuck_jobs[0]).await.unwrap();
        assert!(
            matches!(result, RepairResult::Success { .. }),
            "Job repair should succeed: {:?}",
            result
        );

        // Verify job transitioned back to InProgress
        let ctx = cm.get_context(job_id).await.unwrap();
        assert_eq!(
            ctx.state,
            JobState::InProgress,
            "Job should be back to InProgress after repair"
        );

        // --- Phase 2: Repair a broken tool via builder ---
        let broken = BrokenTool {
            name: "broken-wasm-tool".to_string(),
            failure_count: 10,
            last_error: Some("panic in tool execution".to_string()),
            first_failure: Utc::now() - chrono::Duration::hours(1),
            last_failure: Utc::now(),
            last_build_result: None,
            repair_attempts: 0,
        };

        let tool_result = repair.repair_broken_tool(&broken).await.unwrap();
        assert!(
            matches!(tool_result, RepairResult::Success { .. }),
            "Tool repair should succeed with mock builder: {:?}",
            tool_result
        );

        // Verify builder was actually invoked
        assert_eq!(builder.builds(), 1, "Builder should have been called once");
    }

    /// A minimal builtin (non-WASM) tool for self-verify tests.
    #[cfg(feature = "libsql")]
    #[derive(Debug)]
    struct MockBuiltinTool;

    #[cfg(feature = "libsql")]
    #[async_trait]
    impl crate::tools::Tool for MockBuiltinTool {
        fn name(&self) -> &str {
            "memory_read"
        }

        fn description(&self) -> &str {
            "mock builtin"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(
            &self,
            _params: serde_json::Value,
            _ctx: &crate::context::JobContext,
        ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
            Ok(crate::tools::ToolOutput::text(
                "ok",
                Duration::from_millis(1),
            ))
        }
    }

    /// Work item #73.3: a registered builtin tool flagged "broken" must not
    /// be rebuilt (a WASM rebuild can never repair a Rust builtin). The
    /// watchdog self-verifies — the tool is loadable — and clears the flag
    /// without burning a repair cycle.
    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn repair_broken_tool_clears_flag_for_registered_builtin() {
        let cm = Arc::new(ContextManager::new(10));
        let builder = Arc::new(MockBuilder::new());
        let tools = Arc::new(ToolRegistry::new());
        tools.register(Arc::new(MockBuiltinTool)).await;
        let (db, _tmp_dir) = crate::testing::test_db().await;

        // Seed a broken-looking row for the builtin.
        for _ in 0..5 {
            db.record_tool_failure("memory_read", "transient error")
                .await
                .unwrap();
        }
        assert_eq!(db.get_broken_tools(5).await.unwrap().len(), 1);

        let repair = DefaultSelfRepair::new(cm, Duration::from_secs(0), 3)
            .with_store(Arc::clone(&db))
            .with_builder(
                Arc::clone(&builder) as Arc<dyn crate::tools::SoftwareBuilder>,
                tools,
            );

        let broken = BrokenTool {
            name: "memory_read".to_string(),
            failure_count: 5,
            last_error: Some("transient error".to_string()),
            first_failure: Utc::now(),
            last_failure: Utc::now(),
            last_build_result: None,
            repair_attempts: 0,
        };

        let result = repair.repair_broken_tool(&broken).await.unwrap();
        assert!(
            matches!(result, RepairResult::Success { .. }),
            "builtin self-verify should report Success, got: {:?}",
            result
        );
        assert_eq!(
            builder.builds(),
            0,
            "builder must not run for registered builtin tools"
        );
        assert!(
            db.get_broken_tools(5).await.unwrap().is_empty(),
            "broken flag should be cleared after self-verify"
        );
    }

    /// Work item #73.1: ManualRequired is terminal. A tool that exhausted
    /// its repair attempts is surfaced exactly once per boot (so the alert
    /// fires), then filtered out of subsequent detection cycles instead of
    /// being retried every interval forever.
    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn detect_broken_tools_surfaces_exhausted_tool_once_per_boot() {
        let cm = Arc::new(ContextManager::new(10));
        let (db, _tmp_dir) = crate::testing::test_db().await;

        for _ in 0..5 {
            db.record_tool_failure("flaky-tool", "boom").await.unwrap();
        }
        for _ in 0..3 {
            db.increment_repair_attempts("flaky-tool").await.unwrap();
        }

        let repair = DefaultSelfRepair::new(cm, Duration::from_secs(0), 3).with_store(db);

        let first = repair.detect_broken_tools().await;
        assert_eq!(
            first.len(),
            1,
            "exhausted tool should surface once for the terminal alert"
        );
        assert_eq!(first[0].name, "flaky-tool");

        let second = repair.detect_broken_tools().await;
        assert!(
            second.is_empty(),
            "exhausted tool must be filtered out after the one-time alert"
        );
    }

    /// Work item #76: a `ManualRequired` outcome that does not consume a
    /// repair attempt (builder disabled/unavailable) must also be terminal
    /// per boot. Otherwise the attempt counter never reaches the max, the
    /// terminal filter never engages, and the watchdog retries + alerts
    /// every check interval forever.
    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn manual_required_without_builder_suppresses_future_cycles() {
        let cm = Arc::new(ContextManager::new(10));
        let (db, _tmp_dir) = crate::testing::test_db().await;

        for _ in 0..5 {
            db.record_tool_failure("wasm-tool", "boom").await.unwrap();
        }

        // Store but NO builder: repair_broken_tool returns ManualRequired
        // without incrementing the attempt counter.
        let repair = DefaultSelfRepair::new(cm, Duration::from_secs(0), 3).with_store(db);

        let detected = repair.detect_broken_tools().await;
        assert_eq!(detected.len(), 1, "broken tool should be detected");
        assert_eq!(detected[0].repair_attempts, 0);

        let result = repair.repair_broken_tool(&detected[0]).await.unwrap();
        assert!(
            matches!(result, RepairResult::ManualRequired { .. }),
            "no builder should yield ManualRequired, got: {:?}",
            result
        );

        assert!(
            repair.detect_broken_tools().await.is_empty(),
            "ManualRequired must be terminal: tool suppressed after the alert"
        );
    }

    /// Work item #73.3: last-known-good gate — a successful execution clears
    /// the accumulated failure state so functional tools are not flagged.
    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn record_tool_success_clears_failure_state() {
        let (db, _tmp_dir) = crate::testing::test_db().await;

        for _ in 0..5 {
            db.record_tool_failure("noisy-tool", "boom").await.unwrap();
        }
        db.increment_repair_attempts("noisy-tool").await.unwrap();
        assert_eq!(db.get_broken_tools(5).await.unwrap().len(), 1);

        db.record_tool_success("noisy-tool").await.unwrap();
        assert!(
            db.get_broken_tools(5).await.unwrap().is_empty(),
            "success should clear the broken flag"
        );

        // Failure history can re-accumulate after success, and the repair
        // counter must have been reset too.
        for _ in 0..5 {
            db.record_tool_failure("noisy-tool", "boom").await.unwrap();
        }
        let tools = db.get_broken_tools(5).await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(
            tools[0].repair_attempts, 0,
            "success should also reset repair attempts"
        );
    }

    /// Work item #73.2: repair-attempt counters persist in the DB; the
    /// engine resets them at startup so each boot gets a fresh cycle.
    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn reset_tool_repair_attempts_gives_fresh_cycle() {
        let (db, _tmp_dir) = crate::testing::test_db().await;

        for _ in 0..5 {
            db.record_tool_failure("tired-tool", "boom").await.unwrap();
        }
        for _ in 0..3 {
            db.increment_repair_attempts("tired-tool").await.unwrap();
        }

        let reset = db.reset_tool_repair_attempts().await.unwrap();
        assert_eq!(reset, 1, "one tool had a non-zero counter");

        let tools = db.get_broken_tools(5).await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].repair_attempts, 0);

        // Idempotent: nothing left to reset.
        let again = db.reset_tool_repair_attempts().await.unwrap();
        assert_eq!(again, 0);
    }
}
