//! Cron worker mode for external workers (WI#325).
//!
//! A cron-mode external worker hosts a **mission-like, long-running supervised
//! job**: instead of the in-process mission scheduler, a host-side supervisor
//! dispatches a structured `cron.tick` job to the worker on a cron schedule,
//! watches the tick job for heartbeat liveness, and re-dispatches a fresh
//! bounded job when the tick job dies or wedges. Mission lifetime is the
//! chain of jobs plus the worker's durable `/workspace` state, never any one
//! process.
//!
//! Design notes:
//!
//! - **Infinite deadline participation** — the supervisor (host) always holds
//!   the mission lease. Tick jobs are *not* subject to the standard bounded
//!   `timeout_ms` worker deadline; they run under the cron block's own
//!   `tick_timeout_ms` (defaulting to `timeout_ms`) plus the heartbeat
//!   watchdog. The mission itself has no deadline: it fires again at the next
//!   scheduled tick regardless of how any one tick resolved.
//! - **Not the mission lifecycle machine** — a cron-mode mission is never an
//!   engine `Mission`, so the WI#311 (`Completed` parking) and WI#317
//!   (unlocked boot-time snapshot writes) failure modes cannot apply to it.
//! - **Observability** — every tick dispatch, re-entry, and tick end is a
//!   persisted `job_events` record (structured `cron_tick` / `cron_reentry` /
//!   `cron_tick_end` events) plus an SSE `JobStatus` event, answering the
//!   "silent scheduler" gap from WI#323. The full session log for a mission
//!   is retrievable after the fact by reading the per-tick jobs' events.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::channels::web::types::SseEvent;
use crate::config::{CronWorkerConfig, ExternalWorkerConfig};
use crate::context::JobState;
use crate::db::Database;
use crate::error::OrchestratorError;
use crate::history::SandboxJobRecord;
use crate::orchestrator::external_worker::{CronTick, ExternalWorkerManager, build_task_context};

/// How often the tick watcher polls the job context for state/heartbeat.
/// Deliberately small relative to the default 15-minute heartbeat window so
/// a lapse is caught within seconds of crossing the threshold.
const WATCH_POLL: Duration = Duration::from_secs(5);

/// Outcome of watching one tick job to resolution (or to a heartbeat lapse).
#[derive(Debug, Clone, PartialEq, Eq)]
enum TickOutcome {
    /// The worker reported success; the tick is done.
    Completed,
    /// The job failed or was flagged stuck (worker error or tick deadline).
    Failed(String),
    /// No genuine progress within the heartbeat window — the job is wedged
    /// and must be cancelled and re-dispatched.
    HeartbeatLapse,
    /// The job was cancelled externally (e.g. `cancel_job` by the user).
    Cancelled,
    /// The job context vanished (e.g. daemon state cleanup); treat as failed.
    Missing,
}

impl TickOutcome {
    /// Whether the supervisor may re-dispatch a fresh job for this tick.
    fn is_retryable(&self) -> bool {
        matches!(self, Self::Failed(_) | Self::HeartbeatLapse | Self::Missing)
    }
}

