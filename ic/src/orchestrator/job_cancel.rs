//! Shared cancellation sequencing for sandbox and external-worker jobs.
//!
//! Cancellation has three separate effects: stop the runtime, move the
//! in-memory context to `Cancelled`, and persist the `cancelled` sandbox-job
//! status. The web route and `cancel_job` tool use this module so container
//! and external runtimes cannot drift into different orderings.
//!
//! A still-cancellable context is transitioned before the durable write. This
//! prevents a completion that already won the terminal race from being
//! overwritten in the database. A missing context does not block persistence:
//! once cancellation is delivered, a context lookup failure is reported
//! separately from a store failure. If persistence fails after the context
//! moved, a caller may retry the durable write without requiring a runtime
//! handle that has already disappeared.

use std::sync::Arc;

use chrono::Utc;
use uuid::Uuid;

use crate::context::{ContextManager, JobState};
use crate::db::Database;
use crate::error::OrchestratorError;
use crate::orchestrator::external_worker::ExternalWorkerManager;
use crate::orchestrator::job_manager::ContainerJobManager;
use crate::tools::builtin::SchedulerSlot;

pub const USER_CANCEL_REASON: &str = "Cancelled by user";

/// Whether cancellation requires a runtime to accept the stop request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelPolicy {
    /// The web sandbox route requires a live runtime except when retrying a
    /// persistence failure for an already-cancelled context.
    RequireRuntime,
    /// The tool retains its context-only behavior when no runtime backends
    /// were configured at all.
    AllowContextOnly,
}

/// A cancellation that cannot proceed without overwriting a terminal result
/// or claiming a runtime stop that was not delivered.
#[derive(Debug, thiserror::Error)]
pub enum JobCancelError {
    #[error(
        "job {job_id} is finalizing write-back promotion; cancellation is refused — retry once it completes"
    )]
    Finalizing { job_id: Uuid },
    #[error("failed to stop scheduled job: {message}")]
    Scheduler { job_id: Uuid, message: String },
    #[error("job {job_id} has no active runtime to cancel")]
    NoActiveRuntime { job_id: Uuid },
    #[error(
        "job {job_id} finished in state '{state}' before cancellation could be synchronized; the terminal result was preserved"
    )]
    ContextConflict { job_id: Uuid, state: JobState },
}

/// The independent effects achieved by one cancellation attempt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CancelReport {
    pub delivered: bool,
    pub persisted: bool,
    pub persistence_error: Option<String>,
    pub context_synced: bool,
    pub context_error: Option<String>,
    /// The terminal container state was claimed and credentials were revoked,
    /// but container teardown still needs the reaper to finish it.
    pub runtime_stop_deferred: bool,
}

#[derive(Debug, Default)]
struct DeliveryOutcome {
    delivered: bool,
    runtime_stop_deferred: bool,
}

enum ContextSync {
    Synced,
    Conflict(JobState),
    Unavailable(String),
}

