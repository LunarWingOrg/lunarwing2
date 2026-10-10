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
//!   `tick_timeout_ms` (validated against the worker bridge cap at load) plus
//!   the heartbeat watchdog. The mission itself has no deadline: it fires
//!   again at the next scheduled tick regardless of how any one tick resolved.
//! - **Not the mission lifecycle machine** — a cron-mode mission is never an
//!   engine `Mission`, so the WI#311 (`Completed` parking) and WI#317
//!   (unlocked boot-time snapshot writes) failure modes cannot apply to it.
//! - **Runaway guards** — config resolution enforces a schedule floor, a
//!   heartbeat floor, a re-entry cap, and a tick-timeout bound; re-dispatch
//!   backs off exponentially; a circuit breaker pauses the mission after too
//!   many consecutive abandoned ticks; the owner can pause/resume at runtime
//!   (persisted, restart-safe); terminal tick contexts are evicted so the
//!   in-memory context map does not grow without bound.
//! - **Workspace-lock safety** — after a heartbeat lapse the supervisor
//!   cancels the wedged job and waits (bounded) for it to reach a terminal
//!   state before re-dispatching, so the fresh link does not race the old
//!   runner's workspace lock. A worker rejection reporting the workspace as
//!   busy retries with backoff without consuming the re-entry budget.
//! - **Observability** — every tick dispatch, re-entry, circuit-breaker trip,
//!   and tick end is a persisted `job_events` record (structured `cron_tick` /
//!   `cron_reentry` / `cron_paused` / `cron_tick_end` events, written inline
//!   so rows are never lost to detached-spawn teardown) plus SSE `JobStatus`
//!   frames, answering the "silent scheduler" gap from WI#323. The full
//!   session log for a mission is retrievable after the fact by reading the
//!   per-tick jobs' events.
//! - **Durable tick counter** — the tick index is persisted per mission in
//!   the DB-backed settings store, so it does not restart at 1 on every boot.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

use crate::channels::web::types::SseEvent;
use crate::config::{CronWorkerConfig, ExternalWorkerConfig};
use crate::context::JobState;
use crate::db::Database;
use crate::error::OrchestratorError;
use crate::history::SandboxJobRecord;
use crate::orchestrator::external_worker::{
    CancelReason, CronCommand, CronMissionControl, CronMissionStatus, CronTick,
    ExternalWorkerManager, build_task_context,
};

/// How often the tick watcher polls the job context for state/heartbeat.
/// Deliberately small relative to the default 15-minute heartbeat window so
/// a lapse is caught within seconds of crossing the threshold.
const WATCH_POLL: Duration = Duration::from_secs(5);

/// Poll cadence while waiting for a cancelled tick job to go terminal.
const TERMINAL_POLL: Duration = Duration::from_millis(250);

/// Bounded wait for a cancelled tick job to reach a terminal state before
/// re-dispatching. Covers the cancel handshake (send ≤ 2s + ack ≤ 3s) plus
/// the worker's own kill grace (kimi SIGTERM→SIGKILL is 7s by default) and
/// bookkeeping slack.
const TERMINAL_WAIT: Duration = Duration::from_secs(20);

/// Base for the exponential backoff between chain links.
const REENTRY_BACKOFF_BASE: Duration = Duration::from_secs(2);

/// Cap for the exponential backoff between chain links.
const REENTRY_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Workspace-busy rejections retry the same link without consuming the
/// re-entry budget, bounded so a permanently busy workspace still abandons.
const MAX_BUSY_RETRIES: u32 = 5;

/// Consecutive abandoned ticks before the circuit breaker pauses the mission.
const CIRCUIT_BREAKER_THRESHOLD: u32 = 3;

/// Worker-reported markers that a dispatch rejection is a workspace-busy
/// collision (kimi, pebble, and opencode bridges share this wording).
const BUSY_MARKER: &str = "already being modified";

/// Worker-reported markers for protocol/config rejections that retrying can
/// never fix (bad timeout, unresolvable project_dir, empty prompt).
const NON_RETRYABLE_MARKERS: &[&str] = &[
    "timeout_ms must be",
    "could not be resolved under WORKSPACE_ROOT",
    "Invalid workspace path",
    "Missing or invalid prompt",
];

