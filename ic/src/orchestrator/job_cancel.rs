//! Shared cancellation sequencing for jobs (WI#476).
//!
//! Cancelling a job changes three things that must agree: the runtime that
//! owns the job has to stop, the in-memory [`ContextManager`] entry has to
//! move to `Cancelled`, and the persisted sandbox-job status has to become
//! `cancelled`. The web route and the `cancel_job` tool used to implement
//! that sequence separately, and they drifted: the web route called
//! `ContainerJobManager::stop_job`, which claims the terminal container state
//! before container I/O, so the exit watcher's `claim_unreported_exit`
//! returned false and never updated the context. A cancelled container job
//! therefore stayed `InProgress` in memory while the API reported it
//! `cancelled`, and the self-repair sweep kept flagging it.
//!
//! Both callers now go through [`JobCanceller`] so every runtime kind gets
//! the same order: deliver the cancel, persist the terminal status, then
//! move the context. The DB write happens before the context transition so a
//! delivered cancel can never leave the row `running` because the in-memory
//! update failed; store and context failures are reported separately.

use std::sync::Arc;

use chrono::Utc;
use uuid::Uuid;

use crate::context::{ContextManager, JobState};
use crate::db::Database;
use crate::error::OrchestratorError;
use crate::orchestrator::external_worker::ExternalWorkerManager;
use crate::orchestrator::job_manager::ContainerJobManager;
use crate::tools::builtin::SchedulerSlot;

/// The persisted/context reason for an owner-initiated cancellation. Matches
/// `ExternalWorkerManager`'s `CancelReason::User` message so the job record
/// and `job_events` read the same for every runtime kind.
pub const USER_CANCEL_REASON: &str = "Cancelled by user";

/// What an undelivered cancel means for the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelPolicy {
    /// A runtime backend must accept the cancel before the stores are
    /// touched. The web route uses this: a sandbox row can outlive its
    /// runtime (daemon restart), and reporting `cancelled` without a runtime
    /// to stop would hide that.
    RequireRuntime,
    /// With no runtime backend configured at all, still synchronize the
    /// stores. The `cancel_job` tool uses this: agent jobs live in the
    /// context/scheduler it already holds, and a context-only cancellation
    /// is the supported path there.
    AllowContextOnly,
}

/// A cancellation that could not be delivered.
#[derive(Debug, thiserror::Error)]
pub enum JobCancelError {
    /// The job is mid write-back promotion; stopping would clear the secrets
    /// and staging dir the promotion is walking (DH-4).
    #[error(
        "job {job_id} is finalizing write-back promotion; cancellation is refused — retry once it completes"
    )]
    Finalizing { job_id: Uuid },
    /// The scheduler backend refused the stop.
    #[error("failed to stop scheduled job: {message}")]
    Scheduler { job_id: Uuid, message: String },
    /// No configured backend owned (or would accept) the job.
    #[error("job {job_id} has no active runtime to cancel")]
    NoActiveRuntime { job_id: Uuid },
}

/// What the cancellation sequence achieved.
///
/// A delivered cancel is not thrown away when a later store update fails:
/// both failures are carried here so callers can report the exact half that
/// did not land.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CancelReport {
    /// A runtime backend accepted the cancel.
    pub delivered: bool,
    /// The persisted sandbox status became `cancelled` (or there is no store
    /// to persist to).
    pub persisted: bool,
    /// Store failure, when the terminal status could not be written.
    pub persistence_error: Option<String>,
    /// The in-memory context is now `Cancelled`.
    pub context_synced: bool,
    /// Context failure, when the in-memory entry could not be updated or the
    /// stage was already terminal.
    pub context_error: Option<String>,
}

/// Delivery backends for one cancellation. Build it from whichever handles
/// the caller owns; unset backends are simply not attempted.
pub struct JobCanceller {
    context_manager: Option<Arc<ContextManager>>,
    scheduler: Option<SchedulerSlot>,
    job_manager: Option<Arc<ContainerJobManager>>,
    external_worker_manager: Option<Arc<ExternalWorkerManager>>,
    store: Option<Arc<dyn Database>>,
}

impl JobCanceller {
    pub fn new(context_manager: Option<Arc<ContextManager>>) -> Self {
        Self {
            context_manager,
            scheduler: None,
            job_manager: None,
            external_worker_manager: None,
            store: None,
        }
    }

    pub fn with_scheduler(mut self, scheduler: SchedulerSlot) -> Self {
        self.scheduler = Some(scheduler);
        self
    }

    pub fn with_container_jobs(mut self, job_manager: Arc<ContainerJobManager>) -> Self {
        self.job_manager = Some(job_manager);
        self
    }

    pub fn with_external_workers(mut self, manager: Arc<ExternalWorkerManager>) -> Self {
        self.external_worker_manager = Some(manager);
        self
    }

    pub fn with_store(mut self, store: Arc<dyn Database>) -> Self {
        self.store = Some(store);
        self
    }

    /// Deliver the cancel to every configured backend that owns the job,
    /// then persist the terminal status and move the context.
    ///
    /// Delivery is attempted on all backends (a job is enrolled in exactly
    /// one, and the extras are no-ops). `Finalizing` is the only delivery
    /// error: the job is mid write-back promotion and must not be touched.
    /// A scheduler stop error is surfaced because the task handle may still
    /// be live; container teardown noise after the terminal state is claimed
    /// is logged and the store/context sequence still runs.
    pub async fn cancel(
        &self,
        job_id: Uuid,
        policy: CancelPolicy,
    ) -> Result<CancelReport, JobCancelError> {
        let delivered = self.deliver(job_id).await?;
        let any_runtime_configured = self.external_worker_manager.is_some()
            || self.job_manager.is_some()
            || self.scheduler.is_some();
        if !delivered && (any_runtime_configured || policy == CancelPolicy::RequireRuntime) {
            return Err(JobCancelError::NoActiveRuntime { job_id });
        }

        let mut report = CancelReport {
            delivered,
            ..Default::default()
        };
        self.persist_terminal_status(job_id, &mut report).await;
        self.sync_context(job_id, &mut report).await;
        Ok(report)
    }

