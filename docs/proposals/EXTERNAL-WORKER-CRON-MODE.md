# External Worker Cron Mode (WI#325)

Status: implemented in v2.1.6.0; revised after the MR !1509 (CU-1..CU-10),
!1522 (DE-1..DE-10), and !1531 (DN/DO) reviews. WI#325 was closed out
2026-10-13 on the owner's instruction ("verify and close out when possible"):
the audio-capture scope-out is accepted — no audio path exists in external
workers today, and ask 4's log half is covered by `job_events` + workspace
state files. Residual review nits stay tracked by WI#375 (child of #325),
plus WI#370/WI#371.
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
tick_timeout_ms = 14400000        # optional; host limit matching Kimi's default
                                  # 4h cap; bounded 1_000..=14_400_000
max_reentries = 3                 # optional; default 3; hard cap 16
# allow_subminute_schedule = true # explicit override for fire gaps < 60s
# project_dir = "missions/pr-digest" # optional; default "missions/<mission_id>"
# Updated bridges create this directory for cron ticks. On older images,
# pre-create it under the worker's WORKSPACE_ROOT before enabling the schedule.
```

**Workspace prerequisite:** `WORKSPACE_ROOT` must already exist and be writable.
Updated Kimi, OpenCode, OpenCode2 and Pebble bridges create missing mission
directories before executing a `cron.tick`. Creation is component-by-component,
relative to held directory descriptors: a symlink (or file/FIFO) component is
refused rather than created through, and absolute paths, `..`, `.` and `~` are
rejected up front. After creation the returned path is re-resolved with
realpath and re-checked for containment — symlink-safe creation, not a
guarantee that a racing swap cannot redirect the *returned* path to another
in-root directory (DS-1; no privilege boundary is crossed: bridge, CLI and
agent share one uid). Components must be plain directories, not pre-existing
symlinks (DQ-3). Existing mission files are preserved across ticks. Ordinary
tasks do not gain directory creation. These bridges run in Linux containers
with `/proc/self/fd` available; a missing `/proc/self/fd` is detected up front
and reported as a non-retryable environment error (DS-6). Bootstrap failures
split into permanent (`Invalid workspace path for cron tick: …` — validation,
ENOTDIR/ELOOP, EACCES/EPERM, containment) and transient (`Transient workspace
error for cron tick: …` — EMFILE/ENFILE/ENOMEM, ENOSPC/EDQUOT/EIO, swap-race
ENOENT/EINVAL), and the host re-dispatches only the transient ones (DS-2).
**Older images require the directory to pre-exist**
(for example, create `missions/pr-digest` inside their workspace mount before
enabling this example). Upgrade/recreate workers with the daemon changes;
rolling back to an older worker image requires pre-creating each mission dir.

**Semantic validation fails loudly at load**: an invalid cron block (unparseable or
sub-60s schedule without the override, empty goal, heartbeat below the floor,
re-entry count above the cap, tick timeout outside the bounded range, a
`project_dir` that is absolute/`..`-traversal/`~`-rooted, has a `..`-prefixed
or over-255-byte component, an unparseable
`timezone`, a duplicate `mission_id` — or a missing `[cron]`
block on a `mode = "cron"` worker) rejects the worker from the resolved config
with an `error!` naming the reason. A schedule typo can never boot a silently
mission-less worker (WI#321 class). This removes the entire worker, including
ordinary task-mode availability, rather than disabling only its schedule.

**Unknown keys fail earlier:** `deny_unknown_fields` rejects deserialization
of the complete `Settings` value in strict TOML/JSON parsing; it is not a
per-worker resolution error. The DB-backed loader applies settings rows via
`Settings::set`: an invalid `sandbox.external_workers` row logs a warning and
is skipped wholesale, retaining the default empty worker list. Validate the
whole worker array before storing/reloading it; one misspelled cron key can
therefore remove every worker supplied by that row (DO-6).

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
cron block's own `tick_timeout_ms` — **defaulting to a host-imposed 4h limit
matching Kimi's default cap**, i.e. a tick link outlives the ordinary deadline by
default — plus the heartbeat watchdog, which bounds *silence*, not work. The
mission itself has no deadline: it fires again at the next scheduled tick
regardless of how any one tick resolved. Because a cron-mode mission is never
an engine `Mission` record, the WI#311 `Completed`-parking lifecycle machine
and the WI#317 boot writer never touch it (structural, not behavioral,
exclusion).

The host validates every worker's tick links to 1s..4h. Kimi additionally
enforces `KIMI_MAX_TIMEOUT_MS` (default 4h; configurable from 1s to 24h);
OpenCode/OpenCode2/Pebble have no equivalent wire-protocol maximum. Set
`cron.tick_timeout_ms` at or below any lower configured Kimi cap: the daemon
cannot discover that environment value, so a default 4h tick is otherwise
rejected. A worker-side protocol-validation rejection
(`timeout_ms must be…`, unresolvable/escaping `project_dir` — kimi's raw
realpathSync failure (Node reports the failing syscall as `lstat`:
`ENOENT: no such file or directory, lstat …`) and pebble's wrapped wording
are both classified) is treated
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
- `cron_paused` — on circuit-breaker trips and owner pause; `cron_resumed`
  — on owner resume (the breaker never resumes itself). Both are keyed to
  the mission's most recent tick job, persisted across restart; a mission that has
  never ticked (or whose last job was deleted before restart) has no job row,
  so the tracing line is the record then.

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
  (`pause`/`resume`/`status`, owner-gated with AskEachTime and an explicit
  autonomous denylist, stricter than `cancel_job`). `cancel_job` ends only the current link;
  `cron_mission pause` stops future fires and pending re-entries, and
  survives restart. The tool is v1-only in the engine bridge
  (`is_v1_only_tool`); all deployments can use the owner-authenticated HTTP
  control route below, including Engine V2 tenants.
- **Memory growth (CU-2)** — terminal tick contexts are evicted from the
  `ContextManager` once their events are persisted; the durable record lives
  in `agent_jobs` + `job_events` (`job_status` falls back to the DB).
  Durable rows currently have no automatic retention. A per-minute mission
  adds up to 1,440 jobs/day plus events and reentries; `list_jobs`' newest-100
  limit can hide ordinary jobs. Retention/discovery is tracked by
  [WI#370](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/370).
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
  chain link (bounded, 5×) and, on exhaustion, log `skipped_busy` and wait
  for the next scheduled fire **without counting toward the circuit
  breaker**. Busy worker rejections have a job row and persist a
  `cron_tick_end`; pre-dispatch capacity failures are log-only. Ordinary
  tasks contending with a mission no longer pause it.
- **CU-1 residue / project_dir** — `cron.project_dir` now **defaults to
  `missions/<mission_id>`**, so a mission never shares the worker root with
  ordinary dispatches unless the operator opts into that, and the value is
  host-validated at load (relative, no `..`, no `~`, no `.` segments, no NUL).
  Updated bridges bootstrap missing dirs; older images require the directory
  to pre-exist. Worker preflight errors still use textual classification. Honest
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
  markers cover both shipped bridges (kimi/opencode's raw Node
  realpathSync failure, which names the `lstat` syscall —
  `ENOENT: no such file or directory, lstat …` —
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
  payload. Missing-directory startup is additionally covered through the real
  Kimi bridge with a mock CLI, Pebble bridge dispatch, and all four workspace
  helpers. The bridge suites gate the image builds (Kimi `npm test`, both
  OpenCode `path_expand_test.ts`, Pebble `cargo test --locked`); no MR CI lane
  runs them host-side, so between image builds they remain author-run. Production model execution and older images are not covered by
  those tests; older images need the documented pre-creation step.
- [x] The run outlives the standard bounded deadline without being killed —
  per-link `tick_timeout_ms` defaults to the host's 4h limit (longer than any
  shipped worker's ordinary default), and the mission itself has no deadline:
  the supervisor holds the lease and chains fresh links. No `Completed`
  parking. (A single link cannot exceed the host limit or Kimi's configured cap, documented
  above.)
- [x] If the job dies mid-run, the supervisor re-dispatches a fresh job and
  the mission resumes from workspace state — `cron_supervisor_redispatches_wedged_tick_job`
  (lapse path), `cron_reentry_on_failed_link` (failure path),
  `cron_busy_rejection_retries_without_spending_reentry_budget` (lock-enforcing
  mock). Cross-job workspace durability is a property of the worker bind mount
  recorded in the WI's evidence section; the chain itself is
  integration-tested.
- [~] Tick/job transitions are persisted where a job row exists —
  `cron_tick_events_persist_to_job_events` asserts persisted `cron_tick` and
  `cron_tick_end` rows against a real store; owner pause/resume persist
  `cron_paused`/`cron_resumed` rows keyed to the mission's most recent tick
  job, including resume after restart. Pre-dispatch capacity failures and
  lifecycle controls before any job exists are log-only; full store-backed
  outcome/FK coverage is tracked by WI#370.
- [x] A `cron`-mode mission is NOT parked `Completed` by the in-process
  lifecycle machine — it is never an engine `Mission` at all.
- [~] Session audio/log capture — text session logs are covered by
  `job_events` + workspace state files; **audio capture is not implemented**
   (no audio path exists in external workers today). The WI owner accepted
   this scope-out at close-out (2026-10-13, "verify and close out when
   possible"); the audio half is future work if a worker ever gains an
   audio path.

## Owner recovery API (MR !1531 DN-3)

`GET /api/workers/{worker}/cron` returns live mission status.
`POST /api/workers/{worker}/cron` accepts `{"action":"pause"}` or
`{"action":"resume"}` (also `status`). Responses contain `action` and
`mission` (`mission_id`, `worker_name`, `paused`, `tick_index`,
`consecutive_abandoned`). Mutations enqueue the command and then read status,
so a 200 reflects the applied command. If the command was accepted by the
supervisor queue but not confirmed within the 10-second bound, the response is
`202 Accepted` with `queued: true` — re-query status before retrying
(pause/resume are idempotent); a 504 means the command never entered the
queue (DQ-4). Lifecycle audit rows record `origin` (`owner_api` for this
route, `owner_tool` for the `cron_mission` tool) (DQ-5).

Both routes use the existing gateway bearer authentication and require the
authenticated user to match the configured daemon owner before any lookup.
No body-supplied identity or query-string bearer is accepted. Existing gateway
CORS and body limits apply; the handler bounds supervisor waits to 10 seconds.
This route is independent of Engine V1/V2 and needs no LLM, UI, or database edit:

```sh
curl --fail -H "Authorization: Bearer $GATEWAY_AUTH_TOKEN" \
  -H 'Content-Type: application/json' -d '{"action":"resume"}' \
  "$GATEWAY_URL/api/workers/kimi/cron"