impl ExternalWorkerManager {
    /// Spawn one mission supervisor per cron-mode worker (WI#325).
    ///
    /// Workers in the default task mode are untouched. A cron-mode worker
    /// with a missing or invalid `cron` block logs an error and is skipped —
    /// it stays usable for ordinary bounded dispatches. Each supervisor owns
    /// a serialized tick loop: the next tick is only scheduled after the
    /// current tick chain resolves, so a mission never overlaps itself.
    pub fn spawn_cron_supervisors(
        self: &Arc<Self>,
        owner_id: &str,
        shutdown_rx: broadcast::Receiver<()>,
    ) {
        for worker in self.cron_worker_configs() {
            let Some(cron) = worker.cron.clone() else {
                tracing::error!(
                    worker = %worker.name,
                    "External worker is mode=cron but has no [cron] block; \
                     mission supervisor disabled for this worker"
                );
                continue;
            };
            if let Err(reason) = validate_cron_schedule(&cron.schedule) {
                tracing::error!(
                    worker = %worker.name,
                    schedule = %cron.schedule,
                    "Invalid cron schedule for cron-mode worker; mission supervisor \
                     disabled for this worker: {reason}"
                );
                continue;
            }
            if self.context_manager_handle().is_none() {
                tracing::warn!(
                    worker = %worker.name,
                    "Cron-mode worker has no context manager; mission supervisor disabled"
                );
                continue;
            }

            let mission_id = cron.resolved_mission_id(&worker.name);
            tracing::info!(
                worker = %worker.name,
                %mission_id,
                schedule = %cron.schedule,
                "Cron mission supervisor starting"
            );

            let manager = Arc::clone(self);
            let owner = owner_id.to_string();
            let mission_shutdown = shutdown_rx.resubscribe();
            tokio::spawn(async move {
                run_cron_mission(manager, worker, cron, mission_id, owner, mission_shutdown).await;
            });
        }
    }

    /// Fire one cron tick immediately for a cron-mode worker (WI#325).
    ///
    /// Runs the same dispatch → watch → re-entry chain the scheduled loop
    /// uses, with `tick_index` 0 marking it as an operator/test trigger. The
    /// mission's durable state handoff still goes through the worker's
    /// `/workspace` mount, so a manual tick composes with scheduled ticks.
    pub async fn fire_cron_tick_now(
        self: &Arc<Self>,
        worker_name: &str,
        owner_id: &str,
    ) -> Result<(), OrchestratorError> {
        let worker = self
            .get_worker(worker_name)
            .ok_or_else(|| OrchestratorError::ExternalWorkerNotFound {
                worker_name: worker_name.to_string(),
            })?
            .clone();
        if !worker.is_cron() {
            return Err(OrchestratorError::ExternalWorkerNotCron {
                worker_name: worker_name.to_string(),
            });
        }
        let cron = worker
            .cron
            .clone()
            .ok_or_else(|| OrchestratorError::ExternalWorkerNotCron {
                worker_name: worker_name.to_string(),
            })?;
        let mission_id = cron.resolved_mission_id(&worker.name);
        run_tick(self, &worker, &cron, &mission_id, 0, owner_id).await;
        Ok(())
    }
}

/// Validate that a cron expression parses under the same normalization the
/// routine engine applies (5/6/7-field expressions).
fn validate_cron_schedule(schedule: &str) -> Result<(), String> {
    crate::agent::routine::next_cron_fire(schedule, None)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// The per-mission loop: wait for each scheduled fire, then run the tick's
/// job chain to resolution before computing the next fire.
async fn run_cron_mission(
    manager: Arc<ExternalWorkerManager>,
    worker: ExternalWorkerConfig,
    cron: CronWorkerConfig,
    mission_id: String,
    owner_id: String,
    mut shutdown_rx: broadcast::Receiver<()>,
) {
    let mut tick_index: u64 = 0;
    loop {
        let Some(fire_at) = next_fire_or_log(&worker.name, &cron.schedule) else {
            return;
        };
        let wait = (fire_at - Utc::now()).to_std().unwrap_or_default();
        tracing::info!(
            worker = %worker.name,
            %mission_id,
            next_fire_at = %fire_at.to_rfc3339(),
            "Cron mission awaiting next tick"
        );
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = shutdown_rx.recv() => {
                tracing::info!(
                    worker = %worker.name,
                    %mission_id,
                    "Cron mission supervisor shutting down"
                );
                return;
            }
        }

        tick_index += 1;
        run_tick(&manager, &worker, &cron, &mission_id, tick_index, &owner_id).await;
    }
}