/// Outcome of watching one tick job to resolution (or to a heartbeat lapse).
#[derive(Debug, Clone, PartialEq, Eq)]
enum TickOutcome {
    /// The worker reported success; the tick is done.
    Completed,
    /// The job failed (worker error or tick deadline).
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

/// How a resolved tick feeds back into the mission loop's circuit breaker.
#[derive(Debug)]
enum TickEnd {
    /// The tick resolved (completed, or cancelled by the owner).
    Resolved,
    /// The tick exhausted its re-entry budget or hit a non-retryable error.
    Abandoned { job_id: Uuid, reason: String },
}

/// Why the mission supervisor loop exited.
#[derive(Debug, PartialEq, Eq)]
enum MissionExit {
    /// Daemon shutdown was signalled.
    Shutdown,
    /// The schedule stopped yielding fire times (hard config error).
    ScheduleExhausted,
}

/// Per-mission supervisor state: the durable tick counter, the circuit
/// breaker, and the persisted pause flag.
#[derive(Debug, Default)]
struct MissionState {
    tick_index: u64,
    consecutive_abandoned: u32,
    paused: bool,
}

/// Bundles the invariant parameters of one tick so dispatch/watch helpers
/// stay within the argument-count limit.
struct TickCtx<'a> {
    manager: &'a Arc<ExternalWorkerManager>,
    worker: &'a ExternalWorkerConfig,
    cron: &'a CronWorkerConfig,
    mission_id: &'a str,
    owner_id: &'a str,
}

impl ExternalWorkerManager {
    /// Spawn one mission supervisor per cron-mode worker (WI#325).
    ///
    /// Workers in the default task mode are untouched. A cron-mode worker
    /// whose `cron` block is missing or has an unparseable schedule logs an
    /// error and is skipped (config resolution already rejects invalid cron
    /// configs, so this is defense in depth). Each supervisor owns a
    /// serialized tick loop — scheduled fires and manual triggers share one
    /// mpsc command channel, so a mission never overlaps itself — and panics
    /// are reported loudly instead of silently killing the mission.
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

            let (cmd_tx, cmd_rx) = mpsc::channel(16);
            let manager = Arc::clone(self);
            let owner = owner_id.to_string();
            let mission_shutdown = shutdown_rx.resubscribe();
            let worker_name = worker.name.clone();
            let mission = mission_id.clone();
            let registered_name = worker.name.clone();
            let mission_task = tokio::spawn(async move {
                run_cron_mission(
                    manager,
                    worker,
                    cron,
                    mission_id,
                    owner,
                    mission_shutdown,
                    cmd_rx,
                )
                .await
            });
            // Report supervisor death loudly (WI#323 class): a panic in the
            // mission loop must not silently strand the mission.
            tokio::spawn(async move {
                match mission_task.await {
                    Ok(MissionExit::Shutdown) => {
                        tracing::info!(worker = %worker_name, mission = %mission,
                            "Cron mission supervisor shut down cleanly");
                    }
                    Ok(MissionExit::ScheduleExhausted) => {
                        tracing::error!(worker = %worker_name, mission = %mission,
                            "Cron mission supervisor stopped: schedule yields no fire times");
                    }
                    Err(join_error) => {
                        tracing::error!(worker = %worker_name, mission = %mission,
                            %join_error,
                            "Cron mission supervisor panicked; mission is down until restart");
                    }
                }
            });