/// Cancellation backends owned by a caller. Unset backends are not attempted.
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

    pub async fn cancel(
        &self,
        job_id: Uuid,
        policy: CancelPolicy,
    ) -> Result<CancelReport, JobCancelError> {
        let persistence_retry = self.context_is_cancelled(job_id).await;
        let delivery = self.deliver(job_id).await?;
        let any_runtime_configured = self.external_worker_manager.is_some()
            || self.job_manager.is_some()
            || self.scheduler.is_some();
        if !delivery.delivered
            && !persistence_retry
            && (any_runtime_configured || policy == CancelPolicy::RequireRuntime)
        {
            return Err(JobCancelError::NoActiveRuntime { job_id });
        }

        let mut report = CancelReport {
            delivered: delivery.delivered,
            runtime_stop_deferred: delivery.runtime_stop_deferred,
            ..Default::default()
        };
        match self.sync_context(job_id).await {
            ContextSync::Synced => report.context_synced = true,
            ContextSync::Conflict(state) => {
                return Err(JobCancelError::ContextConflict { job_id, state });
            }
            ContextSync::Unavailable(error) => {
                tracing::warn!(job_id = %job_id, %error, "Failed to synchronize cancelled job context");
                report.context_error = Some(error);
            }
        }

        // A delivered cancel must become durable even when the context could
        // not be loaded. The only exception is the terminal conflict above,
        // where the completion result deliberately wins.
        self.persist_terminal_status(job_id, &mut report).await;
        Ok(report)
    }

    async fn context_is_cancelled(&self, job_id: Uuid) -> bool {
        match &self.context_manager {
            Some(manager) => manager
                .get_context(job_id)
                .await
                .is_ok_and(|context| context.state == JobState::Cancelled),
            None => false,
        }
    }

    async fn deliver(&self, job_id: Uuid) -> Result<DeliveryOutcome, JobCancelError> {
        let mut outcome = DeliveryOutcome::default();
        let mut scheduler_error = None;

        if let Some(manager) = &self.external_worker_manager {
            outcome.delivered |= manager.cancel_task(job_id).await;
        }

        if let Some(slot) = &self.scheduler {
            let scheduler = slot.read().await.clone();
            if let Some(scheduler) = scheduler {
                match scheduler.stop(job_id).await {
                    Ok(stopped) => outcome.delivered |= stopped,
                    Err(error) => scheduler_error = Some(error.to_string()),
                }
            }
        }

        if let Some(manager) = &self.job_manager {
            match manager.stop_job_with_outcome(job_id).await {
                Ok(stop) => {
                    outcome.delivered = true;
                    outcome.runtime_stop_deferred |= stop.runtime_stop_deferred;
                }
                Err(
                    OrchestratorError::ContainerNotFound { .. }
                    | OrchestratorError::InvalidContainerState { .. },
                ) => {}
                Err(OrchestratorError::JobFinalizing { .. }) => {
                    return Err(JobCancelError::Finalizing { job_id });
                }
                Err(OrchestratorError::Docker { reason }) => {
                    // stop_job claims the terminal handle and revokes all job
                    // credentials before obtaining the Docker client. The
                    // runtime stop is therefore deferred, not undelivered.
                    tracing::warn!(
                        job_id = %job_id,
                        %reason,
                        "Container stop deferred after its terminal state was claimed"
                    );
                    outcome.delivered = true;
                    outcome.runtime_stop_deferred = true;
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

        if let Some(message) = scheduler_error {
            if outcome.delivered {
                tracing::warn!(
                    job_id = %job_id,
                    %message,
                    "Scheduler stop failed after another runtime accepted cancellation"
                );
            } else {
                return Err(JobCancelError::Scheduler { job_id, message });
            }
        }
        Ok(outcome)
    }

    async fn sync_context(&self, job_id: Uuid) -> ContextSync {
        let Some(manager) = &self.context_manager else {
            return ContextSync::Unavailable("job context manager is not available".to_string());
        };
        let result = manager
            .update_context(job_id, |context| {
                let state = context.state;
                if state == JobState::Cancelled {
                    Ok(())
                } else if state.can_transition_to(JobState::Cancelled) {
                    context
                        .transition_to(JobState::Cancelled, Some(USER_CANCEL_REASON.to_string()))
                        .map_err(|_| state)
                } else {
                    Err(state)
                }
            })
            .await;
        match result {
            Ok(Ok(())) => ContextSync::Synced,
            Ok(Err(state)) => ContextSync::Conflict(state),
            Err(error) => ContextSync::Unavailable(error.to_string()),
        }
    }

    async fn persist_terminal_status(&self, job_id: Uuid, report: &mut CancelReport) {
        let Some(store) = &self.store else {
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
            .unwrap() // safety: test-only
            .unwrap(); // safety: test-only
        job_id
    }

    #[tokio::test]
    async fn context_only_cancel_synchronizes_when_no_runtime_is_configured() {
        let manager = Arc::new(ContextManager::new(5));
        let job_id = in_progress_job(&manager).await;

        let report = JobCanceller::new(Some(Arc::clone(&manager)))
            .cancel(job_id, CancelPolicy::AllowContextOnly)
            .await
            .unwrap(); // safety: test-only

        assert!(!report.delivered);
        assert!(report.persisted);
        assert!(report.context_synced);
        assert_eq!(
            manager.get_context(job_id).await.unwrap().state, // safety: test-only
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
            .unwrap_err(); // safety: test-only

        assert!(matches!(error, JobCancelError::NoActiveRuntime { .. }));
        assert_eq!(
            manager.get_context(job_id).await.unwrap().state, // safety: test-only
            JobState::InProgress
        );
    }

    #[tokio::test]
    async fn terminal_context_conflict_preserves_the_completion_state() {
        let manager = Arc::new(ContextManager::new(5));
        let job_id = in_progress_job(&manager).await;
        manager
            .update_context(job_id, |ctx| ctx.transition_to(JobState::Completed, None))
            .await
            .unwrap() // safety: test-only
            .unwrap(); // safety: test-only

        let error = JobCanceller::new(Some(Arc::clone(&manager)))
            .cancel(job_id, CancelPolicy::AllowContextOnly)
            .await
            .unwrap_err(); // safety: test-only

        assert!(matches!(
            error,
            JobCancelError::ContextConflict {
                state: JobState::Completed,
                ..
            }
        ));
        assert_eq!(
            manager.get_context(job_id).await.unwrap().state, // safety: test-only
            JobState::Completed
        );
    }

    #[tokio::test]
    async fn cancelled_context_allows_a_persistence_retry_without_runtime() {
        let manager = Arc::new(ContextManager::new(5));
        let job_id = in_progress_job(&manager).await;
        manager
            .update_context(job_id, |ctx| ctx.transition_to(JobState::Cancelled, None))
            .await
            .unwrap() // safety: test-only
            .unwrap(); // safety: test-only

        let report = JobCanceller::new(Some(Arc::clone(&manager)))
            .cancel(job_id, CancelPolicy::RequireRuntime)
            .await
            .unwrap(); // safety: test-only

        assert!(!report.delivered);
        assert!(report.persisted);
        assert!(report.context_synced);
    }
}