/// Compute the next fire time, logging and ending the mission loop when the
/// schedule stops yielding one (a previously-validated schedule should not
/// do this; treat as a hard config error if it does).
fn next_fire_or_log(worker_name: &str, schedule: &str) -> Option<chrono::DateTime<Utc>> {
    match crate::agent::routine::next_cron_fire(schedule, None) {
        Ok(Some(fire_at)) => Some(fire_at),
        Ok(None) => {
            tracing::error!(
                worker = %worker_name,
                %schedule,
                "Cron schedule yields no future fire times; mission supervisor stopping"
            );
            None
        }
        Err(error) => {
            tracing::error!(
                worker = %worker_name,
                %schedule,
                "Cron schedule failed to parse at fire time; mission supervisor \
                 stopping: {error}"
            );
            None
        }
    }
}

/// Run one tick as a chain of bounded jobs: dispatch, watch, and re-dispatch
/// on wedge/failure up to `max_reentries` times. Every chain link is a fresh
/// job id so `job_events` shows each attempt; the mission id links the chain.
async fn run_tick(
    manager: &Arc<ExternalWorkerManager>,
    worker: &ExternalWorkerConfig,
    cron: &CronWorkerConfig,
    mission_id: &str,
    tick_index: u64,
    owner_id: &str,
) {
    let max_reentries = cron.max_reentries();
    let mut chain_index: u32 = 0;
    loop {
        let job_id = Uuid::new_v4();
        dispatch_tick_job(
            manager,
            worker,
            cron,
            mission_id,
            tick_index,
            chain_index,
            job_id,
            owner_id,
        )
        .await;

        let outcome = watch_tick_job(manager, cron, job_id).await;
        match &outcome {
            TickOutcome::Completed | TickOutcome::Cancelled => {
                let label = describe_outcome(&outcome);
                tracing::info!(
                    worker = %worker.name,
                    %mission_id,
                    tick_index,
                    chain_index,
                    %job_id,
                    outcome = %label,
                    "Cron tick resolved"
                );
                persist_cron_event(
                    manager.store_handle(),
                    job_id,
                    "cron_tick_end",
                    serde_json::json!({
                        "mission_id": mission_id,
                        "tick_index": tick_index,
                        "chain_index": chain_index,
                        "outcome": label,
                    }),
                );
                return;
            }
            retryable if retryable.is_retryable() && chain_index < max_reentries => {
                let reason = describe_outcome(retryable);
                tracing::warn!(
                    worker = %worker.name,
                    %mission_id,
                    tick_index,
                    chain_index,
                    %job_id,
                    %reason,
                    "Cron tick job unresolved; re-dispatching a fresh bounded job"
                );
                if matches!(retryable, TickOutcome::HeartbeatLapse) {
                    manager.cancel_task(job_id).await;
                }
                persist_cron_event(
                    manager.store_handle(),
                    job_id,
                    "cron_reentry",
                    serde_json::json!({
                        "mission_id": mission_id,
                        "tick_index": tick_index,
                        "chain_index": chain_index,
                        "reason": reason,
                    }),
                );
                chain_index += 1;
            }
            terminal => {
                let reason = describe_outcome(terminal);
                tracing::error!(
                    worker = %worker.name,
                    %mission_id,
                    tick_index,
                    chain_index,
                    %job_id,
                    %reason,
                    "Cron tick abandoned after re-entry budget; mission continues at \
                     next scheduled fire"
                );
                persist_cron_event(
                    manager.store_handle(),
                    job_id,
                    "cron_tick_end",
                    serde_json::json!({
                        "mission_id": mission_id,
                        "tick_index": tick_index,
                        "chain_index": chain_index,
                        "outcome": "abandoned",
                        "reason": reason,
                    }),
                );
                return;
            }
        }
    }
}

fn describe_outcome(outcome: &TickOutcome) -> String {
    match outcome {
        TickOutcome::Completed => "completed".to_string(),
        TickOutcome::Failed(reason) => format!("failed: {reason}"),
        TickOutcome::HeartbeatLapse => "heartbeat lapse".to_string(),
        TickOutcome::Cancelled => "cancelled".to_string(),
        TickOutcome::Missing => "job context missing".to_string(),
    }
}