            let control = CronMissionControl {
                owner_id: owner_id.to_string(),
                tx: cmd_tx,
            };
            self.cron_controls
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(registered_name, control);
        }
    }

    /// Fire one cron tick immediately for a cron-mode worker (WI#325).
    ///
    /// The fire is routed through the mission supervisor's command channel,
    /// so a manual tick is serialized with scheduled ticks and can never
    /// overlap one on the same worker workspace. `tick_index` 0 marks the
    /// run as an operator/test trigger. The mission's durable state handoff
    /// still goes through the worker's `/workspace` mount.
    pub async fn fire_cron_tick_now(
        &self,
        worker_name: &str,
        owner_id: &str,
    ) -> Result<(), OrchestratorError> {
        self.send_cron_command(worker_name, owner_id, CronCommand::FireNow)
            .await
    }

    /// Pause a cron mission at runtime (WI#325). Persisted: a paused mission
    /// stays paused across daemon restarts until resumed.
    pub async fn pause_cron_mission(
        &self,
        worker_name: &str,
        owner_id: &str,
    ) -> Result<(), OrchestratorError> {
        self.send_cron_command(worker_name, owner_id, CronCommand::Pause)
            .await
    }

    /// Resume a paused cron mission and reset its circuit breaker.
    pub async fn resume_cron_mission(
        &self,
        worker_name: &str,
        owner_id: &str,
    ) -> Result<(), OrchestratorError> {
        self.send_cron_command(worker_name, owner_id, CronCommand::Resume)
            .await
    }

    /// Live + persisted status of a cron mission, owner-gated.
    pub async fn cron_mission_status(
        &self,
        worker_name: &str,
        owner_id: &str,
    ) -> Result<CronMissionStatus, OrchestratorError> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.send_cron_command(worker_name, owner_id, CronCommand::Status(reply_tx))
            .await?;
        reply_rx.await.map_err(|_| OrchestratorError::CronSupervisorNotRunning {
            worker_name: worker_name.to_string(),
        })
    }

    /// Owner-gated command dispatch to a running mission supervisor. The
    /// gate mirrors `cancel_job`: only the mission's owning user may drive it.
    async fn send_cron_command(
        &self,
        worker_name: &str,
        owner_id: &str,
        command: CronCommand,
    ) -> Result<(), OrchestratorError> {
        let tx = {
            let controls = self
                .cron_controls
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match controls.get(worker_name) {
                Some(control) if control.owner_id == owner_id => control.tx.clone(),
                Some(_) => {
                    return Err(OrchestratorError::CronMissionNotOwned {
                        worker_name: worker_name.to_string(),
                    });
                }
                None if self.get_worker(worker_name).is_some_and(|w| w.is_cron()) => {
                    return Err(OrchestratorError::CronSupervisorNotRunning {
                        worker_name: worker_name.to_string(),
                    });
                }
                None => {
                    return Err(OrchestratorError::ExternalWorkerNotCron {
                        worker_name: worker_name.to_string(),
                    });
                }
            }
        };
        tx.send(command)
            .await
            .map_err(|_| OrchestratorError::CronSupervisorNotRunning {
                worker_name: worker_name.to_string(),
            })
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
/// job chain to resolution before computing the next fire. Commands from
/// `fire_cron_tick_now`/`pause_cron_mission`/`resume_cron_mission` are
/// serviced in the same select so everything stays serialized.
async fn run_cron_mission(
    manager: Arc<ExternalWorkerManager>,
    worker: ExternalWorkerConfig,
    cron: CronWorkerConfig,
    mission_id: String,
    owner_id: String,
    mut shutdown_rx: broadcast::Receiver<()>,
    mut cmd_rx: mpsc::Receiver<CronCommand>,
) -> MissionExit {
    let ctx = TickCtx {
        manager: &manager,
        worker: &worker,
        cron: &cron,
        mission_id: &mission_id,
        owner_id: &owner_id,
    };
    let mut state = load_mission_state(manager.store_handle(), &owner_id, &mission_id).await;
    if state.paused {
        tracing::info!(
            worker = %worker.name,
            %mission_id,
            "Cron mission starts paused (persisted owner pause)"
        );
    }
    loop {
        if state.paused {
            tokio::select! {
                command = cmd_rx.recv() => {
                    let Some(command) = command else {
                        return MissionExit::Shutdown;
                    };
                    if handle_command(&ctx, &mut state, command, &mut shutdown_rx).await
                        == CommandOutcome::Shutdown
                    {
                        return MissionExit::Shutdown;
                    }
                }
                _ = shutdown_rx.recv() => return MissionExit::Shutdown,
            }
            continue;
        }
        let Some(fire_at) = next_fire_or_log(&worker.name, &cron.schedule) else {
            return MissionExit::ScheduleExhausted;
        };
        let wait = (fire_at - Utc::now()).to_std().unwrap_or_default();
        tracing::info!(
            worker = %worker.name,
            %mission_id,
            next_fire_at = %fire_at.to_rfc3339(),
            "Cron mission awaiting next tick"
        );
        tokio::select! {
            _ = tokio::time::sleep(wait) => {
                state.tick_index += 1;
                persist_tick_counter(ctx.manager, &owner_id, &mission_id, state.tick_index).await;
                let tick_index = state.tick_index;
                match run_tick(&ctx, tick_index, &mut shutdown_rx).await {
                    Some(end) => after_tick(&ctx, &mut state, end).await,
                    None => return MissionExit::Shutdown,
                }
            }
            command = cmd_rx.recv() => {
                let Some(command) = command else {
                    return MissionExit::Shutdown;
                };
                if handle_command(&ctx, &mut state, command, &mut shutdown_rx).await
                    == CommandOutcome::Shutdown
                {
                    return MissionExit::Shutdown;
                }
            }
            _ = shutdown_rx.recv() => {
                tracing::info!(
                    worker = %worker.name,
                    %mission_id,
                    "Cron mission supervisor shutting down"
                );
                return MissionExit::Shutdown;
            }
        }
    }
}

/// Whether the mission loop should keep running after a command.
#[derive(Debug, PartialEq, Eq)]
enum CommandOutcome {
    Continue,
    Shutdown,
}

/// Handle one supervisor command. `FireNow` runs a manual tick (tick_index 0)
/// even while paused — an operator trigger is the point of the escape hatch.
async fn handle_command(
    ctx: &TickCtx<'_>,
    state: &mut MissionState,
    command: CronCommand,
    shutdown_rx: &mut broadcast::Receiver<()>,
) -> CommandOutcome {
    match command {
        CronCommand::FireNow => {
            tracing::info!(worker = %ctx.worker.name, mission_id = %ctx.mission_id, "Manual cron tick fired");
            match run_tick(ctx, 0, shutdown_rx).await {
                Some(end) => {
                    after_tick(ctx, state, end).await;
                    CommandOutcome::Continue
                }
                None => CommandOutcome::Shutdown,
            }
        }
        CronCommand::Pause => {
            state.paused = true;
            persist_paused(ctx.manager, ctx.owner_id, ctx.mission_id, true).await;
            tracing::info!(worker = %ctx.worker.name, mission_id = %ctx.mission_id, "Cron mission paused by owner");
            CommandOutcome::Continue
        }
        CronCommand::Resume => {
            state.paused = false;
            state.consecutive_abandoned = 0;
            persist_paused(ctx.manager, ctx.owner_id, ctx.mission_id, false).await;
            tracing::info!(worker = %ctx.worker.name, mission_id = %ctx.mission_id, "Cron mission resumed by owner");
            CommandOutcome::Continue
        }
        CronCommand::Status(reply) => {
            let _ = reply.send(CronMissionStatus {
                mission_id: ctx.mission_id.to_string(),
                worker_name: ctx.worker.name.clone(),
                paused: state.paused,
                tick_index: state.tick_index,
                consecutive_abandoned: state.consecutive_abandoned,
            });
            CommandOutcome::Continue
        }
    }
}

/// Update circuit-breaker state after a scheduled tick resolves; trips into
/// a persisted pause after `CIRCUIT_BREAKER_THRESHOLD` consecutive abandons.
async fn after_tick(ctx: &TickCtx<'_>, state: &mut MissionState, end: TickEnd) {
    match end {
        TickEnd::Resolved => state.consecutive_abandoned = 0,
        TickEnd::Abandoned { job_id, reason } => {
            state.consecutive_abandoned += 1;
            if state.consecutive_abandoned >= CIRCUIT_BREAKER_THRESHOLD {
                state.paused = true;
                persist_paused(ctx.manager, ctx.owner_id, ctx.mission_id, true).await;
                tracing::error!(
                    worker = %ctx.worker.name,
                    mission_id = %ctx.mission_id,
                    consecutive_abandoned = state.consecutive_abandoned,
                    "Cron mission circuit breaker tripped; mission paused until owner resumes"
                );
                persist_cron_event(
                    ctx.manager.store_handle(),
                    job_id,
                    "cron_paused",
                    serde_json::json!({
                        "mission_id": ctx.mission_id,
                        "reason": "circuit_breaker",
                        "consecutive_abandoned": state.consecutive_abandoned,
                        "last_abandon_reason": reason,
                        "schedule": ctx.cron.schedule,
                    }),
                )
                .await;
                emit_cron_status(
                    ctx.manager,
                    job_id,
                    ctx.owner_id,
                    format!(
                        "cron mission {} paused: circuit breaker after {} abandoned ticks",
                        ctx.mission_id, state.consecutive_abandoned
                    ),
                );
            }
        }
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
/// on wedge/failure up to `max_reentries` times, with exponential backoff
/// between links. Every chain link is a fresh job id so `job_events` shows
/// each attempt; the mission id links the chain. Returns `None` when daemon
/// shutdown interrupted the tick.
async fn run_tick(
    ctx: &TickCtx<'_>,
    tick_index: u64,
    shutdown_rx: &mut broadcast::Receiver<()>,
) -> Option<TickEnd> {
    let manager = ctx.manager;
    let cron = ctx.cron;
    let max_reentries = cron.max_reentries();
    let mut chain_index: u32 = 0;
    let mut busy_retries: u32 = 0;
    loop {
        let job_id = Uuid::new_v4();
        dispatch_tick_job(ctx, tick_index, chain_index, job_id).await;

        let Some(outcome) = watch_tick_job(ctx, job_id, shutdown_rx).await else {
            // Shutdown mid-tick: cancel the in-flight link with the shutdown
            // reason so it is not mistaken for an owner cancel.
            manager
                .cancel_task_with_reason(job_id, CancelReason::Shutdown)
                .await;
            return None;
        };

        if let Some(end) = resolve_terminal(ctx, tick_index, chain_index, job_id, &outcome).await
        {
            return Some(end);
        }
        if let Some(action) =
            handle_busy_rejection(ctx, tick_index, chain_index, job_id, &outcome, &mut busy_retries)
                .await
        {
            match action {
                BusyAction::Retry => {
                    if !backoff_sleep(busy_retries, shutdown_rx).await {
                        return None;
                    }
                    continue;
                }
                BusyAction::Abandon(end) => return Some(end),
            }
        }
        match prepare_reentry(ctx, tick_index, chain_index, job_id, &outcome, max_reentries).await {
            Reentry::Redispatch => {
                chain_index += 1;
                if !backoff_sleep(chain_index, shutdown_rx).await {
                    return None;
                }
            }
            Reentry::Abandon(end) => return Some(end),
        }
    }
}

/// Terminal outcomes: persist `cron_tick_end` inline, evict the tick job's
/// context, and report the tick end to the mission loop. Returns `None` when
/// the outcome is not terminal and the chain should continue.
async fn resolve_terminal(
    ctx: &TickCtx<'_>,
    tick_index: u64,
    chain_index: u32,
    job_id: Uuid,
    outcome: &TickOutcome,
) -> Option<TickEnd> {
    let label = match outcome {
        TickOutcome::Completed => "completed",
        TickOutcome::Cancelled => "cancelled",
        _ => return None,
    };
    tracing::info!(
        worker = %ctx.worker.name,
        mission_id = %ctx.mission_id,
        tick_index,
        chain_index,
        %job_id,
        outcome = %label,
        "Cron tick resolved"
    );
    persist_cron_event(
        ctx.manager.store_handle(),
        job_id,
        "cron_tick_end",
        serde_json::json!({
            "mission_id": ctx.mission_id,
            "tick_index": tick_index,
            "chain_index": chain_index,
            "outcome": label,
        }),
    )
    .await;
    evict_tick_context(ctx.manager, job_id).await;
    Some(TickEnd::Resolved)
}

/// Busy-rejection handling (CU-1): the old link may still hold the worker's
/// workspace lock, so retry the same chain link with backoff instead of
/// spending re-entry budget. Bounded by `MAX_BUSY_RETRIES`.
async fn handle_busy_rejection(
    ctx: &TickCtx<'_>,
    tick_index: u64,
    chain_index: u32,
    job_id: Uuid,
    outcome: &TickOutcome,
    busy_retries: &mut u32,
) -> Option<BusyAction> {
    let TickOutcome::Failed(reason) = outcome else {
        return None;
    };
    if !reason.contains(BUSY_MARKER) {
        return None;
    }
    *busy_retries += 1;
    if *busy_retries > MAX_BUSY_RETRIES {
        let reason = format!("workspace still busy after {MAX_BUSY_RETRIES} retries");
        return Some(BusyAction::Abandon(
            abandon_tick(ctx, tick_index, chain_index, job_id, reason).await,
        ));
    }
    tracing::warn!(
        worker = %ctx.worker.name,
        mission_id = %ctx.mission_id,
        tick_index,
        chain_index,
        %job_id,
        busy_retry = *busy_retries,
        "Cron tick rejected: workspace busy; backing off without spending re-entry budget"
    );
    persist_cron_event(
        ctx.manager.store_handle(),
        job_id,
        "cron_reentry",
        serde_json::json!({
            "mission_id": ctx.mission_id,
            "tick_index": tick_index,
            "chain_index": chain_index,
            "kind": "busy_retry",
            "busy_retry": *busy_retries,
        }),
    )
    .await;
    evict_tick_context(ctx.manager, job_id).await;
    Some(BusyAction::Retry)
}

enum BusyAction {
    Retry,
    Abandon(TickEnd),
}

/// Decide whether the chain re-dispatches or abandons. A heartbeat lapse is
/// cancelled with the supervisor reason and awaited to terminal first, so
/// the fresh link does not race the old runner's workspace lock (CU-1).
async fn prepare_reentry(
    ctx: &TickCtx<'_>,
    tick_index: u64,
    chain_index: u32,
    job_id: Uuid,
    outcome: &TickOutcome,
    max_reentries: u32,
) -> Reentry {
    if matches!(outcome, TickOutcome::HeartbeatLapse) {
        ctx.manager
            .cancel_task_with_reason(job_id, CancelReason::CronSupervisor)
            .await;
        await_terminal(ctx.manager, job_id).await;
    }
    if let TickOutcome::Failed(reason) = outcome
        && is_non_retryable(reason)
    {
        let reason = format!("non-retryable dispatch rejection: {}", describe_outcome(outcome));
        return Reentry::Abandon(
            abandon_tick(ctx, tick_index, chain_index, job_id, reason).await,
        );
    }
    if !outcome.is_retryable() || chain_index >= max_reentries {
        return Reentry::Abandon(
            abandon_tick(
                ctx,
                tick_index,
                chain_index,
                job_id,
                describe_outcome(outcome),
            )
            .await,
        );
    }
    let reason = describe_outcome(outcome);
    tracing::warn!(
        worker = %ctx.worker.name,
        mission_id = %ctx.mission_id,
        tick_index,
        chain_index,
        %job_id,
        %reason,
        "Cron tick job unresolved; re-dispatching a fresh bounded job after backoff"
    );
    persist_cron_event(
        ctx.manager.store_handle(),
        job_id,
        "cron_reentry",
        serde_json::json!({
            "mission_id": ctx.mission_id,
            "tick_index": tick_index,
            "chain_index": chain_index,
            "kind": "reentry",
            "reason": reason,
        }),
    )
    .await;
    evict_tick_context(ctx.manager, job_id).await;
    Reentry::Redispatch
}

enum Reentry {
    Redispatch,
    Abandon(TickEnd),
}

/// Persist the abandoned tick end and evict the link context.
async fn abandon_tick(
    ctx: &TickCtx<'_>,
    tick_index: u64,
    chain_index: u32,
    job_id: Uuid,
    reason: String,
) -> TickEnd {
    tracing::error!(
        worker = %ctx.worker.name,
        mission_id = %ctx.mission_id,
        tick_index,
        chain_index,
        %job_id,
        %reason,
        "Cron tick abandoned; mission continues at next scheduled fire"
    );
    persist_cron_event(
        ctx.manager.store_handle(),
        job_id,
        "cron_tick_end",
        serde_json::json!({
            "mission_id": ctx.mission_id,
            "tick_index": tick_index,
            "chain_index": chain_index,
            "outcome": "abandoned",
            "reason": reason,
        }),
    )
    .await;
    evict_tick_context(ctx.manager, job_id).await;
    TickEnd::Abandoned { job_id, reason }
}

/// Whether a worker rejection can never be fixed by re-dispatching (CU-4):
/// protocol validation (timeout above the bridge cap, bad project_dir) is a
/// config error, not a transient failure.
fn is_non_retryable(reason: &str) -> bool {
    NON_RETRYABLE_MARKERS
        .iter()
        .any(|marker| reason.contains(marker))
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

/// Exponential backoff between chain links; returns false when shutdown
/// fired during the wait.
async fn backoff_sleep(attempt: u32, shutdown_rx: &mut broadcast::Receiver<()>) -> bool {
    let shift = attempt.min(5);
    let delay = REENTRY_BACKOFF_BASE
        .saturating_mul(1u32 << shift)
        .min(REENTRY_BACKOFF_MAX);
    tokio::select! {
        _ = tokio::time::sleep(delay) => true,
        _ = shutdown_rx.recv() => false,
    }
}

/// Wait (bounded) for a cancelled tick job to reach a terminal state so the
/// next link does not dispatch while the old runner still owns the worker
/// workspace (CU-1).
async fn await_terminal(manager: &ExternalWorkerManager, job_id: Uuid) {
    let Some(cm) = manager.context_manager_handle() else {
        return;
    };
    let deadline = tokio::time::Instant::now() + TERMINAL_WAIT;
    loop {
        match cm.get_context(job_id).await {
            Ok(ctx)
                if matches!(
                    ctx.state,
                    JobState::Completed
                        | JobState::Accepted
                        | JobState::Failed
                        | JobState::Cancelled
                ) =>
            {
                return;
            }
            Ok(_) => {}
            Err(_) => return,
        }
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(
                %job_id,
                "Cancelled tick job did not reach a terminal state within the bounded wait"
            );
            return;
        }
        tokio::time::sleep(TERMINAL_POLL).await;
    }
}

/// Dispatch one bounded tick job: reserve, register, persist, then fire the
/// task on the worker with the structured `cron.tick` context attached.
async fn dispatch_tick_job(ctx: &TickCtx<'_>, tick_index: u64, chain_index: u32, job_id: Uuid) {
    let TickCtx {
        manager,
        worker,
        cron,
        mission_id,
        owner_id,
    } = *ctx;
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

    persist_tick_record(manager, worker, cron, job_id, owner_id, &description).await;

    let fired_at = Utc::now().to_rfc3339();
    let mut context = build_task_context(
        owner_id,
        cron.project_dir.as_deref(),
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
    )
    .await;
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
    cron: &CronWorkerConfig,
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
        project_dir: cron.project_dir.clone().unwrap_or_default(),
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
/// synthetic `[worker] ` heartbeats do not — see WI#263). Returns `None`
/// when daemon shutdown fires mid-watch so the caller can cancel promptly
/// instead of observing the shutdown only between ticks.
async fn watch_tick_job(
    ctx: &TickCtx<'_>,
    job_id: Uuid,
    shutdown_rx: &mut broadcast::Receiver<()>,
) -> Option<TickOutcome> {
    let Some(cm) = ctx.manager.context_manager_handle() else {
        return Some(TickOutcome::Missing);
    };
    let heartbeat_timeout = ctx.cron.heartbeat_timeout();
    // Poll no coarser than half the heartbeat window (floor 200ms) so short
    // watchdog configs are still enforced promptly.
    let poll = WATCH_POLL
        .min(heartbeat_timeout / 2)
        .max(Duration::from_millis(200));
    loop {
        tokio::select! {
            _ = tokio::time::sleep(poll) => {}
            _ = shutdown_rx.recv() => return None,
        }
        let job_ctx = match cm.get_context(job_id).await {
            Ok(job_ctx) => job_ctx,
            Err(_) => return Some(TickOutcome::Missing),
        };
        match job_ctx.state {
            JobState::Completed | JobState::Accepted => return Some(TickOutcome::Completed),
            JobState::Failed => {
                // Carry the transition reason so the chain can classify busy
                // workspace rejections (retry without spending budget) and
                // protocol-validation rejections (non-retryable).
                let reason = job_ctx
                    .transitions
                    .iter()
                    .rev()
                    .find(|t| t.to == JobState::Failed)
                    .and_then(|t| t.reason.clone())
                    .unwrap_or_else(|| format!("job entered {} state", job_ctx.state));
                return Some(TickOutcome::Failed(reason));
            }
            // Stuck means the idle detector already flagged this job as
            // wedged; route it through the lapse path so the supervisor
            // cancels before re-dispatching (a stuck job may otherwise
            // recover later — WI#263 Stuck→Completed — and double-run the
            // tick alongside its replacement).
            JobState::Stuck => return Some(TickOutcome::HeartbeatLapse),
            JobState::Cancelled => return Some(TickOutcome::Cancelled),
            JobState::Pending | JobState::InProgress | JobState::Submitted => {}
        }
        let baseline = job_ctx
            .last_activity_at
            .or(job_ctx.started_at)
            .unwrap_or(job_ctx.created_at);
        if Utc::now()
            .signed_duration_since(baseline)
            .to_std()
            .unwrap_or_default()
            > heartbeat_timeout
        {
            return Some(TickOutcome::HeartbeatLapse);
        }
    }
}

/// Evict a terminal tick job's in-memory context so a long-lived mission
/// does not grow the ContextManager without bound (CU-2). The persisted
/// sandbox-job row and `job_events` remain the durable record; `job_status`
/// falls back to the DB once the in-memory entry is gone.
async fn evict_tick_context(manager: &ExternalWorkerManager, job_id: Uuid) {
    if let Some(cm) = manager.context_manager_handle()
        && let Err(error) = cm.remove_job(job_id).await
    {
        tracing::debug!(%job_id, "Tick context eviction skipped: {error}");
    }
}

/// Persist a structured supervisor event to `job_events` (WI#325 logging).
/// Awaited inline: a detached spawn could reorder rows or drop them during
/// shutdown teardown.
async fn persist_cron_event(
    store: Option<&Arc<dyn Database>>,
    job_id: Uuid,
    event_type: &str,
    value: serde_json::Value,
) {
    let Some(db) = store else {
        return;
    };
    if let Err(error) = db.save_job_event(job_id, event_type, &value).await {
        tracing::warn!(%job_id, %event_type, "Failed to persist cron supervisor event: {error}");
    }
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

/// Settings-store key for the persisted per-mission tick counter.
fn tick_counter_key(mission_id: &str) -> String {
    format!("cron-mission:{mission_id}:tick-index")
}

/// Settings-store key for the persisted per-mission pause flag.
fn paused_key(mission_id: &str) -> String {
    format!("cron-mission:{mission_id}:paused")
}

/// Load durable mission state (tick counter + pause flag) from the
/// DB-backed settings store so a restart neither resets the tick index nor
/// silently resumes an owner-paused mission.
async fn load_mission_state(
    store: Option<&Arc<dyn Database>>,
    owner_id: &str,
    mission_id: &str,
) -> MissionState {
    let Some(db) = store else {
        return MissionState::default();
    };
    let tick_index = db
        .get_setting(owner_id, &tick_counter_key(mission_id))
        .await
        .ok()
        .flatten()
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let paused = db
        .get_setting(owner_id, &paused_key(mission_id))
        .await
        .ok()
        .flatten()
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    MissionState {
        tick_index,
        consecutive_abandoned: 0,
        paused,
    }
}

/// Persist the per-mission tick counter (best-effort; a lost write only
/// reuses an index after a restart, never double-fires a tick).
async fn persist_tick_counter(
    manager: &ExternalWorkerManager,
    owner_id: &str,
    mission_id: &str,
    tick_index: u64,
) {
    let Some(db) = manager.store_handle() else {
        return;
    };
    if let Err(error) = db
        .set_setting(
            owner_id,
            &tick_counter_key(mission_id),
            &serde_json::json!(tick_index),
        )
        .await
    {
        tracing::warn!(%mission_id, "Failed to persist cron tick counter: {error}");
    }
}

/// Persist the per-mission pause flag so owner pauses and circuit-breaker
/// trips survive daemon restarts.
async fn persist_paused(
    manager: &ExternalWorkerManager,
    owner_id: &str,
    mission_id: &str,
    paused: bool,
) {
    let Some(db) = manager.store_handle() else {
        return;
    };
    if let Err(error) = db
        .set_setting(owner_id, &paused_key(mission_id), &serde_json::json!(paused))
        .await
    {
        tracing::warn!(%mission_id, "Failed to persist cron pause flag: {error}");
    }
}

/// Build the tick job prompt: the mission goal plus the chain-handoff
/// contract the worker follows via its durable workspace mount.
fn build_tick_prompt(
    cron: &CronWorkerConfig,
    mission_id: &str,
    tick_index: u64,
    chain_index: u32,
) -> String {
    let state_dir = cron.project_dir.as_deref().unwrap_or("/workspace");
    format!(
        "You are the resident worker for mission '{mission_id}', firing on cron schedule \
         '{schedule}'.\n\nMISSION GOAL:\n{goal}\n\nThis is tick #{tick_index} (attempt \
         {attempt}). Mission state is durable in your workspace directory ({state_dir}): read \
         prior state at the start of the tick and write updated state before you finish. The \
         host supervisor holds the mission lease — if this job dies or goes silent, a fresh job \
         is dispatched and resumes from the workspace. Work the mission goal for this tick, then \
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
            allow_subminute_schedule: None,
            project_dir: None,
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
    fn tick_prompt_uses_mission_project_dir_when_configured() {
        let mut cron = test_cron();
        cron.project_dir = Some("missions/pr-digest".to_string());
        let prompt = build_tick_prompt(&cron, "pr-digest", 1, 0);
        assert!(prompt.contains("missions/pr-digest"));
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
    fn busy_and_non_retryable_markers_classify() {
        assert!("Workspace /workspace is already being modified by task x".contains(BUSY_MARKER));
        assert!(is_non_retryable(
            "timeout_ms must be an integer between 1 and 14400000"
        ));
        assert!(is_non_retryable(
            "project_dir 'missions/x' could not be resolved under WORKSPACE_ROOT: ENOENT"
        ));
        assert!(!is_non_retryable("worker exploded"));
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