    async fn deliver(&self, job_id: Uuid) -> Result<bool, JobCancelError> {
        let mut delivered = false;

        if let Some(manager) = &self.external_worker_manager {
            delivered |= manager.cancel_task(job_id).await;
        }

        if let Some(slot) = &self.scheduler {
            // Clone the handle out so the slot lock is not held across the
            // scheduler's own awaits.
            let scheduler = slot.read().await.clone();
            if let Some(scheduler) = scheduler {
                delivered |=
                    scheduler
                        .stop(job_id)
                        .await
                        .map_err(|error| JobCancelError::Scheduler {
                            job_id,
                            message: error.to_string(),
                        })?;
            }
        }

        if let Some(manager) = &self.job_manager {
            match manager.stop_job(job_id).await {
                Ok(()) => delivered = true,
                // Nothing to stop: another backend or the agent owns it, or
                // the container has not been created yet (empty container id).
                Err(
                    OrchestratorError::ContainerNotFound { .. }
                    | OrchestratorError::InvalidContainerState { .. },
                ) => {}
                Err(OrchestratorError::JobFinalizing { .. }) => {
                    return Err(JobCancelError::Finalizing { job_id });
                }
                Err(OrchestratorError::Docker { .. }) => {
                    // `stop_job` claims the terminal container state before
                    // any Docker I/O, so this error lands after the claim and
                    // the watcher will never complete this job. Treat the
                    // cancellation as delivered — otherwise the context would
                    // stay InProgress and the stuck sweep would flag a job
                    // whose container handle is already Stopped. The reaper
                    // cleans up a container that outlived the stop request.
                    tracing::warn!(
                        job_id = %job_id,
                        "Container stop failed after claiming its terminal state during cancellation"
                    );
                    delivered = true;
                }
                Err(error) => {
                    tracing::warn!(
                        job_id = %job_id,
                        %error,
                        "Failed to stop container during cancellation"
                    );
                }
            }
        }

        Ok(delivered)
    }

    async fn persist_terminal_status(&self, job_id: Uuid, report: &mut CancelReport) {
        let Some(store) = &self.store else {
            // No sandbox record to reconcile (agent jobs the tool cancels).
            report.persisted = true;
            return;
        };
        match store
            .update_sandbox_job_status(
                job_id,
                "cancelled",
                Some(false),
                Some(USER_CANCEL_REASON),
                None,
                Some(Utc::now()),
            )
            .await
        {
            Ok(()) => report.persisted = true,
            Err(error) => {
                tracing::warn!(job_id = %job_id, %error, "Failed to persist job cancellation");
                report.persistence_error = Some(error.to_string());
            }
        }
    }

    async fn sync_context(&self, job_id: Uuid, report: &mut CancelReport) {
        let Some(context_manager) = &self.context_manager else {
            report.context_error = Some("job context manager is not available".to_string());
            return;
        };
        match context_manager
            .update_context(job_id, |context| {
                if context.state == JobState::Cancelled {
                    Ok(())
                } else {
                    context.transition_to(JobState::Cancelled, Some(USER_CANCEL_REASON.to_string()))
                }
            })
            .await
        {
            Ok(Ok(())) => report.context_synced = true,
            Ok(Err(reason)) => {
                tracing::warn!(job_id = %job_id, %reason, "Job context cannot be cancelled");
                report.context_error = Some(reason);
            }
            Err(error) => {
                tracing::warn!(job_id = %job_id, %error, "Failed to load job context for cancellation");
                report.context_error = Some(error.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn in_progress_job(manager: &Arc<ContextManager>) -> Uuid {
        let job_id = manager
            .create_job_for_user("default", "Cancel job", "track cancellation")
            .await
            .unwrap(); // safety: test-only
        manager
            .update_context(job_id, |ctx| ctx.transition_to(JobState::InProgress, None))
            .await
            .unwrap()
            .unwrap();
        job_id
    }

    #[tokio::test]
    async fn context_only_cancel_synchronizes_when_no_runtime_is_configured() {
        let manager = Arc::new(ContextManager::new(5));
        let job_id = in_progress_job(&manager).await;

        let report = JobCanceller::new(Some(Arc::clone(&manager)))
            .cancel(job_id, CancelPolicy::AllowContextOnly)
            .await
            .unwrap();

        assert!(!report.delivered);
        assert!(report.persisted);
        assert!(report.context_synced);
        assert_eq!(
            manager.get_context(job_id).await.unwrap().state,
            JobState::Cancelled
        );
    }

    #[tokio::test]
    async fn require_runtime_refuses_a_context_only_cancel() {
        let manager = Arc::new(ContextManager::new(5));
        let job_id = in_progress_job(&manager).await;

        let error = JobCanceller::new(Some(Arc::clone(&manager)))
            .cancel(job_id, CancelPolicy::RequireRuntime)
            .await
            .unwrap_err();

        assert!(matches!(error, JobCancelError::NoActiveRuntime { .. }));
        assert_eq!(
            manager.get_context(job_id).await.unwrap().state,
            JobState::InProgress
        );
    }
}
