# External Worker Cron Mode (WI#325)

Status: implemented (host-side) in v2.1.6.0; revised after the MR !1509
adversarial review (CU-1..CU-9) and the MR !1522 adversarial review
(DE-1..DE-10; see "Review-driven hardening").
Work item: https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/325

## Problem

External workers (kimi / opencode / pebble) execute **bounded dispatches**: a
job has a deadline from worker config, and the pool is built around many short
jobs over a long-lived connection. Missions — long-running, recurring,
stateful work — lived only in the in-process mission scheduler, which has a
documented failure cluster: WI#311 (cron missions park in `Completed` and
never fire again), WI#317 (boot-time snapshot writer races real fires),
WI#321 (silent disappearance on parse failure), WI#323 (no liveness logs).

**Plan D**: rather than hardening the in-process scheduler, host mission-grade
execution in external workers whose dispatcher is already reliable.

## Design

A worker opts in via config:

```toml
[[sandbox.external_workers]]
name = "kimi"
url = "ws://127.0.0.1:10007/ws/agent"
timeout_ms = 3600000
mode = "cron"

[sandbox.external_workers.cron]
schedule = "0 15 10 * * *"        # 5/6/7-field cron (same normalization as routines)
goal = "Produce the daily PR digest"
mission_id = "pr-digest"          # optional; default "cron-worker-<name>";
                                  # must be unique across cron workers
timezone = "America/New_York"     # optional IANA zone; default UTC
heartbeat_timeout_secs = 900      # optional; default 900 (15 min); floor 5
tick_timeout_ms = 14400000        # optional; default = 14_400_000 (the 4h
                                  # worker bridge cap); bounded 1_000..=14_400_000
max_reentries = 3                 # optional; default 3; hard cap 16
# allow_subminute_schedule = true # explicit override for fire gaps < 60s
# project_dir = "missions/pr-digest" # optional; default "missions/<mission_id>"
```

