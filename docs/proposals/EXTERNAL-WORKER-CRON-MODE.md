# External Worker Cron Mode (WI#325)

Status: implemented in v2.1.6.0; revised after the MR !1509 (CU-1..CU-10),
!1522 (DE-1..DE-10), and !1531 (DN/DO) reviews. WI#325 was closed out
2026-10-13 under the owner's general
[close-out instruction (#8039)](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/325#note_8039).
Audio capture was scoped out by the implementer in the
[close-out note (#8451)](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/325#note_8451);
cmc's [question (#7755)](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/325#note_7755)
was answered there, and the scope-out remains open to the owner's decision.
No audio path exists in external workers today; ask 4's text logs use
`job_events` + workspace state files, with the log-only cases documented below.
Residual review nits stay tracked by WI#375 (child of #325), plus WI#370/WI#371.
Work item: https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/325

- **Dependency disposition (GG-2):** the merged WI#324 hub/Kimi proxy supplies
  cron ticks with the proxied tool path under `cron.tool_allowlist`, covered by
  `cron_tick_tool_calls_use_cron_allowlist_not_worker_allowlist`; WI#324 remains
  open/partial pending AC1 real-worker memory round-trip evidence and AC2 real
  `knowledge_search` hits. Hub-mediated KV + advisory locks remain deferred to
  WI#324 phase 2; true multi-threaded mission coordination is not implemented.
  The long/infinite-deadline dependency is met by `cron.tick_timeout_ms`
  (1s..4h, default 4h) replacing the ordinary worker deadline for each link,
  while the supervised mission has no deadline across scheduled ticks and
  re-entries; each link still has its timeout and heartbeat watchdog.

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
retain_last_ticks = 50            # optional; default 50; cap 10_000 (WI#370 —
                                  # per-mission retention of tick rows+events;
                                  # the last-job anchor always survives)
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

### 5. Retention and list visibility (WI#370)

At the 60-second schedule floor each mission adds up to **1,440 `agent_jobs`
rows per day before reentries**, plus `job_events` (a `cron_tick` per link, a
`cron_tick_end`/`cron_reentry` per resolution). `list_jobs` sorts newest
first and truncates to 100, so cron ticks hid ordinary jobs within hours.
Two cooperating bounds:

- **Per-mission retention (`cron.retain_last_ticks`, default 50, cap
  10_000).** After every tick — and once at supervisor start, so a mission
  idle across restarts still gets swept — the supervisor deletes this
  mission's tick rows beyond the newest `retain_last_ticks`, always keeping
  the persisted last-job anchor (`cron-mission:<id>:last-job`), even when
  the anchor sits outside the window (DQ-6). Identification is exact: the
  supervisor-authored prompt-title prefix for THIS mission (the row and its
  marker are written by one INSERT), scoped to the mission worker's
  `job_mode` and the mission owner — ordinary jobs, other missions' ticks,
  and other owners' rows are never matched (pinned in the postgres lane).
  `job_events` are deleted before their row (`delete_sandbox_job_cascade`)
  because `job_events.job_id` has no `ON DELETE CASCADE; the PostgreSQL
  integration target `postgres_cron_retention` pins the FK-safe order and
  that the FK refuses an event for a missing job row. The sweep runs between
  ticks, when no mission tick can be in flight. Storage growth is therefore
  bounded at roughly `retain_last_ticks` job rows plus their events per
  mission (a few hundred rows at the default), independent of schedule
  cadence. Retention trades audit depth for boundedness: ticks beyond the
  window lose their `job_events` audit trail with the row — raise the knob
  (up to the cap) when a longer audit window matters more than the space.
- **`list_jobs` cron filter.** Tick rows (title marker) and live tick
  contexts (`cron_mode` metadata) are tagged `"cron_tick": true` and hidden
  from the tool's 100-row window unless `include_cron: true` is passed.
  Ordinary jobs stay discoverable even when retention keeps 50 ticks per
  mission.

After each sweep the anchor is re-checked: a deleted or foreign anchor row
clears the live slot and the persisted setting, so lifecycle events
(`cron_paused`/`cron_resumed`) can never be keyed to an orphan or
cross-owner job row.

## Review-driven hardening (MR !1509)

- **Workspace-lock race (CU-1)** — after a heartbeat lapse the supervisor
  cancels with the supervisor reason, then waits (bounded, 20s, shutdown-
  aware) for the old link to reach a terminal state before re-dispatching,
  and applies exponential backoff (2s base, 60s cap) between links.
- **Runaway guards (CU-2)** — config validation (above), exponential backoff,
  and a **circuit breaker**: after 3 consecutive abandoned ticks the mission
  pauses itself, persists the pause (restart-safe), and surfaces it via
  `job_events` (`cron_paused`) and SSE. WI#371: the pause flag and the
  consecutive-abandon counter share one atomic settings row
  (`cron-mission:<id>:control`), so the trip and the count land in one write;
  the counter is also persisted on every abandon and durably reset on an
  owner resume or a resolved tick, and a control-state read/parse failure at
  startup starts the mission paused (fail-closed) with the fault reported on
  the mission status instead of silently resuming.
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
  (no audio path exists in external workers today). It was scoped out by the
  implementer in close-out note #8451 under the owner's general instruction
  #8039; question #7755 was answered there, and the scope-out remains open to
  the owner's decision.

## Owner recovery API (MR !1531 DN-3)

`GET /api/workers/{worker}/cron` returns live mission status.
`POST /api/workers/{worker}/cron` accepts `{"action":"pause"}` or
`{"action":"resume"}` (also `status`). Responses contain `action` and
`mission` (`mission_id`, `worker_name`, `paused`, `tick_index`,
`consecutive_abandoned`, and WI#371's `durability_error`) plus a top-level
`durable` flag (`mission.durability_error == null`): when `durable` is false
the applied state is live but could not be persisted after bounded retries —
a restart would lose it — so the caller is told rather than shown a false
success. Mutations enqueue the command and then read status,
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
- **DN-1:** terminal wait returns shutdown to the reentry chain; the
  deterministic test delivers shutdown while a non-terminal job is awaited and
  pins that `prepare_reentry` returns `None` (the shutdown propagates out of
  the bounded wait instead of being swallowed).
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
  owned DN-5 (fail-open state reads, pause-write retries/acknowledgements, and
  durable breaker counters) — **landed for v2.1.6.0**: control-state reads
  fail closed (paused + actionable `durability_error`), pause/resume/breaker
  writes retry with bounded backoff and report exhaustion on the mission
  status (HTTP `durable: false` / tool `durability_error`) instead of
  claiming success, and the breaker counter is durable across restarts
  (fault-injection coverage: read failures, write retry/exhaustion, and
  restart after 1/2/3 abandoned ticks). WI#370 owns DN-7 and remaining DO-4 (retention,
  complete persisted outcome matrix and PostgreSQL FK CI coverage). Rollback
  note: the control state moved from the legacy `cron-mission:<id>:paused`
  key to the combined `cron-mission:<id>:control` row; the legacy key is
  still read as a fallback but never written, so a downgrade would not see
  pauses written by this build (write both keys if a downgrade must keep
  pause durability).

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
  scoped out by the implementer in close-out note #8451 under the owner's
  general instruction #8039; question #7755 was answered there, and the
  scope-out remains open to the owner's decision.
- Per-tick credential grants: `create_job` can inject named tenant secrets
  per job; tick jobs currently dispatch with an empty environment and
  `credential_grants_json = "[]"`. Follow-up if missions need tenant secrets.
- Worker-side consumption of `cron_tick` beyond the prompt (optional by
  design).
- A non-test manual-fire surface: `fire_cron_tick_now` is exercised by the
  integration tests; exposing it on a route is follow-up work if wanted.