```

## MR !1531 disposition (review #7569)

- **DN-2 / DO-1:** confined cron-directory bootstrap in all four bridges;
  missing-default-dir and traversal/symlink regressions, plus upgrade prerequisites above.
- **DN-3:** owner-authenticated recovery API, tested with a persisted pause,
  real auth middleware, non-owner/missing/query-token rejection, and ordered status.
- **DN-1:** terminal wait returns shutdown to the reentry chain; deterministic
  test delivers shutdown while awaiting a non-terminal job and requires chain exit.
- **DN-4:** persist/reload the last real job id for post-restart audit events;
  store test reads owner pause and post-restart resume back from `job_events`.
- **DN-6:** only dispatched/persisted links can anchor pause events. A paused
  capacity retry exits without creating a phantom job/event; failed job-row
  persistence prevents dispatch. A store-backed regression checks no orphan row.
- **DN-8:** only the exact Node missing-path shape (`ENOENT: no such file or
  directory, lstat` — Node's JS `realpathSync` reports the failing syscall as
  `lstat`, not `realpath`; corrected in MR !1531 DQ-2 after the original
  `realpath` wording matched no shipped bridge) is non-retryable; arbitrary
  user-task mentions retry.
- **DO-2 / DO-3:** cap-boundary unit test and positive abandonment assertions;
  live-supervisor tool pause/status/resume success and JSON-shape coverage.
- **DO-7:** queued-command precedence over shutdown is unit-tested; the
  original restart test alone did not pin the select ordering.
- **DN-9 / DN-10 / DO-5 / DO-6 / DO-8..DO-11:** timeout/config/claim corrections;
  closed command channels cancel the active tick; durable tick-index docs and
  test-only manual-fire wording corrected. A mission can still consume one
  parallel slot per active link for up to 4h × (1 + max_reentries) per tick.
- **Deferred as permitted by the review:** [WI#371](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/371)
  owns DN-5 (fail-open state reads, pause-write retries/acknowledgements, and
  durable breaker counters); WI#370 owns DN-7 and remaining DO-4 (retention,
  complete persisted outcome matrix and PostgreSQL FK CI coverage). Pause writes
  are currently best-effort, and the abandon counter currently resets on restart.

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
- `ic/src/tools/permissions.rs`, `ic/src/tools/builtin/mod.rs` — permission seed/export.
- `ic/src/bridge/effect_adapter.rs` — V1-only tool fence.
- `ic/src/agent/routine_engine.rs` — autonomous denylist regression.
- `ic/src/error.rs`, `ic/src/config/mod.rs`, `ic/src/orchestrator/mod.rs` — errors/exports.
- `ic/src/channels/web/handlers/cron_workers.rs`, `handlers/mod.rs`, `server.rs`
  — owner HTTP control and authenticated route registration.
- All four worker bridges and their `workspace_path` helpers — cron-only
  directory creation; Pebble protocol and TS runtime context carry `cron_tick`.
- `ic/src/main.rs` — supervisor spawn next to the pool eviction task.
- `ic/tests/external_worker_integration.rs` — wire-shape, scheduled-loop,
  shutdown, re-entry, busy-lock, abandonment, breaker, owner-gate, command
  starvation, eviction, restart-persistence, and store tests.

## Out of scope

- Migrating existing engine missions wholesale (additive; see WI#324 for the
  proxied tool path that lets workers call `memory_*`/`knowledge_*`).
- Hub-mediated cross-thread coordination (KV + advisory locks) — WI#324 phase 2.
- Audio session capture (no audio path exists in external workers today);
  the scope-out was accepted by the WI owner at close-out (2026-10-13).
- Per-tick credential grants: `create_job` can inject named tenant secrets
  per job; tick jobs currently dispatch with an empty environment and
  `credential_grants_json = "[]"`. Follow-up if missions need tenant secrets.
- Worker-side consumption of `cron_tick` beyond the prompt (optional by
  design).
- A non-test manual-fire surface: `fire_cron_tick_now` is exercised by the
  integration tests; exposing it on a route is follow-up work if wanted.