/// Dispatch one bounded tick job: reserve, register, persist, then fire the
/// task on the worker with the structured `cron.tick` context attached.
#[allow(clippy::too_many_arguments)]
async fn dispatch_tick_job(
    manager: &Arc<ExternalWorkerManager>,
    worker: &ExternalWorkerConfig,
    cron: &CronWorkerConfig,
    mission_id: &str,
    tick_index: u64,
    chain_index: u32,
    job_id: Uuid,
    owner_id: &str,
) {
    if let Err(error) = manager.reserve_task(job_id, &worker.name).await {
        tracing::warn!(%job_id, worker = %worker.name, "Cron tick reservation failed: {error}");
        return;
    }

    let title = format!("{mission_id} tick #{tick_index}");
    let description = build_tick_prompt(cron, mission_id, tick_index, chain_index);
    if let Some(cm) = manager.context_manager_handle()
        && let Err(error) = cm
            .register_sandbox_job(job_id, owner_id, &title, &description)
            .await
    {
        tracing::warn!(%job_id, "Failed to register cron tick job: {error}");
        manager.release_task_reservation(job_id).await;
        return;
    }
    if let Some(cm) = manager.context_manager_handle() {
        let mission = mission_id.to_string();
        let schedule = cron.schedule.clone();
        let _ = cm
            .update_context(job_id, |ctx| {
                ctx.metadata = serde_json::json!({
                    "mission_id": mission,
                    "cron_schedule": schedule,
                    "tick_index": tick_index,
                    "chain_index": chain_index,
                    "cron_mode": true,
                });
            })
            .await;
    }

    persist_tick_record(manager, worker, job_id, owner_id, &description).await;

    let fired_at = Utc::now().to_rfc3339();
    let mut context = build_task_context(
        owner_id,
        None,
        HashMap::new(),
        Vec::new(),
        HashMap::from([
            ("mission_id".to_string(), mission_id.to_string()),
            ("cron_mode".to_string(), "true".to_string()),
        ]),
    );
    context.cron_tick = Some(CronTick {
        event: "cron.tick".to_string(),
        mission_id: mission_id.to_string(),
        schedule: cron.schedule.clone(),
        goal: cron.goal.clone(),
        tick_index,
        chain_index,
        fired_at: fired_at.clone(),
    });

    let tick_timeout_ms = cron.tick_timeout_ms(worker.timeout_ms);
    persist_cron_event(
        manager.store_handle(),
        job_id,
        "cron_tick",
        serde_json::json!({
            "mission_id": mission_id,
            "schedule": cron.schedule,
            "goal": cron.goal,
            "tick_index": tick_index,
            "chain_index": chain_index,
            "fired_at": fired_at,
            "tick_timeout_ms": tick_timeout_ms,
        }),
    );
    emit_cron_status(
        manager,
        job_id,
        owner_id,
        format!("cron tick #{tick_index} (mission {mission_id}) dispatched"),
    );

    if let Err(error) = manager
        .execute_task(
            job_id,
            &worker.name,
            &description,
            Some(tick_timeout_ms),
            false,
            context,
        )
        .await
    {
        // Dispatch itself failed (e.g. worker unreachable at connect time on
        // every endpoint): mark the job failed so the watcher resolves
        // immediately instead of waiting out the heartbeat window; the chain
        // logic then re-dispatches.
        tracing::warn!(%job_id, worker = %worker.name, "Cron tick dispatch failed: {error}");
        if let Some(cm) = manager.context_manager_handle() {
            let reason = error.to_string();
            let _ = cm
                .update_context(job_id, |ctx| {
                    if matches!(ctx.state, JobState::Pending | JobState::InProgress) {
                        let _ = ctx.transition_to(JobState::Failed, Some(reason.clone()));
                    }
                })
                .await;
        }
    }
}