**Validation fails loudly at load**: an invalid cron block (unparseable or
sub-60s schedule without the override, empty goal, heartbeat below the floor,
re-entry count above the cap, tick timeout outside the bounded range, a
`project_dir` that is absolute/`..`-traversal/`~`-rooted, an unparseable
`timezone`, a duplicate `mission_id`, an unknown key — or a missing `[cron]`
block on a `mode = "cron"` worker) rejects the worker from the resolved config
with an `error!` naming the reason. A schedule typo can never boot a silently
mission-less worker (WI#321 class).

### 1. Cron-tick JSON events

On every scheduled fire the host supervisor dispatches a worker job whose
`task_request.context.cron_tick` carries the structured event:

```json
{
  "event": "cron.tick",
  "mission_id": "pr-digest",
  "schedule": "0 15 10 * * *",
  "goal": "Produce the daily PR digest",
  "tick_index": 12,
  "chain_index": 0,
  "fired_at": "2026-10-10T10:15:00+00:00"
}
```

The field is `skip_serializing_if = None` on `TaskContext`, so ordinary task
dispatches never carry it and older workers ignore it. The prompt also carries
the goal plus the chain-handoff contract ("read prior state from the workspace
at tick start; write updated state before finishing"), so even a worker that
only reads the prompt behaves mission-like.

`tick_index` is **persisted per mission** (DB settings store), so it is
monotonic across daemon restarts; manual fires use `tick_index = 0` as a
manual/test trigger marker and do not advance the durable counter.

### 2. Infinite deadline participation

The **host supervisor always holds the mission lease**. Tick jobs are not
subject to the standard bounded `timeout_ms` semantics: they run under the
cron block's own `tick_timeout_ms` — **defaulting to the 4h worker-bridge
cap**, i.e. a tick link outlives the worker's ordinary bounded deadline by
default — plus the heartbeat watchdog, which bounds *silence*, not work. The
mission itself has no deadline: it fires again at the next scheduled tick
regardless of how any one tick resolved. Because a cron-mode mission is never
an engine `Mission` record, the WI#311 `Completed`-parking lifecycle machine
and the WI#317 boot writer never touch it (structural, not behavioral,
exclusion).

The honest bound: a single link cannot exceed the worker bridge's hard timeout
cap (kimi's `KIMI_MAX_TIMEOUT_MS`, 4h by default) — the wire protocol rejects
it. Config validation refuses out-of-range values at load instead of letting
every tick fail on the wire, and a worker-side protocol-validation rejection
(`timeout_ms must be…`, unresolvable/escaping `project_dir` — both kimi's
`realpathSync` ENOENT and pebble's wrapped wording are classified) is treated
as **non-retryable**: the tick is abandoned immediately rather than burning
the re-entry budget on a config error. Missions longer than one link's bound
are expressed by chaining (the goal text should work in per-tick increments).

### 3. Re-dispatch / re-entry supervisor

`orchestrator::cron_worker` runs one serialized loop per cron-mode worker.
Scheduled fires and manual triggers share one mpsc command channel, and owner
commands are serviced **while a tick chain runs** (the tick is a pinned
future selected against the command channel), so pause/resume/status answer
immediately rather than after a potentially hours-long chain:

1. Compute the next fire from the cron schedule in the configured timezone
   (`agent::routine`'s normalization), sleeping in a `select!` that also
   services commands and shutdown — shutdown is observed mid-tick, not just
   between ticks, and cancels the in-flight link with a distinct
   `daemon shutdown` reason.
2. Dispatch a bounded tick job (fresh job id per link).
3. Watch the job via the `ContextManager`: terminal state resolves the tick;
   no genuine progress for `heartbeat_timeout_secs` (synthetic `[worker] …`
   keep-alives do not count — the WI#263 gate) cancels the wedged job with
   the `cron supervisor (heartbeat lapse)` reason and re-dispatches a fresh
   bounded job for the same tick, up to `max_reentries`. An owner pause is
   observed at each link boundary and stops the chain there (pause never
   cancels the running link — that is `cancel_job`'s job).
4. Mission state flows through the worker's durable `/workspace` bind mount
   (the WI#325 evidence section records live verification of cross-job
   durability); mission lifetime = the chain + the state, never any one
   process.

### 4. Logging / session capture

Per-link jobs already stream `task_progress` into `job_events`. The supervisor
adds structured records on top, all written **inline** (never detached
spawns, so rows are ordered and survive shutdown):

- `cron_tick` — on every dispatch (mission id, schedule, goal, tick/chain
  index, fired_at, tick_timeout_ms).
- `cron_reentry` — on every re-dispatch, with `kind`: `reentry` (failure or
  wedge, consumes budget), `busy_retry` (workspace lock/concurrency
  contention; does **not** consume budget), or `capacity_retry` (dispatch-time
  reservation/registration failure; log-only, since no job row exists yet).
- `cron_tick_end` — `completed` / `cancelled` / `abandoned` (+reason) /
  `skipped_busy` / `paused`.
- `cron_paused` / `cron_resumed` — on circuit-breaker trips AND on owner
  commands (keyed to the mission's most recent tick job; a mission that has
  never ticked has no job row, so the tracing line is the record then).

plus SSE `JobStatus` frames for live subscribers, and `info!`/`warn!`/`error!`
tracing at supervisor start, each tick fire, each re-entry, each tick end, and
supervisor death (a panicked mission loop is logged at `error!` instead of
silently stranding the mission — WI#323 class). After the fact, a mission's
full session log is retrievable by reading the per-tick jobs' `job_events`
(the mission id links the chain; each job's metadata carries `mission_id`,
`tick_index`, `chain_index`).

## Review-driven hardening (MR !1509)

- **Workspace-lock race (CU-1)** — after a heartbeat lapse the supervisor
  cancels with the supervisor reason, then waits (bounded, 20s, shutdown-
  aware) for the old link to reach a terminal state before re-dispatching,
  and applies exponential backoff (2s base, 60s cap) between links.
- **Runaway guards (CU-2)** — config validation (above), exponential backoff,
  and a **circuit breaker**: after 3 consecutive abandoned ticks the mission
  pauses itself, persists the pause (restart-safe), and surfaces it via
  `job_events` (`cron_paused`) and SSE.
- **Runtime stop (CU-2)** — the owner-gated `cron_mission` tool
  (`pause`/`resume`/`status`, gated exactly like `cancel_job`; AskEachTime
  permission; v1-only in the engine bridge). `cancel_job` still ends only the
  current link; `cron_mission pause` stops future fires and pending
  re-entries, and survives restart.
- **Memory growth (CU-2)** — terminal tick contexts are evicted from the
  `ContextManager` once their events are persisted; the durable record lives
  in `agent_jobs` + `job_events` (`job_status` falls back to the DB).
- **Self-repair interference (CU-5)** — jobs whose metadata carries
  `cron_mode: true` are exempt from the self-repair idle detector; the cron
  supervisor owns their watchdog, so no double-cancel and no spurious
  "Job … was stuck" owner notices.
- **Manual fires serialized (CU-9)** — `fire_cron_tick_now` routes through the
  supervisor's command channel; there is no out-of-band tick path.

## Review-driven hardening (MR !1522)

- **DE-1 (autonomous lockdown)** — `cron_mission` is on
  `AUTONOMOUS_TOOL_DENYLIST`, overrides `allows_autonomous_execution()` to
  `false`, and is a `PROTECTED_TOOL_NAMES` builtin: an owner routine
  processing injected content cannot silently pause a mission, and cannot
  reset the circuit breaker via `resume`. Owner pause/resume now persist
  `cron_paused`/`cron_resumed` `job_events` rows and emit SSE frames.
- **DE-2 (breaker vs. contention)** — busy rejections (workspace lock held,
  worker concurrency cap) and dispatch-time capacity failures retry the same
  chain link (bounded, 5×) and, on exhaustion, record `skipped_busy` and wait
  for the next scheduled fire **without counting toward the circuit
  breaker**. Ordinary tasks contending with a mission no longer pause it.
- **CU-1 residue / project_dir** — `cron.project_dir` now **defaults to
  `missions/<mission_id>`**, so a mission never shares the worker root with
  ordinary dispatches unless the operator opts into that, and the value is
  host-validated at load (relative, no `..`, no `~`, no `.` segments, no NUL)
  so classification no longer depends on per-bridge error wording. Honest
  caveat: the kimi/pebble workspace lock treats nested paths as overlapping,
  so an ordinary dispatch that uses the worker root still contends with a
  running tick — give ordinary tasks on a cron-mode worker their own
  non-overlapping `project_dir`. (The mock-bridge lock test proves the
  cancel→terminal-wait→busy-retry sequence does not race the lock.)
- **DE-3 (command starvation)** — the mission loop drives the tick chain as a
  pinned future selected against the command channel: pause/resume/status
  answer immediately mid-tick, and an owner pause stops pending re-entries at
  the next link boundary. `FireNow` during a running tick is skipped with a
  warning (a mission never overlaps itself).
- **DE-4 (merge hygiene)** — `ExternalWorkerConfig` has a shared `Default`;
  every test literal uses `..Default::default()`, so sibling MRs adding
  config fields no longer break this tree in auto-merged code.
- **DE-5 (per-bridge wording)** — `NON_RETRYABLE_MARKERS` and the busy
  markers cover both shipped bridges (kimi `realpathSync` ENOENT,
  "Workspace path escapes WORKSPACE_ROOT", "Worker concurrency limit
  reached"; pebble's wrapped "could not be resolved under WORKSPACE_ROOT").
- **DE-6 (test realism)** — the sub-minute override test uses a genuinely
  sub-60s schedule; new coverage: breaker trip + busy-skip immunity,
  busy-retry exhaustion, pause/tick-counter persistence across restart,
  context eviction, non-retryable abandonment end-to-end, status/pause
  during a running tick, owner-gate uniformity.
- **DE-7 (mission id uniqueness)** — duplicate `mission_id` across cron
  workers is rejected at load (persisted state is keyed by mission id).
- **DE-8 (timezone)** — `cron.timezone` accepts an IANA zone and resolves
  fire times with the same normalization as routines; default UTC.
- **DE-9 (uniform owner gate)** — non-owner callers get one
  `CronMissionNotOwned` error regardless of the named worker's mode or
  supervisor state; only the owner sees the precise distinctions.
- **DE-10 (loud config)** — `CronWorkerConfig` is `deny_unknown_fields`.

## Verification checklist (WI#325)

- [x] A `cron`-mode worker receives a `cron.tick` event on schedule and starts
  a mission-like run — `cron_scheduled_loop_fires_on_schedule` drives the real
  scheduled loop (per-second schedule) and asserts monotonic tick indices;
  `cron_tick_reaches_worker_with_structured_tick_context` asserts the on-wire
  payload.
- [x] The run outlives the standard bounded deadline without being killed —
  per-link `tick_timeout_ms` defaults to the 4h bridge cap (longer than any
  shipped worker's ordinary default), and the mission itself has no deadline:
  the supervisor holds the lease and chains fresh links. No `Completed`
  parking. (A single link cannot exceed the wire-protocol cap, documented
  above.)
- [x] If the job dies mid-run, the supervisor re-dispatches a fresh job and
  the mission resumes from workspace state — `cron_supervisor_redispatches_wedged_tick_job`
  (lapse path), `cron_reentry_on_failed_link` (failure path),
  `cron_busy_rejection_retries_without_spending_reentry_budget` (lock-enforcing
  mock). Cross-job workspace durability is a property of the worker bind mount
  recorded in the WI's evidence section; the chain itself is
  integration-tested.
- [x] Every tick and job transition is visible in `job_events` —
  `cron_tick_events_persist_to_job_events` asserts persisted `cron_tick` and
  `cron_tick_end` rows against a real store; owner pause/resume persist
  `cron_paused`/`cron_resumed` rows keyed to the mission's most recent tick
  job.
- [x] A `cron`-mode mission is NOT parked `Completed` by the in-process
  lifecycle machine — it is never an engine `Mission` at all.
- [~] Session audio/log capture — text session logs are covered by
  `job_events` + workspace state files; **audio capture is not implemented**
  (no audio path exists in external workers today) and remains out of scope
  for this item.

## Files

- `ic/src/config/sandbox.rs` — `ExternalWorkerMode`, `CronWorkerConfig`,
  validation/clamps (incl. project_dir host validation, timezone, mission-id
  uniqueness, `deny_unknown_fields`), `Default` for `ExternalWorkerConfig`,
  resolution from `[[sandbox.external_workers]]`.
- `ic/src/settings.rs` — settings fields (TOML/JSON/DB map plumbing).
- `ic/src/orchestrator/external_worker.rs` — `CronTick` payload on
  `TaskContext`; `CancelReason` plumbed through the cancel path;
  `CronCommand`/control-channel registry; uniform non-owner gate state.
- `ic/src/orchestrator/cron_worker.rs` — the supervisor (schedule loop,
  concurrent command servicing, tick dispatch, heartbeat watchdog, re-entry
  chain with backoff, busy/capacity skip path, circuit breaker, event
  persistence, context eviction, persisted tick counter/pause flag).
- `ic/src/agent/self_repair.rs` — cron-mode exemption from idle repair.
- `ic/src/agent/routine.rs` — `cron_min_fire_gap_secs` (schedule floor).
- `ic/src/tools/builtin/job.rs` — `CronMissionTool` (pause/resume/status;
  never autonomous).
- `ic/crates/lunarwing_engine/src/gate/tool_tier.rs` —
  `AUTONOMOUS_TOOL_DENYLIST` entry.
- `ic/src/tools/registry.rs` — `PROTECTED_TOOL_NAMES` entry.
- `ic/src/main.rs` — supervisor spawn next to the pool eviction task.
- `ic/tests/external_worker_integration.rs` — wire-shape, scheduled-loop,
  shutdown, re-entry, busy-lock, abandonment, breaker, owner-gate, command
  starvation, eviction, restart-persistence, and store tests.

## Out of scope

- Migrating existing engine missions wholesale (additive; see WI#324 for the
  proxied tool path that lets workers call `memory_*`/`knowledge_*`).
- Hub-mediated cross-thread coordination (KV + advisory locks) — WI#324 phase 2.
- Audio session capture (no audio path exists in external workers today).
- Worker-side consumption of `cron_tick` beyond the prompt (optional by
  design).
- A non-test manual-fire surface: `fire_cron_tick_now` is exercised by the
  integration tests; exposing it on a route is follow-up work if wanted.
