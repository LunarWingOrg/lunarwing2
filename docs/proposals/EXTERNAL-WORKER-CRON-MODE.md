# External Worker Cron Mode (WI#325)

Status: implemented (host-side) in v2.1.6.0.
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
mission_id = "pr-digest"          # optional; default "cron-worker-<name>"
heartbeat_timeout_secs = 900      # optional; default 900 (15 min)
tick_timeout_ms = 1800000         # optional; default = worker timeout_ms
max_reentries = 3                 # optional; default 3
```

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
the goal plus the chain-handoff contract ("read prior state from /workspace at
tick start; write updated state before finishing"), so even a worker that only
reads the prompt behaves mission-like.

### 2. Infinite deadline participation

The **host supervisor always holds the mission lease**. Tick jobs are not
subject to the standard bounded `timeout_ms` semantics: they run under the
cron block's own `tick_timeout_ms` (a per-link bound, not a mission bound)
plus the heartbeat watchdog. The mission itself has no deadline — it fires
again at the next scheduled tick regardless of how any one tick resolved.
Because a cron-mode mission is never an engine `Mission` record, the WI#311
`Completed`-parking lifecycle machine and the WI#317 boot writer never touch
it (structural, not behavioral, exclusion).

### 3. Re-dispatch / re-entry supervisor

`orchestrator::cron_worker` runs one serialized loop per cron-mode worker
(a mission never overlaps itself):

1. Compute the next fire from the cron schedule (`agent::routine`'s
   normalization), sleep until then or shutdown.
2. Dispatch a bounded tick job (fresh job id per link).
3. Watch the job via the `ContextManager`: terminal state resolves the tick;
   no genuine progress for `heartbeat_timeout_secs` (synthetic `[worker] …`
   keep-alives do not count — the WI#263 gate) cancels the wedged job and
   re-dispatches a fresh bounded job for the same tick, up to
   `max_reentries`.
4. Mission state flows through the worker's durable `/workspace` bind mount
   (verified host-side by the WI#324 probe); mission lifetime = the chain +
   the state, never any one process.

`ExternalWorkerManager::fire_cron_tick_now(worker, owner)` fires one tick on
demand through the same chain path (operator trigger and integration tests).

### 4. Logging / session capture

Per-link jobs already stream `task_progress` into `job_events`. The supervisor
adds structured records on top:

- `cron_tick` — on every dispatch (mission id, schedule, goal, tick/chain
  index, fired_at, tick_timeout_ms).
- `cron_reentry` — on every re-dispatch (with the failure/wedge reason).
- `cron_tick_end` — `completed` / `cancelled` / `abandoned` (+reason).

plus SSE `JobStatus` frames for live subscribers, and `info!`/`warn!`/`error!`
tracing at supervisor start, each tick fire, each re-entry, and each tick end
— answering WI#323's "dead ticker indistinguishable from idle" gap by
construction. After the fact, a mission's full session log is retrievable by
reading the per-tick jobs' `job_events` (the mission id links the chain; each
job's metadata carries `mission_id`, `tick_index`, `chain_index`).

## Verification checklist (WI#325)

- [x] A `cron`-mode worker receives a `cron.tick` event on schedule and starts
  a mission-like run — `fire_cron_tick_now` integration test asserts the
  on-wire `cron_tick` payload; the scheduled loop shares the dispatch path.
- [x] The run outlives the standard bounded deadline without being killed —
  tick jobs use `tick_timeout_ms` (per-link bound); the mission has no
  deadline and no `Completed` parking.
- [x] If the job dies mid-run, the supervisor re-dispatches a fresh job and
  the mission resumes from `/workspace` state — heartbeat-lapse integration
  test observes chain_index 0 → 1 under one mission id; `/workspace`
  durability is host-side (bind mount, verified by the WI#324 probe).
- [x] Every tick and job transition is visible in `job_events` —
  `cron_tick` / `cron_reentry` / `cron_tick_end` plus the existing per-job
  message/result events.
- [x] A `cron`-mode mission is NOT parked `Completed` by the in-process
  lifecycle machine — it is never an engine `Mission` at all.
- [~] Session audio/log capture — text session logs are covered by
  `job_events` + `/workspace` state files; audio capture is not part of this
  host-side pass (no audio path exists in external workers today).

## Files

- `ic/src/config/sandbox.rs` — `ExternalWorkerMode`, `CronWorkerConfig`,
  resolution from `[[sandbox.external_workers]]`.
- `ic/src/settings.rs` — settings fields (TOML/JSON/DB map plumbing).
- `ic/src/orchestrator/external_worker.rs` — `CronTick` payload on
  `TaskContext`; supervisor-facing manager accessors.
- `ic/src/orchestrator/cron_worker.rs` — the supervisor (schedule loop, tick
  dispatch, heartbeat watchdog, re-entry chain, event persistence).
- `ic/src/main.rs` — supervisor spawn next to the pool eviction task.
- `ic/tests/external_worker_integration.rs` — wire-shape and re-entry tests.

## Out of scope

- Migrating existing engine missions wholesale (additive; see WI#324 for the
  proxied tool path that lets workers call `memory_*`/`knowledge_*`).
- Hub-mediated cross-thread coordination (KV + advisory locks) — WI#324 phase 2.
- Audio session capture.