/// Persist the sandbox-job row for a tick job so it survives restarts and is
/// visible to `list_jobs`/`job_status` DB fallbacks.
async fn persist_tick_record(
    manager: &ExternalWorkerManager,
    worker: &ExternalWorkerConfig,
    job_id: Uuid,
    owner_id: &str,
    description: &str,
) {
    let Some(store) = manager.store_handle() else {
        return;
    };
    let now = Utc::now();
    let record = SandboxJobRecord {
        id: job_id,
        task: description.to_string(),
        status: "running".to_string(),
        user_id: owner_id.to_string(),
        project_dir: String::new(),
        success: None,
        failure_reason: None,
        created_at: now,
        started_at: Some(now),
        completed_at: None,
        credential_grants_json: "[]".to_string(),
    };
    if let Err(error) = store.save_sandbox_job(&record).await {
        tracing::warn!(%job_id, "Failed to persist cron tick job record: {error}");
    }
    let mode = crate::orchestrator::job_manager::JobMode::External(worker.name.clone()).db_value();
    if let Err(error) = store.update_sandbox_job_mode(job_id, &mode).await {
        tracing::warn!(%job_id, "Failed to persist cron tick job mode: {error}");
    }
}

/// Watch one tick job: poll the context until the job resolves or the
/// heartbeat window lapses (genuine progress refreshes `last_activity_at`,
/// synthetic `[worker] ` heartbeats do not — see WI#263).
async fn watch_tick_job(
    manager: &ExternalWorkerManager,
    cron: &CronWorkerConfig,
    job_id: Uuid,
) -> TickOutcome {
    let Some(cm) = manager.context_manager_handle() else {
        return TickOutcome::Missing;
    };
    let heartbeat_timeout = cron.heartbeat_timeout();
    // Poll no coarser than half the heartbeat window (floor 200ms) so short
    // watchdog configs are still enforced promptly.
    let poll = WATCH_POLL
        .min(heartbeat_timeout / 2)
        .max(Duration::from_millis(200));
    loop {
        tokio::time::sleep(poll).await;
        let ctx = match cm.get_context(job_id).await {
            Ok(ctx) => ctx,
            Err(_) => return TickOutcome::Missing,
        };
        match ctx.state {
            JobState::Completed | JobState::Accepted => return TickOutcome::Completed,
            JobState::Failed => {
                return TickOutcome::Failed(format!("job entered {} state", ctx.state));
            }
            // Stuck means the idle detector already flagged this job as
            // wedged; route it through the lapse path so the supervisor
            // cancels before re-dispatching (a stuck job may otherwise
            // recover later — WI#263 Stuck→Completed — and double-run the
            // tick alongside its replacement).
            JobState::Stuck => return TickOutcome::HeartbeatLapse,
            JobState::Cancelled => return TickOutcome::Cancelled,
            JobState::Pending | JobState::InProgress | JobState::Submitted => {}
        }
        let baseline = ctx
            .last_activity_at
            .or(ctx.started_at)
            .unwrap_or(ctx.created_at);
        if Utc::now()
            .signed_duration_since(baseline)
            .to_std()
            .unwrap_or_default()
            > heartbeat_timeout
        {
            return TickOutcome::HeartbeatLapse;
        }
    }
}

/// Persist a structured supervisor event to `job_events` (WI#325 logging).
fn persist_cron_event(
    store: Option<&Arc<dyn Database>>,
    job_id: Uuid,
    event_type: &str,
    value: serde_json::Value,
) {
    let Some(db) = store.cloned() else {
        return;
    };
    let event_type = event_type.to_string();
    tokio::spawn(async move {
        if let Err(error) = db.save_job_event(job_id, &event_type, &value).await {
            tracing::warn!(%job_id, %event_type, "Failed to persist cron supervisor event: {error}");
        }
    });
}

/// Emit an SSE job-status frame so live subscribers see tick transitions.
fn emit_cron_status(manager: &ExternalWorkerManager, job_id: Uuid, user_id: &str, message: String) {
    if let Some(tx) = manager.event_tx_handle() {
        let _ = tx.send((
            job_id,
            user_id.to_string(),
            SseEvent::JobStatus {
                job_id: job_id.to_string(),
                message,
            },
        ));
    }
}

