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
//!   `tick_timeout_ms` (bounded by the host's 4h cap at load) plus
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
use std::sync::atomic::{AtomicBool, Ordering};
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
    CancelReason, CronCommand, CronCommandOrigin, CronMissionControl, CronMissionStatus, CronTick,
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

/// Worker-reported markers that a dispatch rejection is a workspace-busy /
/// concurrency collision, not a mission failure (MR !1522 DE-2/DE-5): the
/// kimi and pebble bridges share the "already being modified" wording for the
/// per-workspace lock, and kimi additionally rejects with its connection
/// concurrency cap wording.
const BUSY_MARKERS: &[&str] = &["already being modified", "Worker concurrency limit reached"];

/// Worker-reported markers for protocol/config rejections that retrying can
/// never fix (bad timeout, unresolvable/escaping project_dir, empty prompt).
/// Covers both shipped bridges (MR !1522 DE-5): pebble wraps a missing dir as
/// "could not be resolved under WORKSPACE_ROOT", both use the same
/// "Workspace path escapes WORKSPACE_ROOT" wording for traversal, and on
/// pre-DN-2 images the kimi/opencode bridges surface Node's raw realpathSync
/// failure — which names the failing syscall `lstat`, not `realpath`
/// (MR !1531 DQ-2).
const NON_RETRYABLE_MARKERS: &[&str] = &[
    "timeout_ms must be",
    "could not be resolved under WORKSPACE_ROOT",
    "Workspace path escapes WORKSPACE_ROOT",
    "Invalid workspace path",
    "Missing or invalid prompt",
    "ENOENT: no such file or directory, lstat",
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
    /// The tick resolved (completed, cancelled by the owner, or its chain was
    /// stopped at a link boundary by an owner pause).
    Resolved,
    /// The tick exhausted its re-entry budget or hit a non-retryable error.
    Abandoned { job_id: Uuid, reason: String },
    /// The tick was skipped because the workspace stayed busy / the context
    /// manager stayed at capacity through the whole busy-retry budget
    /// (MR !1522 DE-2): ordinary contention, NOT mission failure — the
    /// circuit breaker must not count it, and the next scheduled fire
    /// retries normally.
    SkippedBusy,
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
    /// Most recent tick job id — used to key owner-command audit events to a
    /// real job row (`job_events.job_id` references `agent_jobs`). Shared
    /// with the in-flight tick via a std mutex (never held across `.await`).
    last_tick_job_id: &'a Arc<std::sync::Mutex<Option<Uuid>>>,
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
        // Record the mission owner up front: the command gate rejects
        // non-owners uniformly before any worker-mode lookup (MR !1522 DE-9).
        self.cron_owner_id
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(owner_id.to_string());
        for worker in self.cron_worker_configs() {
            let Some(cron) = worker.cron.clone() else {
                tracing::error!(
                    worker = %worker.name,
                    "External worker is mode=cron but has no [cron] block; \
                     mission supervisor disabled for this worker"
                );
                continue;
            };
            if let Err(reason) = validate_cron_schedule(&cron.schedule, cron.timezone.as_deref()) {
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
    /// run as a test trigger (no public manual-fire surface). The durable state handoff
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
    /// stays paused across daemon restarts until resumed. `origin` is
    /// recorded on the audit row (MR !1531 DQ-5).
    pub async fn pause_cron_mission(
        &self,
        worker_name: &str,
        owner_id: &str,
        origin: CronCommandOrigin,
    ) -> Result<(), OrchestratorError> {
        self.send_cron_command(worker_name, owner_id, CronCommand::Pause(origin))
            .await
    }

    /// Resume a paused cron mission and reset its circuit breaker. `origin`
    /// is recorded on the audit row (MR !1531 DQ-5).
    pub async fn resume_cron_mission(
        &self,
        worker_name: &str,
        owner_id: &str,
        origin: CronCommandOrigin,
    ) -> Result<(), OrchestratorError> {
        self.send_cron_command(worker_name, owner_id, CronCommand::Resume(origin))
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
        reply_rx
            .await
            .map_err(|_| OrchestratorError::CronSupervisorNotRunning {
                worker_name: worker_name.to_string(),
            })
    }

    /// Owner-gated command dispatch to a running mission supervisor. The
    /// gate mirrors `cancel_job`: only the mission's owning user may drive
    /// it. Non-owners get one uniform `CronMissionNotOwned` error regardless
    /// of the worker's mode or supervisor state, so the error surface cannot
    /// be used to probe which workers run cron missions (MR !1522 DE-9).
    async fn send_cron_command(
        &self,
        worker_name: &str,
        owner_id: &str,
        command: CronCommand,
    ) -> Result<(), OrchestratorError> {
        {
            let mission_owner = self
                .cron_owner_id
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(mission_owner) = mission_owner.as_deref()
                && mission_owner != owner_id
            {
                return Err(OrchestratorError::CronMissionNotOwned {
                    worker_name: worker_name.to_string(),
                });
            }
        }
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

/// Validate that a cron expression parses and yields fire times under the
/// same normalization the routine engine applies (5/6/7-field expressions,
/// optional IANA timezone — WI#325, MR !1522 DE-8).
fn validate_cron_schedule(schedule: &str, timezone: Option<&str>) -> Result<(), String> {
    match crate::agent::routine::next_cron_fire(schedule, timezone) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err("schedule yields no future fire times".to_string()),
        Err(error) => Err(error.to_string()),
    }
}

/// What woke the mission loop out of its idle wait.
enum Wake {
    /// The scheduled fire time arrived.
    Tick,
    /// An owner command arrived first.
    Command(CronCommand),
}

/// The per-mission loop: wait for each scheduled fire, then run the tick's
/// job chain to resolution before computing the next fire. Owner commands are
/// serviced while the loop waits AND while a tick chain runs (MR !1522 DE-3),
/// so pause/resume/status are never starved by a long-running chain.
async fn run_cron_mission(
    manager: Arc<ExternalWorkerManager>,
    worker: ExternalWorkerConfig,
    cron: CronWorkerConfig,
    mission_id: String,
    owner_id: String,
    mut shutdown_rx: broadcast::Receiver<()>,
    mut cmd_rx: mpsc::Receiver<CronCommand>,
) -> MissionExit {
    let last_job = load_last_tick_job(manager.store_handle(), &owner_id, &mission_id).await;
    let last_tick_job_id = Arc::new(std::sync::Mutex::new(last_job));
    let ctx = TickCtx {
        manager: &manager,
        worker: &worker,
        cron: &cron,
        mission_id: &mission_id,
        owner_id: &owner_id,
        last_tick_job_id: &last_tick_job_id,
    };
    let mut state = load_mission_state(manager.store_handle(), &owner_id, &mission_id).await;
    // Shared with the in-flight tick chain: an owner pause stops further
    // re-entries at the next link boundary, not only after the chain ends.
    let pause_flag = Arc::new(AtomicBool::new(state.paused));
    if state.paused {
        tracing::info!(
            worker = %worker.name,
            %mission_id,
            "Cron mission starts paused (persisted owner pause)"
        );
    }
    loop {
        if state.paused {
            if !paused_turn(&ctx, &mut state, &pause_flag, &mut cmd_rx, &mut shutdown_rx).await {
                return MissionExit::Shutdown;
            }
            continue;
        }
        let Some(fire_at) =
            next_fire_or_log(&worker.name, &cron.schedule, cron.timezone.as_deref())
        else {
            return MissionExit::ScheduleExhausted;
        };
        let wait = (fire_at - Utc::now()).to_std().unwrap_or_default();
        tracing::info!(
            worker = %worker.name,
            %mission_id,
            next_fire_at = %fire_at.to_rfc3339(),
            "Cron mission awaiting next tick"
        );
        let tick_index = match next_wake(wait, &mut cmd_rx, &mut shutdown_rx).await {
            None => return MissionExit::Shutdown,
            Some(Wake::Tick) => {
                state.tick_index += 1;
                persist_tick_counter(ctx.manager, &owner_id, &mission_id, state.tick_index).await;
                state.tick_index
            }
            Some(Wake::Command(command)) => {
                match handle_idle_command(&ctx, &mut state, &pause_flag, command).await {
                    IdleAction::Continue => continue,
                    // Manual fires use tick_index 0 as a manual/test trigger
                    // marker; the durable counter is not advanced.
                    IdleAction::FireNow => 0,
                }
            }
        };
        if !drive_tick(
            &ctx,
            &mut state,
            &pause_flag,
            tick_index,
            &mut cmd_rx,
            &mut shutdown_rx,
        )
        .await
        {
            return MissionExit::Shutdown;
        }
    }
}

/// Service one command while the mission is paused. Returns false when the
/// supervisor should exit (command channel closed or daemon shutdown).
/// `biased`: a queued owner command is applied before shutdown is observed,
/// so an acknowledged pause/resume is never lost to a same-instant shutdown.
async fn paused_turn(
    ctx: &TickCtx<'_>,
    state: &mut MissionState,
    pause_flag: &Arc<AtomicBool>,
    cmd_rx: &mut mpsc::Receiver<CronCommand>,
    shutdown_rx: &mut broadcast::Receiver<()>,
) -> bool {
    let command = tokio::select! {
        biased;
        command = cmd_rx.recv() => command,
        _ = shutdown_rx.recv() => return false,
    };
    let Some(command) = command else {
        return false;
    };
    if handle_idle_command(ctx, state, pause_flag, command).await == IdleAction::FireNow {
        return drive_tick(ctx, state, pause_flag, 0, cmd_rx, shutdown_rx).await;
    }
    true
}

/// Wait for the next scheduled fire, an owner command, or shutdown. Returns
/// `None` on shutdown / closed command channel. `biased`: a queued owner
/// command is applied before shutdown is observed (see `paused_turn`).
async fn next_wake(
    wait: Duration,
    cmd_rx: &mut mpsc::Receiver<CronCommand>,
    shutdown_rx: &mut broadcast::Receiver<()>,
) -> Option<Wake> {
    tokio::select! {
        biased;
        command = cmd_rx.recv() => command.map(Wake::Command),
        _ = shutdown_rx.recv() => None,
        _ = tokio::time::sleep(wait) => Some(Wake::Tick),
    }
}

/// Run one tick to resolution while still servicing owner commands, so a
/// multi-hour chain does not starve pause/resume/status (MR !1522 DE-3).
/// Returns false when daemon shutdown interrupted the tick: `run_tick`
/// observes shutdown on the shared receiver at every internal await point
/// (watch, backoff, terminal wait) and cancels the in-flight link with the
/// shutdown reason, so this loop only needs to service commands alongside.
async fn drive_tick(
    ctx: &TickCtx<'_>,
    state: &mut MissionState,
    pause_flag: &Arc<AtomicBool>,
    tick_index: u64,
    cmd_rx: &mut mpsc::Receiver<CronCommand>,
    shutdown_rx: &mut broadcast::Receiver<()>,
) -> bool {
    let tick = run_tick(ctx, tick_index, shutdown_rx, pause_flag);
    tokio::pin!(tick);
    let end = loop {
        // biased: a finished tick completes its bookkeeping first.
        tokio::select! {
            biased;
            end = &mut tick => break end,
            command = cmd_rx.recv() => {
                let Some(command) = command else {
                    if let Some(job_id) = last_tick_job(ctx) {
                        ctx.manager.cancel_task_with_reason(job_id, CancelReason::Shutdown).await;
                    }
                    return false;
                };
                handle_active_command(ctx, state, pause_flag, command).await;
            }
        }
    };
    let Some(end) = end else {
        return false;
    };
    after_tick(ctx, state, pause_flag, end).await;
    true
}

/// What the mission loop should do after an idle-time command.
#[derive(Debug, PartialEq, Eq)]
enum IdleAction {
    Continue,
    /// A manual/test trigger (`tick_index` 0) — allowed even while paused;
    /// being able to fire a paused mission by hand is the point of the
    /// escape hatch. The mission stays paused afterwards.
    FireNow,
}

/// Handle one owner command while no tick is running.
async fn handle_idle_command(
    ctx: &TickCtx<'_>,
    state: &mut MissionState,
    pause_flag: &Arc<AtomicBool>,
    command: CronCommand,
) -> IdleAction {
    match command {
        CronCommand::FireNow => {
            tracing::info!(worker = %ctx.worker.name, mission_id = %ctx.mission_id, "Manual cron tick fired");
            IdleAction::FireNow
        }
        CronCommand::Pause(origin) => {
            apply_owner_pause(ctx, state, pause_flag, origin).await;
            IdleAction::Continue
        }
        CronCommand::Resume(origin) => {
            apply_owner_resume(ctx, state, pause_flag, origin).await;
            IdleAction::Continue
        }
        CronCommand::Status(reply) => {
            reply_status(ctx, state, reply);
            IdleAction::Continue
        }
    }
}

/// Handle one owner command while a tick chain is running. Pause applies at
/// the next chain-link boundary (it never cancels the running link — that is
/// `cancel_job`'s job); a manual fire is skipped because a mission never
/// overlaps itself.
async fn handle_active_command(
    ctx: &TickCtx<'_>,
    state: &mut MissionState,
    pause_flag: &Arc<AtomicBool>,
    command: CronCommand,
) {
    match command {
        CronCommand::FireNow => {
            tracing::warn!(
                worker = %ctx.worker.name,
                mission_id = %ctx.mission_id,
                "Manual cron tick requested while a tick is running; skipped \
                 (a mission never overlaps itself)"
            );
        }
        CronCommand::Pause(origin) => apply_owner_pause(ctx, state, pause_flag, origin).await,
        CronCommand::Resume(origin) => apply_owner_resume(ctx, state, pause_flag, origin).await,
        CronCommand::Status(reply) => reply_status(ctx, state, reply),
    }
}

fn reply_status(
    ctx: &TickCtx<'_>,
    state: &MissionState,
    reply: tokio::sync::oneshot::Sender<CronMissionStatus>,
) {
    let _ = reply.send(CronMissionStatus {
        mission_id: ctx.mission_id.to_string(),
        worker_name: ctx.worker.name.clone(),
        paused: state.paused,
        tick_index: state.tick_index,
        consecutive_abandoned: state.consecutive_abandoned,
    });
}

/// Pause the mission on the owner's command: flip the live flag the
/// in-flight tick chain observes, persist the pause (restart-safe), and
/// record an auditable `cron_paused` event + SSE frame (MR !1522 DE-1 —
/// owner lifecycle commands must not leave only an `info!` line).
async fn apply_owner_pause(
    ctx: &TickCtx<'_>,
    state: &mut MissionState,
    pause_flag: &Arc<AtomicBool>,
    origin: CronCommandOrigin,
) {
    state.paused = true;
    pause_flag.store(true, Ordering::SeqCst);
    persist_paused(ctx.manager, ctx.owner_id, ctx.mission_id, true).await;
    tracing::info!(worker = %ctx.worker.name, mission_id = %ctx.mission_id, "Cron mission paused by owner");
    record_lifecycle_event(
        ctx,
        "cron_paused",
        serde_json::json!({
            "mission_id": ctx.mission_id,
            "origin": origin.as_str(),
        }),
        format!("cron mission {} paused by owner", ctx.mission_id),
    )
    .await;
}

/// Resume the mission on the owner's command and reset the circuit breaker.
async fn apply_owner_resume(
    ctx: &TickCtx<'_>,
    state: &mut MissionState,
    pause_flag: &Arc<AtomicBool>,
    origin: CronCommandOrigin,
) {
    state.paused = false;
    state.consecutive_abandoned = 0;
    pause_flag.store(false, Ordering::SeqCst);
    persist_paused(ctx.manager, ctx.owner_id, ctx.mission_id, false).await;
    tracing::info!(worker = %ctx.worker.name, mission_id = %ctx.mission_id, "Cron mission resumed by owner");
    record_lifecycle_event(
        ctx,
        "cron_resumed",
        serde_json::json!({
            "mission_id": ctx.mission_id,
            "origin": origin.as_str(),
        }),
        format!("cron mission {} resumed by owner", ctx.mission_id),
    )
    .await;
}

/// Persist a mission lifecycle event (`cron_paused` / `cron_resumed`) keyed
/// to the mission's most recent tick job, plus the matching SSE frame. A
/// mission that has never ticked has no job row to key the event to (the
/// `job_events.job_id` foreign key requires one), so the tracing line above
/// is the record in that case.
async fn record_lifecycle_event(
    ctx: &TickCtx<'_>,
    event_type: &str,
    value: serde_json::Value,
    sse_message: String,
) {
    let job_id = last_tick_job(ctx);
    let Some(job_id) = job_id else {
        tracing::info!(
            worker = %ctx.worker.name,
            mission_id = %ctx.mission_id,
            %event_type,
            "Mission lifecycle event before the first tick; no job row to key it to"
        );
        return;
    };
    persist_cron_event(ctx.manager.store_handle(), job_id, event_type, value).await;
    emit_cron_status(ctx.manager, job_id, ctx.owner_id, sse_message);
}

/// The mission's most recent tick job, if any.
fn last_tick_job(ctx: &TickCtx<'_>) -> Option<Uuid> {
    *ctx.last_tick_job_id
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Update circuit-breaker state after a tick resolves; trips into a
/// persisted pause after `CIRCUIT_BREAKER_THRESHOLD` consecutive abandons.
/// Busy/capacity skips leave the breaker untouched (MR !1522 DE-2).
async fn after_tick(
    ctx: &TickCtx<'_>,
    state: &mut MissionState,
    pause_flag: &Arc<AtomicBool>,
    end: TickEnd,
) {
    match end {
        TickEnd::Resolved => state.consecutive_abandoned = 0,
        // Ordinary busy/capacity contention must not trip the breaker
        // (MR !1522 DE-2): the mission simply retries at the next fire.
        TickEnd::SkippedBusy => {}
        TickEnd::Abandoned { job_id, reason } => {
            state.consecutive_abandoned += 1;
            if state.consecutive_abandoned >= CIRCUIT_BREAKER_THRESHOLD {
                state.paused = true;
                pause_flag.store(true, Ordering::SeqCst);
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
                        "origin": "circuit_breaker",
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
/// do this; treat as a hard config error if it does). The optional IANA
/// timezone is resolved exactly like routines (MR !1522 DE-8).
fn next_fire_or_log(
    worker_name: &str,
    schedule: &str,
    timezone: Option<&str>,
) -> Option<chrono::DateTime<Utc>> {
    match crate::agent::routine::next_cron_fire(schedule, timezone) {
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
/// each attempt; the mission id links the chain. An owner pause observed at a
/// link boundary stops the chain (the running link is never cancelled — that
/// is `cancel_job`'s job). Returns `None` when daemon shutdown interrupted
/// the tick.
async fn run_tick(
    ctx: &TickCtx<'_>,
    tick_index: u64,
    shutdown_rx: &mut broadcast::Receiver<()>,
    pause_flag: &Arc<AtomicBool>,
) -> Option<TickEnd> {
    let manager = ctx.manager;
    let max_reentries = ctx.cron.max_reentries();
    let mut chain_index: u32 = 0;
    let mut busy_retries: u32 = 0;
    let mut last_link: Option<Uuid> = None;
    loop {
        // Owner pause is honored at chain-link boundaries only: a manual fire
        // always dispatches its first link (the escape hatch works on a
        // paused mission), but no re-entry fires once paused.
        if (last_link.is_some() || busy_retries > 0) && pause_flag.load(Ordering::SeqCst) {
            return Some(pause_chain(ctx, tick_index, chain_index, last_link).await);
        }
        let job_id = Uuid::new_v4();
        if let Some(action) =
            dispatch_or_busy(ctx, tick_index, chain_index, job_id, &mut busy_retries).await
        {
            match action {
                BusyAction::Retry => {
                    if !backoff_sleep(busy_retries, shutdown_rx).await {
                        return None;
                    }
                    continue;
                }
                BusyAction::Skip(end) => return Some(end),
            }
        }

        // Capacity failures have no agent_jobs row (DN-6).
        last_link = Some(job_id);
        let Some(outcome) = watch_tick_job(ctx, job_id, shutdown_rx).await else {
            // Shutdown mid-tick: cancel the in-flight link with the shutdown
            // reason so it is not mistaken for an owner cancel.
            manager
                .cancel_task_with_reason(job_id, CancelReason::Shutdown)
                .await;
            return None;
        };

        if let Some(end) = resolve_terminal(ctx, tick_index, chain_index, job_id, &outcome).await {
            return Some(end);
        }
        if let Some(action) = watch_busy_step(
            ctx,
            tick_index,
            chain_index,
            job_id,
            &outcome,
            &mut busy_retries,
        )
        .await
        {
            match action {
                BusyAction::Retry => {
                    if !backoff_sleep(busy_retries, shutdown_rx).await {
                        return None;
                    }
                    continue;
                }
                BusyAction::Skip(end) => return Some(end),
            }
        }
        match prepare_reentry(
            ctx,
            tick_index,
            chain_index,
            job_id,
            &outcome,
            max_reentries,
            shutdown_rx,
        )
        .await?
        {
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

/// Record the pause-induced tick end against the last link so the tick does
/// not look lost, and report the chain stop as resolved (owner pause is not
/// mission failure evidence).
async fn pause_chain(
    ctx: &TickCtx<'_>,
    tick_index: u64,
    chain_index: u32,
    last_link: Option<Uuid>,
) -> TickEnd {
    if let Some(job_id) = last_link {
        persist_cron_event(
            ctx.manager.store_handle(),
            job_id,
            "cron_tick_end",
            serde_json::json!({
                "mission_id": ctx.mission_id,
                "tick_index": tick_index,
                "chain_index": chain_index,
                "outcome": "paused",
            }),
        )
        .await;
    }
    tracing::info!(
        worker = %ctx.worker.name,
        mission_id = %ctx.mission_id,
        tick_index,
        "Cron tick chain stopped at link boundary: mission paused"
    );
    TickEnd::Resolved
}

/// Dispatch one link; on a dispatch-time capacity failure run the busy-retry
/// path (no job row exists yet, so its events are log-only). Returns `None`
/// when the job was fired and the caller should watch it.
async fn dispatch_or_busy(
    ctx: &TickCtx<'_>,
    tick_index: u64,
    chain_index: u32,
    job_id: Uuid,
    busy_retries: &mut u32,
) -> Option<BusyAction> {
    match dispatch_tick_job(ctx, tick_index, chain_index, job_id).await {
        DispatchOutcome::Fired => None,
        DispatchOutcome::Capacity(reason) => Some(
            busy_retry_step(
                ctx,
                tick_index,
                chain_index,
                None,
                &reason,
                busy_retries,
                "capacity_retry",
            )
            .await,
        ),
    }
}

/// Worker busy rejections (workspace lock held, concurrency cap) retry the
/// same chain link without spending re-entry budget. Returns `None` for
/// non-busy outcomes.
async fn watch_busy_step(
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
    if !is_busy(reason) {
        return None;
    }
    Some(
        busy_retry_step(
            ctx,
            tick_index,
            chain_index,
            Some(job_id),
            reason,
            busy_retries,
            "busy_retry",
        )
        .await,
    )
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

/// Busy/capacity retry bookkeeping shared by worker busy rejections and
/// dispatch-time capacity failures (MR !1522 DE-2): contention is not
/// mission failure, so exhausting the budget SKIPS the tick without tripping
/// the circuit breaker — the next scheduled fire retries normally.
///
/// `job_id` is `None` when the dispatch never produced a persisted job row
/// (capacity failure before registration): events are log-only then because
/// `job_events.job_id` references `agent_jobs`.
async fn busy_retry_step(
    ctx: &TickCtx<'_>,
    tick_index: u64,
    chain_index: u32,
    job_id: Option<Uuid>,
    reason: &str,
    busy_retries: &mut u32,
    kind: &str,
) -> BusyAction {
    *busy_retries += 1;
    if *busy_retries > MAX_BUSY_RETRIES {
        let reason = format!("workspace still busy after {MAX_BUSY_RETRIES} retries: {reason}");
        tracing::error!(
            worker = %ctx.worker.name,
            mission_id = %ctx.mission_id,
            tick_index,
            chain_index,
            %reason,
            "Cron tick skipped: workspace busy; retrying at the next scheduled fire \
             (circuit breaker not affected — contention is not mission failure)"
        );
        if let Some(job_id) = job_id {
            persist_cron_event(
                ctx.manager.store_handle(),
                job_id,
                "cron_tick_end",
                serde_json::json!({
                    "mission_id": ctx.mission_id,
                    "tick_index": tick_index,
                    "chain_index": chain_index,
                    "outcome": "skipped_busy",
                    "reason": reason,
                }),
            )
            .await;
            evict_tick_context(ctx.manager, job_id).await;
        }
        return BusyAction::Skip(TickEnd::SkippedBusy);
    }
    tracing::warn!(
        worker = %ctx.worker.name,
        mission_id = %ctx.mission_id,
        tick_index,
        chain_index,
        ?job_id,
        busy_retry = *busy_retries,
        %reason,
        "Cron tick not dispatched: workspace busy / at capacity; backing off \
         without spending re-entry budget"
    );
    if let Some(job_id) = job_id {
        persist_cron_event(
            ctx.manager.store_handle(),
            job_id,
            "cron_reentry",
            serde_json::json!({
                "mission_id": ctx.mission_id,
                "tick_index": tick_index,
                "chain_index": chain_index,
                "kind": kind,
                "busy_retry": *busy_retries,
            }),
        )
        .await;
        evict_tick_context(ctx.manager, job_id).await;
    }
    BusyAction::Retry
}

enum BusyAction {
    Retry,
    Skip(TickEnd),
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
    shutdown_rx: &mut broadcast::Receiver<()>,
) -> Option<Reentry> {
    if matches!(outcome, TickOutcome::HeartbeatLapse) {
        ctx.manager
            .cancel_task_with_reason(job_id, CancelReason::CronSupervisor)
            .await;
        if !await_terminal(ctx.manager, job_id, shutdown_rx).await {
            return None;
        }
    }
    if let TickOutcome::Failed(reason) = outcome
        && is_non_retryable(reason)
    {
        let reason = format!(
            "non-retryable dispatch rejection: {}",
            describe_outcome(outcome)
        );
        return Some(Reentry::Abandon(
            abandon_tick(ctx, tick_index, chain_index, job_id, reason).await,
        ));
    }
    if !outcome.is_retryable() || chain_index >= max_reentries {
        return Some(Reentry::Abandon(
            abandon_tick(
                ctx,
                tick_index,
                chain_index,
                job_id,
                describe_outcome(outcome),
            )
            .await,
        ));
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
    Some(Reentry::Redispatch)
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

/// Whether a worker rejection is a workspace-busy / concurrency collision
/// (MR !1522 DE-2/DE-5): transient contention, never mission failure.
fn is_busy(reason: &str) -> bool {
    BUSY_MARKERS.iter().any(|marker| reason.contains(marker))
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
/// workspace (CU-1). Returns false on shutdown: the caller must end the
/// entire chain, because this receiver has consumed the shutdown message.
async fn await_terminal(
    manager: &ExternalWorkerManager,
    job_id: Uuid,
    shutdown_rx: &mut broadcast::Receiver<()>,
) -> bool {
    let Some(cm) = manager.context_manager_handle() else {
        return true;
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
                return true;
            }
            Ok(_) => {}
            Err(_) => return true,
        }
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(
                %job_id,
                "Cancelled tick job did not reach a terminal state within the bounded wait"
            );
            return true;
        }
        tokio::select! {
            _ = tokio::time::sleep(TERMINAL_POLL) => {}
            _ = shutdown_rx.recv() => return false,
        }
    }
}

/// How a tick dispatch attempt ended before any worker contact.
enum DispatchOutcome {
    /// The job was registered, persisted, and fired at the worker.
    Fired,
    /// The job could not be reserved/registered/persisted (context manager at
    /// capacity, duplicate reservation, DB error). No confirmed DB row exists, so any
    /// events are log-only (`job_events.job_id` references `agent_jobs`).
    Capacity(String),
}

/// Dispatch one bounded tick job: reserve, register, persist, then fire the
/// task on the worker with the structured `cron.tick` context attached.
async fn dispatch_tick_job(
    ctx: &TickCtx<'_>,
    tick_index: u64,
    chain_index: u32,
    job_id: Uuid,
) -> DispatchOutcome {
    let manager = ctx.manager;
    let worker = ctx.worker;
    let cron = ctx.cron;
    let mission_id = ctx.mission_id;
    let owner_id = ctx.owner_id;
    let project_dir = cron.resolved_project_dir(&worker.name);
    let title = format!("{mission_id} tick #{tick_index}");
    let description = build_tick_prompt(cron, mission_id, tick_index, chain_index, &project_dir);
    if let DispatchOutcome::Capacity(reason) =
        register_tick_job(ctx, tick_index, chain_index, job_id, &title, &description).await
    {
        return DispatchOutcome::Capacity(reason);
    }

    if !persist_tick_record(
        manager,
        worker,
        job_id,
        owner_id,
        &description,
        &project_dir,
    )
    .await
    {
        manager.release_task_reservation(job_id).await;
        evict_tick_context(manager, job_id).await;
        return DispatchOutcome::Capacity("tick job persistence failed".to_string());
    }
    // Record the link as the mission's most recent job so owner-command
    // audit events (cron_paused/cron_resumed) have a job row to key to.
    remember_tick_job(ctx, job_id).await;

    let tick_timeout_ms = cron.tick_timeout_ms();
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
            "fired_at": Utc::now().to_rfc3339(),
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

    let context = build_tick_context(
        cron,
        mission_id,
        owner_id,
        tick_index,
        chain_index,
        &project_dir,
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
    DispatchOutcome::Fired
}

/// Reserve and register the tick job and tag its metadata. Returns
/// `DispatchOutcome::Capacity` when the job never materialized (no context
/// manager, reservation conflict, or registration failure).
async fn register_tick_job(
    ctx: &TickCtx<'_>,
    tick_index: u64,
    chain_index: u32,
    job_id: Uuid,
    title: &str,
    description: &str,
) -> DispatchOutcome {
    if let Err(error) = ctx.manager.reserve_task(job_id, &ctx.worker.name).await {
        tracing::warn!(%job_id, worker = %ctx.worker.name, "Cron tick reservation failed: {error}");
        return DispatchOutcome::Capacity(format!("task reservation failed: {error}"));
    }
    let Some(cm) = ctx.manager.context_manager_handle() else {
        // Without a context manager the watcher can never observe the job;
        // treat as capacity-class so the tick retries instead of vanishing.
        ctx.manager.release_task_reservation(job_id).await;
        return DispatchOutcome::Capacity("no context manager registered".to_string());
    };
    if let Err(error) = cm
        .register_sandbox_job(job_id, ctx.owner_id, title, description)
        .await
    {
        tracing::warn!(%job_id, "Failed to register cron tick job: {error}");
        ctx.manager.release_task_reservation(job_id).await;
        return DispatchOutcome::Capacity(format!("job registration failed: {error}"));
    }
    let mission = ctx.mission_id.to_string();
    let schedule = ctx.cron.schedule.clone();
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
    DispatchOutcome::Fired
}

/// Build the task context for one tick link: the structured `cron.tick`
/// event plus the mission metadata, confined to the mission's workspace dir.
fn build_tick_context(
    cron: &CronWorkerConfig,
    mission_id: &str,
    owner_id: &str,
    tick_index: u64,
    chain_index: u32,
    project_dir: &str,
) -> crate::orchestrator::external_worker::TaskContext {
    let mut context = build_task_context(
        owner_id,
        Some(project_dir),
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
        fired_at: Utc::now().to_rfc3339(),
    });
    context
}

/// Persist the sandbox-job row for a tick job so it survives restarts and is
/// visible to `list_jobs`/`job_status` DB fallbacks.
async fn persist_tick_record(
    manager: &ExternalWorkerManager,
    worker: &ExternalWorkerConfig,
    job_id: Uuid,
    owner_id: &str,
    description: &str,
    project_dir: &str,
) -> bool {
    let Some(store) = manager.store_handle() else {
        return true;
    };
    let now = Utc::now();
    let record = SandboxJobRecord {
        id: job_id,
        task: description.to_string(),
        status: "running".to_string(),
        user_id: owner_id.to_string(),
        project_dir: project_dir.to_string(),
        success: None,
        failure_reason: None,
        created_at: now,
        started_at: Some(now),
        completed_at: None,
        credential_grants_json: "[]".to_string(),
    };
    if let Err(error) = store.save_sandbox_job(&record).await {
        tracing::warn!(%job_id, "Failed to persist cron tick job record: {error}");
        return false;
    }
    let mode = crate::orchestrator::job_manager::JobMode::External(worker.name.clone()).db_value();
    if let Err(error) = store.update_sandbox_job_mode(job_id, &mode).await {
        tracing::warn!(%job_id, "Failed to persist cron tick job mode: {error}");
    }
    true
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

async fn remember_tick_job(ctx: &TickCtx<'_>, job_id: Uuid) {
    ctx.last_tick_job_id
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .replace(job_id);
    if let Some(db) = ctx.manager.store_handle()
        && let Err(error) = db
            .set_setting(
                ctx.owner_id,
                &format!("cron-mission:{}:last-job", ctx.mission_id),
                &serde_json::json!(job_id),
            )
            .await
    {
        tracing::warn!(%job_id, "Failed to persist last cron tick job: {error}");
    }
}

async fn load_last_tick_job(
    store: Option<&Arc<dyn Database>>,
    owner: &str,
    mission: &str,
) -> Option<Uuid> {
    let db = store?;
    let value = db
        .get_setting(owner, &format!("cron-mission:{mission}:last-job"))
        .await
        .ok()??;
    let id = value.as_str().and_then(|s| Uuid::parse_str(s).ok())?;
    // A deleted job cannot anchor lifecycle events (nor may another owner's
    // job). This also keeps Postgres' job_events FK valid after retention.
    db.get_sandbox_job(id)
        .await
        .ok()??
        .user_id
        .eq(owner)
        .then_some(id)
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
        .set_setting(
            owner_id,
            &paused_key(mission_id),
            &serde_json::json!(paused),
        )
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
    project_dir: &str,
) -> String {
    format!(
        "You are the resident worker for mission '{mission_id}', firing on cron schedule \
         '{schedule}'.\n\nMISSION GOAL:\n{goal}\n\nThis is tick #{tick_index} (attempt \
         {attempt}). Mission state is durable in your workspace directory ({project_dir}): read \
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
            timezone: None,
        }
    }

    #[test]
    fn valid_schedules_pass_validation() {
        // 5-field, 6-field, and 7-field forms all normalize.
        assert!(validate_cron_schedule("0 15 10 * * *", None).is_ok());
        assert!(validate_cron_schedule("*/5 * * * *", None).is_ok());
        assert!(validate_cron_schedule("0 0 15 10 * *", None).is_ok());
        assert!(validate_cron_schedule("0 0 15 10 * * *", None).is_ok());
        // Timezone-qualified schedules validate the same way routines do.
        assert!(validate_cron_schedule("0 15 10 * * *", Some("America/New_York")).is_ok());
    }

    #[test]
    fn invalid_schedules_fail_validation() {
        assert!(validate_cron_schedule("not a schedule", None).is_err());
        assert!(validate_cron_schedule("61 * * * *", None).is_err());
        assert!(validate_cron_schedule("", None).is_err());
    }

    #[test]
    fn tick_prompt_carries_mission_context() {
        let cron = test_cron();
        let project_dir = cron.resolved_project_dir("kimi");
        let prompt = build_tick_prompt(&cron, "pr-digest", 7, 1, &project_dir);
        assert!(prompt.contains("pr-digest"));
        assert!(prompt.contains("Produce the daily PR digest"));
        assert!(prompt.contains("tick #7"));
        assert!(prompt.contains("attempt 2"));
        // Default mission workspace: missions/<mission_id> (MR !1522 DE-2).
        assert!(prompt.contains("missions/pr-digest"));
    }

    #[test]
    fn tick_prompt_uses_mission_project_dir_when_configured() {
        let mut cron = test_cron();
        cron.project_dir = Some("missions/custom".to_string());
        let project_dir = cron.resolved_project_dir("kimi");
        let prompt = build_tick_prompt(&cron, "pr-digest", 1, 0, &project_dir);
        assert!(prompt.contains("missions/custom"));
    }

    #[test]
    fn tick_outcome_retry_rules() {
        assert!(TickOutcome::Failed("x".to_string()).is_retryable());
        assert!(TickOutcome::HeartbeatLapse.is_retryable());
        assert!(TickOutcome::Missing.is_retryable());
        assert!(!TickOutcome::Completed.is_retryable());
        assert!(!TickOutcome::Cancelled.is_retryable());
    }

    /// MR !1522 DE-5: busy and non-retryable classification must cover BOTH
    /// shipped worker bridges' wordings (kimi/opencode lstat/ENOENT +
    /// concurrency cap, pebble's wrapped resolution error, the shared escape
    /// wording).
    #[test]
    fn busy_and_non_retryable_markers_classify() {
        assert!(is_busy(
            "Workspace /workspace is already being modified by task x"
        ));
        assert!(is_busy("Worker concurrency limit reached"));
        assert!(!is_busy("worker exploded"));

        assert!(is_non_retryable(
            "timeout_ms must be an integer between 1 and 14400000"
        ));
        // pebble: missing mission dir.
        assert!(is_non_retryable(
            "project_dir 'missions/x' could not be resolved under WORKSPACE_ROOT: ENOENT"
        ));
        // kimi/opencode on pre-DN-2 images: missing mission dir surfaces as
        // Node's raw realpathSync failure, whose syscall name is `lstat`
        // (real bridge output shape, MR !1531 DQ-2).
        assert!(is_non_retryable(
            "ENOENT: no such file or directory, lstat '/workspace/missions/x'"
        ));
        // both bridges: traversal out of WORKSPACE_ROOT.
        assert!(is_non_retryable(
            "Workspace path escapes WORKSPACE_ROOT: ../etc"
        ));
        assert!(!is_non_retryable("worker exploded"));
        assert!(!is_non_retryable(
            "realpath failed during the user's build; retry after checkout"
        ));
    }

    #[tokio::test]
    async fn shutdown_during_terminal_wait_ends_reentry() {
        let cm = Arc::new(crate::context::ContextManager::new(1));
        let (events, _) = broadcast::channel(8);
        let manager =
            Arc::new(ExternalWorkerManager::new(vec![]).with_event_deps(events, cm.clone()));
        let worker = ExternalWorkerConfig::default();
        let cron = test_cron();
        let last = Arc::new(std::sync::Mutex::new(None));
        let ctx = test_ctx(&manager, &worker, &cron, &last);
        let job_id = Uuid::new_v4();
        cm.register_sandbox_job(job_id, "owner", "tick", "pending cancel")
            .await
            .unwrap();
        let (shutdown, mut receiver) = broadcast::channel(1);
        let reentry = prepare_reentry(
            &ctx,
            1,
            0,
            job_id,
            &TickOutcome::HeartbeatLapse,
            3,
            &mut receiver,
        );
        tokio::pin!(reentry);
        // Poll until the non-terminal context makes await_terminal suspend,
        // then deliver the one shutdown message it used to swallow (DN-1).
        tokio::select! {
            biased;
            _ = &mut reentry => panic!("must wait for the active link"),
            _ = std::future::ready(()) => {}
        }
        shutdown.send(()).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), reentry)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn reentry_cap_abandons_at_boundary() {
        let manager = Arc::new(ExternalWorkerManager::new(vec![]));
        let worker = ExternalWorkerConfig::default();
        let cron = test_cron();
        let last = Arc::new(std::sync::Mutex::new(None));
        let ctx = test_ctx(&manager, &worker, &cron, &last);
        let (_shutdown, mut receiver) = broadcast::channel(1);
        for (index, allowed) in [(1, true), (2, false), (3, false)] {
            let result = prepare_reentry(
                &ctx,
                1,
                index,
                Uuid::new_v4(),
                &TickOutcome::Failed("retryable".into()),
                2,
                &mut receiver,
            )
            .await;
            assert_eq!(matches!(result, Some(Reentry::Redispatch)), allowed);
            if !allowed {
                assert!(matches!(result, Some(Reentry::Abandon(_))));
            }
        }
    }

    #[tokio::test]
    async fn queued_owner_command_precedes_shutdown() {
        // Every iteration has both arms ready. Repetition catches a removed
        // bias instead of giving randomized select order a 50% false pass.
        for _ in 0..64 {
            let (commands, mut rx) = mpsc::channel(2);
            let (shutdown, mut shutdown_rx) = broadcast::channel(1);
            commands
                .send(CronCommand::Pause(CronCommandOrigin::OwnerTool))
                .await
                .unwrap();
            shutdown.send(()).unwrap();
            assert!(matches!(
                next_wake(Duration::ZERO, &mut rx, &mut shutdown_rx).await,
                Some(Wake::Command(CronCommand::Pause(_)))
            ));
            assert!(
                next_wake(Duration::from_secs(60), &mut rx, &mut shutdown_rx)
                    .await
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn paused_mission_applies_queued_command_before_shutdown() {
        let manager = Arc::new(ExternalWorkerManager::new(vec![]));
        let worker = ExternalWorkerConfig::default();
        let cron = test_cron();
        let last = Arc::new(std::sync::Mutex::new(None));
        let ctx = test_ctx(&manager, &worker, &cron, &last);
        for _ in 0..64 {
            let (commands, mut rx) = mpsc::channel(2);
            let (shutdown, mut shutdown_rx) = broadcast::channel(1);
            let pause = Arc::new(AtomicBool::new(true));
            let mut state = MissionState {
                paused: true,
                ..Default::default()
            };
            commands
                .send(CronCommand::Resume(CronCommandOrigin::OwnerTool))
                .await
                .unwrap();
            shutdown.send(()).unwrap();
            assert!(paused_turn(&ctx, &mut state, &pause, &mut rx, &mut shutdown_rx).await);
            assert!(!state.paused);
            assert!(!paused_turn(&ctx, &mut state, &pause, &mut rx, &mut shutdown_rx).await);
        }
    }

    /// MR !1531 DN-6 (rewritten for DQ-1/DR-1): capacity rejections must not
    /// persist anything — no sandbox-job row and no last-job pointer — and a
    /// pause landing during the busy-retry backoff must end the tick without
    /// writing events for a job that never fired. Goes through the Database
    /// trait only (architecture-boundaries Check 1); surviving job_events
    /// rows cannot pin the no-orphan-event half because the FK rejects them,
    /// so that half is asserted by the recording-store follow-up in WI#370.
    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn capacity_retry_then_pause_persists_nothing() {
        let (store, _dir) = crate::testing::test_db().await;
        let worker = ExternalWorkerConfig::default();
        let (events, _) = broadcast::channel(8);
        let manager = Arc::new(
            ExternalWorkerManager::new(vec![worker.clone()])
                .with_event_deps(events, Arc::new(crate::context::ContextManager::new(0)))
                .with_store(store.clone()),
        );
        let cron = test_cron();
        let last = Arc::new(std::sync::Mutex::new(None));
        let ctx = test_ctx(&manager, &worker, &cron, &last);
        let pause = Arc::new(AtomicBool::new(false));
        let (_shutdown, mut receiver) = broadcast::channel(1);
        let tick = run_tick(&ctx, 1, &mut receiver, &pause);
        tokio::pin!(tick);
        tokio::select! {
            biased;
            _ = &mut tick => panic!("capacity rejection must back off"),
            _ = std::future::ready(()) => {}
        }
        pause.store(true, Ordering::SeqCst);
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(6), tick)
                .await
                .unwrap(),
            Some(TickEnd::Resolved)
        ));
        // DN-6: reserve failed before persist, so no job row may exist and no
        // last-job pointer may have been recorded.
        assert!(store.list_sandbox_jobs().await.unwrap().is_empty());
        assert!(last_tick_job(&ctx).is_none());
    }

    /// Test TickCtx over an empty manager (no store/context manager: all
    /// persistence and SSE calls become no-ops).
    fn test_ctx<'a>(
        manager: &'a Arc<ExternalWorkerManager>,
        worker: &'a ExternalWorkerConfig,
        cron: &'a CronWorkerConfig,
        last_tick_job_id: &'a Arc<std::sync::Mutex<Option<Uuid>>>,
    ) -> TickCtx<'a> {
        TickCtx {
            manager,
            worker,
            cron,
            mission_id: "pr-digest",
            owner_id: "owner",
            last_tick_job_id,
        }
    }

    /// MR !1522 DE-6: the circuit breaker must actually trip at the
    /// threshold, stay latched below it, reset on a resolved tick — and
    /// busy/capacity skips must NEVER count (DE-2).
    #[tokio::test]
    async fn circuit_breaker_counts_abandons_but_not_busy_skips() {
        let manager = Arc::new(ExternalWorkerManager::new(vec![]));
        let worker = ExternalWorkerConfig::default();
        let cron = test_cron();
        let last = Arc::new(std::sync::Mutex::new(None));
        let ctx = test_ctx(&manager, &worker, &cron, &last);
        let pause = Arc::new(AtomicBool::new(false));
        let mut state = MissionState::default();

        for _ in 0..10 {
            after_tick(&ctx, &mut state, &pause, TickEnd::SkippedBusy).await;
        }
        assert!(!state.paused, "busy skips must never trip the breaker");
        assert_eq!(state.consecutive_abandoned, 0);

        for attempt in 1..CIRCUIT_BREAKER_THRESHOLD {
            after_tick(
                &ctx,
                &mut state,
                &pause,
                TickEnd::Abandoned {
                    job_id: Uuid::new_v4(),
                    reason: format!("abandon {attempt}"),
                },
            )
            .await;
            assert!(!state.paused, "breaker tripped early at {attempt}");
            assert_eq!(state.consecutive_abandoned, attempt);
        }
        after_tick(
            &ctx,
            &mut state,
            &pause,
            TickEnd::Abandoned {
                job_id: Uuid::new_v4(),
                reason: "final".to_string(),
            },
        )
        .await;
        assert!(state.paused, "breaker must trip at the threshold");
        assert!(pause.load(Ordering::SeqCst), "live pause flag must follow");

        // A resolved tick resets the counter.
        after_tick(&ctx, &mut state, &pause, TickEnd::Resolved).await;
        assert_eq!(state.consecutive_abandoned, 0);
    }

    /// MR !1522 DE-6: the busy-retry budget retries exactly MAX_BUSY_RETRIES
    /// times and then skips the tick (SkippedBusy), never abandoning it.
    #[tokio::test]
    async fn busy_retry_budget_exhaustion_skips_without_abandoning() {
        let manager = Arc::new(ExternalWorkerManager::new(vec![]));
        let worker = ExternalWorkerConfig::default();
        let cron = test_cron();
        let last = Arc::new(std::sync::Mutex::new(None));
        let ctx = test_ctx(&manager, &worker, &cron, &last);
        let mut retries = 0;
        for expected in 1..=MAX_BUSY_RETRIES {
            let action = busy_retry_step(
                &ctx,
                1,
                0,
                None,
                "Workspace /workspace is already being modified by task x",
                &mut retries,
                "busy_retry",
            )
            .await;
            assert!(
                matches!(action, BusyAction::Retry),
                "busy retry {expected} must retry"
            );
        }
        let action = busy_retry_step(
            &ctx,
            1,
            0,
            None,
            "Workspace /workspace is already being modified by task x",
            &mut retries,
            "busy_retry",
        )
        .await;
        assert!(
            matches!(action, BusyAction::Skip(TickEnd::SkippedBusy)),
            "exhaustion must skip, not abandon"
        );
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