/// Build the tick job prompt: the mission goal plus the chain-handoff
/// contract the worker follows via its durable `/workspace` mount.
fn build_tick_prompt(
    cron: &CronWorkerConfig,
    mission_id: &str,
    tick_index: u64,
    chain_index: u32,
) -> String {
    format!(
        "You are the resident worker for mission '{mission_id}', firing on cron schedule \
         '{schedule}'.\n\nMISSION GOAL:\n{goal}\n\nThis is tick #{tick_index} (attempt \
         {attempt}). Mission state is durable in your /workspace directory: read prior state \
         at the start of the tick and write updated state before you finish. The host \
         supervisor holds the mission lease — if this job dies or goes silent, a fresh job is \
         dispatched and resumes from /workspace. Work the mission goal for this tick, then \
         report what changed.",
        schedule = cron.schedule,
        goal = cron.goal,
        attempt = chain_index + 1,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cron() -> CronWorkerConfig {
        CronWorkerConfig {
            schedule: "0 15 10 * * *".to_string(),
            goal: "Produce the daily PR digest".to_string(),
            mission_id: Some("pr-digest".to_string()),
            heartbeat_timeout_secs: Some(300),
            tick_timeout_ms: Some(1_800_000),
            max_reentries: Some(2),
        }
    }

    #[test]
    fn valid_schedules_pass_validation() {
        // 5-field, 6-field, and 7-field forms all normalize.
        assert!(validate_cron_schedule("0 15 10 * * *").is_ok());
        assert!(validate_cron_schedule("*/5 * * * *").is_ok());
        assert!(validate_cron_schedule("0 0 15 10 * *").is_ok());
        assert!(validate_cron_schedule("0 0 15 10 * * *").is_ok());
    }

    #[test]
    fn invalid_schedules_fail_validation() {
        assert!(validate_cron_schedule("not a schedule").is_err());
        assert!(validate_cron_schedule("61 * * * *").is_err());
        assert!(validate_cron_schedule("").is_err());
    }

    #[test]
    fn tick_prompt_carries_mission_context() {
        let cron = test_cron();
        let prompt = build_tick_prompt(&cron, "pr-digest", 7, 1);
        assert!(prompt.contains("pr-digest"));
        assert!(prompt.contains("Produce the daily PR digest"));
        assert!(prompt.contains("tick #7"));
        assert!(prompt.contains("attempt 2"));
        assert!(prompt.contains("/workspace"));
    }

    #[test]
    fn tick_outcome_retry_rules() {
        assert!(TickOutcome::Failed("x".to_string()).is_retryable());
        assert!(TickOutcome::HeartbeatLapse.is_retryable());
        assert!(TickOutcome::Missing.is_retryable());
        assert!(!TickOutcome::Completed.is_retryable());
        assert!(!TickOutcome::Cancelled.is_retryable());
    }

    #[test]
    fn cron_tick_event_serializes_all_fields() {
        let tick = CronTick {
            event: "cron.tick".to_string(),
            mission_id: "pr-digest".to_string(),
            schedule: "0 15 10 * * *".to_string(),
            goal: "digest".to_string(),
            tick_index: 3,
            chain_index: 1,
            fired_at: "2026-10-10T10:15:00Z".to_string(),
        };
        let value = serde_json::to_value(&tick).expect("serialize");
        assert_eq!(value["event"], "cron.tick");
        assert_eq!(value["mission_id"], "pr-digest");
        assert_eq!(value["tick_index"], 3);
        assert_eq!(value["chain_index"], 1);
    }

    /// Ordinary task dispatches must not carry a `cron_tick` field on the
    /// wire at all (workers that reject unknown context keys stay compatible).
    #[test]
    fn cron_tick_absent_from_plain_task_context() {
        let ctx = build_task_context("u", None, HashMap::new(), Vec::new(), HashMap::new());
        let json = serde_json::to_value(&ctx).expect("serialize");
        assert!(json.get("cron_tick").is_none());
    }
}
