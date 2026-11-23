# WI #191 — Performance-first EMA estimation integration plan

Status: **tool/approval slice shipped in v2.1.4.0; Phase A diagnostics, category
gating and the evaluation trace (including a per-category `gate_pass` verdict)
are implemented; Phase B1–B4 LLM
logical-call timing and gate-wait analytics are implemented (owner-only
diagnostics, no prompt-facing use yet); Phase C1–C4 bounded local-job and
mission-run estimates are implemented with sandbox/external jobs explicitly
excluded. The held-out tenant evaluation, hint rollout and Phase D remain in
progress; their operator procedure is the A4/A5 runbook below**. [Work item #191](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/191)
closed that initial scope and is kept as the historical record of the
completed wiring. [Work item #318](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/318)
owns the successor phases and is the authoritative backlog and acceptance gate.

Implementation contract: [`ic/src/estimation/README.md`](../../ic/src/estimation/README.md).
The initial integration covers slices 0/1 and the approval-description portion
of slice 3: per-user registry-owned learning, all three tool execution paths,
five-sample warmup, and frozen v1/v2 approval hints. Recording defaults to
observation-only; `LUNARWING_TOOL_ESTIMATES=1` enables hints and `=0` disables
the cache. It uses existing description
fields rather than adding estimate DTOs across channel protocols. Slice 2 (LLM
logical-call timing) is now implemented as a daemon-wide, in-memory,
duration-only cache at the provider-chain boundary, surfaced by the owner-only
`GET /api/estimates/llm`; retries, failover, cache hits and cancellations are
counted separately and never train the EMA. It is not yet used in any estimate
shown to a user. Gate-wait analytics (B4) are implemented separately in
`ic/src/gate/wait_analytics.rs` (owner-only `GET /api/estimates/gate-waits`),
and Phase C job/mission-run views are now implemented: the held-out workload
evaluation (A4), hint rollout (A5) and the conditional durable cache (D1)
remain follow-ups; tool calls within those
jobs are covered by this slice. Costs are only learned when actually reported,
and are not displayed. See the
[goal-10 evidence record](../ops/history/v2.1.4.0_GOAL10_EMA_VALIDATION.md) for checks run.

Original plan reviewed against `wire-in-ema` at `05ddcc5cd` (2026-09-26).
The original #191 prerequisite was satisfied by v2.1.4.0; it is no longer a
reason to defer implementation. Remaining work follows #318: Phase A covers
diagnostics, per-category gating, held-out evaluation and tenant rollout of the
shipped tool hints. Phase B adds LLM logical-call timing and gate-wait
analytics, Phase C bounded job and mission-run estimates, and Phase D a durable
aggregate cache only if the evidence justifies it. Phase A documentation and
decisions are updated by SweetieBot (autonomous) under #318 on 2026-11-20
without changing runtime behavior. Read-only diagnostics and category gating
must precede the real-tenant held-out evaluation, and only categories that pass
that evaluation may receive hints. Treat this document as design context, not
as proof that any follow-up phase has shipped or that #318's closing acceptance
criteria are met.

## Goal and non-goals

Help an operator answer **"How long is this likely to take?"**, and help us
identify slow paths without making the application itself slower. Prioritize
tool execution, LLM calls, approvals, and *bounded* jobs/missions. Cost is
optional supporting data only when an individual call's charge is known;
the daily LLM budget is not a tool cost. Keep all estimates advisory: they
must not change approvals, timeouts, budgets, dispatch, or provider selection.
Do not present `ValueEstimator`'s bid/margin calculation as task value.
This plan improves **runtime visibility and forecast accuracy**; EMA by itself
does not accelerate tools or models. Optimization based on the measured
bottlenecks is separate work.

These are different durations and must never be pooled into one EMA:

| Clock | Start → stop | Initial consumer |
|---|---|---|
| Tool runtime | Immediately before executing a validated tool → completion, failure or timeout | Approval card; bounded step estimate |
| LLM attempt | Request/attempt begins → terminal response or error, including streaming until completion | Bounded job estimate; latency diagnostics |
| Approval wait | Gate created → approved/denied/expired | Observed wait only, not an execution ETA |
| Job elapsed | Worker starts → terminal state | Completed-run calibration, status |
| Mission run elapsed | One dispatched run starts → terminal state | Historical run estimates, not mission lifetime |

An ETA never includes future unknown steps. A single tool estimate is **not**
an estimate of an open-ended conversation, mission, or complete coding job.
An EMA describes a recent typical outcome, not a p90, deadline, SLA, or an
assertion about how quickly a particular pending approval will be handled.

## Existing seams and gaps

- ~~`ic/src/estimation/mod.rs:45-117` exposes `Estimator::estimate_job` and
  `record_actuals`, with **no production callers**.~~ **Removed under #318
  (C5):** the legacy job facade (`estimate_job`, `record_actuals`,
  `JobEstimate`, `ToolEstimate`) is deleted, so `estimate_operation` /
  `record_operation_actuals` are the only estimate API. `learner.rs` keeps
  an in-memory per-category EMA (`alpha=0.1`, `min_samples=5`).
- `ic/src/estimation/cost.rs:16-45` and `time.rs:12-38` know only `http`,
  `echo`, `time`, `json`; unknown tools get a nominal $0.001 / 5 seconds.
  Nothing in production consumes these standalone estimators after C5 removed
  the legacy job facade; keep them unreferenced or delete in a later cleanup
  rather than reintroducing a second estimate path. The LLM price
  in `cost.rs` is a flat approximation; production LLM accounting has its
  own model/provider rates. `value.rs:23-29` ignores the description.
- `ic/src/tools/tool.rs:210-255` gives `ToolOutput` optional cost and a
  duration, but the common tool pipeline times execution itself and returns
  only serialized result (`ic/src/tools/execute.rs:63-115`). Most outputs use
  the constructors with `cost: None`; absence means **unknown**, not free.
  There are also unused `Tool::estimated_duration` / `estimated_cost` hooks
  (`ic/src/tools/tool.rs:330-338`), with some tool-specific overrides; query
  these as provisional baselines where available instead of discarding them.
- Chat, container execution, scheduler subtasks, and the engine-v2 adapter use
  the common execution function:
  `ic/src/agent/dispatcher.rs:1100-1115`,
  `ic/src/bridge/effect_adapter.rs:963-970`. **Local job-worker execution**
  (`ic/src/worker/job.rs:490-760`) and **lightweight routines**
  (`ic/src/agent/routine_engine.rs:2062-2144`) instead have independent
  validate/timeout/execute paths. Instrumenting the common function alone
  would miss them. The v2 adapter also times the surrounding gate/hook path
  separately (`effect_adapter.rs:708,972`); do not submit that time again as
  the tool execution sample. Some mission calls short-circuit tool execution.
- `ic/src/agent/cost_guard.rs:175-217` calculates an individual LLM
  call's price; `ic/src/agent/dispatcher.rs:551-576` receives that return
  value. Its **daily** total cannot be joined to a tool invocation.
  The v2 host bridge maps usage with `cost_usd: 0.0`
  (`ic/src/bridge/llm_adapter.rs:131-144`): this is **not** evidence a call
  was free. Job and container LLM delegates do not share v1 chat's
  `CostGuard::record_llm_call`; do not promise priced coverage for them.
- Engine-v2 gate state is persisted (`ic/src/gate/pending.rs:26-64`);
  its read-only view is `PendingGateView` (`:82-108`). Creation broadcasts
  `GateRequired` (`ic/src/bridge/router.rs:242-310`); the web UI can also
  recover it via the user-scoped pending-gates endpoint
  (`ic/src/bridge/router.rs:1971-1994`). The card is rendered in
  `ic/src/channels/web/static/app.js:1804-1848`.
  Legacy v1 instead stores `PendingApproval` on the thread and publishes
  `StatusUpdate::ApprovalNeeded` (`ic/src/agent/dispatcher.rs:1074-1090`,
  `ic/src/agent/thread_ops.rs:794-825`): the engine-v2 pending-gate store
  alone does not cover all approvals.
- There is another, separate engine-v2 `ReliabilityTracker` EMA at
  `ic/crates/lunarwing_engine/src/reliability.rs:1-112`. It is not a
  substitute for the `ic/src/estimation/` learner; do not introduce a second
  parallel writer or combine reliability and ETA statistics implicitly.
  **Resolved under #318 (E1):** it had no consumers anywhere in `ic/`, so it
  was deleted rather than wired as a second EMA writer. The engine crate no
  longer exports `ReliabilityTracker`.
- Existing `ic/src/observability/traits.rs` defines tool and LLM start/end
  events, but its factory and record path have no production callers. Assess
  wiring that observer as a lightweight, optional carrier rather than adding
  another telemetry bus; its current events have no per-invocation correlation
  ID, cost, or timeout classification, so they are not yet sufficient to
  train the learner safely. Do not let observer failure affect execution.
- `JobContext` already has optional `estimated_duration` / `estimated_cost`
  (`ic/src/context/state.rs:181-183`), and `agent_jobs` has estimated and
  actual duration columns (`ic/migrations/V1__initial.sql:36-65`). Neither
  is filled by the dormant estimator. Existing `ActionPlan.estimated_time_secs`
  (`ic/src/llm/reasoning.rs:326-337`) is only used when planning is enabled;
  that option defaults off (`ic/src/config/agent.rs:202`).

## Architecture and data contract

### 1. Rework the estimator before exposing it

Keep the existing `Estimator` facade and `EstimationLearner`, but add a
performance-oriented single-operation API (a job aggregate can compose it).
A future job view must not reuse the deleted `JobEstimate`: its `value`
was not a useful performance metric and its `tool_breakdown` was unadjusted
(`ic/src/estimation/mod.rs` before C5). Introduce a separate optional public
runtime-estimate DTO; the sum of displayed components must have the same
semantics as the displayed aggregate.
Suggested internal types, subject to existing style and trait constraints:

```text
EstimateKey = Tool(canonical_tool_name)
            | Llm(model_or_provider_and_model)
            | Job(bounded_job_kind)
            | MissionRun(bounded_run_kind)
Estimate = { expected_runtime: Option<Duration>, confidence/sample_count,
             optional_cost: Option<Decimal>, basis: cold_start|learned }
Observation = { key, base_runtime, measured_runtime, outcome,
                optional_base_cost, optional_actual_cost }
```

`EstimateKey` must be namespaced: a tool named `general` must not share the
model for an LLM or a job. Canonicalize tool aliases at the registry boundary,
without using raw parameters, prompt text, user IDs or secret-bearing URLs as
keys. Scope estimator state to the owning daemon/tenant; avoid cross-user
leakage in any public introspection. Do not key by every arbitrary prompt,
job title, command, or model response (unbounded cardinality).

For tool execution time, obtain the **measured elapsed time** around the real
invocation in `tools/execute.rs`, not `ToolOutput.duration` blindly: individual
tools may report an internal duration of zero or only a sub-operation. Record
reported duration separately only if its meaning is documented. Timing begins
after validation and before `tool.execute`; track timeout and real execution
errors as outcomes. Do not train tool latency on validation failures, missing
tools, approval waits, auth gates, or cancellations where execution never
started. Treat failures/timeouts separately from successful latency samples;
record outcome counts but do not make a time-capped timeout look like a typical
successful runtime. The successful output's `cost: Option<Decimal>` is the
only candidate *tool-level* charged cost: `None` must not train a zero-cost
ratio. Preserve existing result serialization and sanitization behavior.
If tool execution succeeds but output serialization fails, classify the
execution observation separately from its delivery error.

**Avoid double correction.** At invocation start capture the *unadjusted*
baseline and the then-current adjusted prediction. The learner's factor target
is `actual / unadjusted_baseline`; compute forecast-error statistics against
the frozen adjusted prediction. Feeding `actual / adjusted_prediction` into
the existing absolute-factor EMA makes it converge to the wrong target.
On the first observation initialize the measured factor from the bounded
ratio; subsequently apply the EMA, gated from display until the minimum
sample threshold. Maintain **separate valid sample counts and error tracking**
for runtime and cost. Where no credible positive baseline exists, learn a
duration from observed successful runs or withhold the estimate; do not
blindly promote the unknown-tool 5-second fallback to a trusted forecast.

Before activating live observations, bound positive ratios and durations,
guard `NaN`/infinity, zero and negative costs, `Duration::from_secs_f64`
overflow, `Decimal` conversion failures, and `sample_count` overflow. Never
silently return factor 1.0 on a failed cost-factor conversion. Ordinary
fractional factors (for example 1.13) are representable; the earlier draft's
implication that they routinely fail conversion was incorrect and is covered
by a regression test. Make alpha/min_samples invariants explicit;
retain defaults until offline calibration demonstrates better settings.
Avoid a global lock across tool/LLM awaits: capture prediction with a brief
read, record with a brief write. Bound category count and evict cold keys;
keep statistical snapshots small and avoid high-cardinality metrics labels.

### 2. Instrument shared tool execution

Add an optional shared estimator handle at the composition root and pass it
to the shared tool pipeline (or provide a narrow observation hook owned by
that pipeline). Take the estimate immediately before execution, then record
one terminal observation per actual execution. Prefer a non-breaking
wrapper/internal return carrying timing, outcome and optional cost rather
than duplicating timing at every caller or changing the string returned to
the model. Common-pipeline callers cover v1 chat, container execution,
scheduler subtasks and engine-v2. To reach **all** relevant tools, migrate
or add the *same* observer to the local job worker's distinct execution
(`ic/src/worker/job.rs:490-760`) and the routine engine's distinct execution
(`ic/src/agent/routine_engine.rs:2062-2144`). A wholesale migration to the
shared executor is optional, not a prerequisite: preserve the job worker's
`ActionRecord`/`job_actions` persistence, parallel behavior and the routine
allowlist/timeout contracts. Audit additional direct `Tool::execute` paths.
An interrupted/dropped execution future may never reach the normal return
path: either use a lightweight cancellation-safe finalizer or explicitly
count these as missing observations, and verify no canceled invocation is
mistaken for a successful fast call. Repeated resume of a gate must not
record the preflight and resumed execution as two tool runs.
The v2 adapter's mission-call short circuit (`effect_adapter.rs:728-735`)
needs its own explicit classification, not automatic double-instrumentation.
Errors in estimation must never turn a successful tool into a failed tool.
Existing `ToolCallStart/End` observer events may carry this observation if
they can remain allocation-light and cancellation-safe; do not run an
observer and a separate estimator hook over the same event.

### 3. Instrument LLM attempts without inventing prices

The v1 provider chain is assembled in `ic/src/llm/mod.rs:329-514`; the v2
`LlmBridgeAdapter` uses that same chain (`ic/src/bridge/llm_adapter.rs:19-43`).
Place an optional monotonic-timer wrapper at the **verified provider-chain
boundary**, not independently in each chat/job/container delegate. Specify
the wrapper's position relative to retry, failover, response cache, timeout
and streaming wrappers. A timer outside retries measures one logical call
including backoff; an inner timer measures one network attempt. Choose one
primary training definition (prefer completed logical-call latency for the
operator-facing estimate) and track attempt/retry/TTFC separately where
available. Never mix these clocks under the same key, count an attempt twice,
or claim a cached answer has network latency. Streaming duration ends at the
terminal event; test mid-stream cancel/EOF separately. Existing
`ic/src/llm/stream_telemetry.rs` has TTFC but not general total-call
duration; its existing file snapshot is not an EMA coefficient store.

Use active model price and observed token usage where already available.
`CostGuard::record_llm_call` supplies v1 **chat** actual charge; job delegates,
container calls, and v2 lack equivalent per-call charged accounting in the
paths reviewed. v2 currently writes `cost_usd: 0.0` in its usage adapter:
interpret that as unavailable, not zero. Train cost only on calls with
usable per-call usage and pricing; never subtract adjacent daily totals
under concurrent load. Base token/output estimates are speculative until
validated; duration-only LLM estimates are preferable to misleading cost.
Guard against cached responses, partial streams, failures without usage,
retry attempts and provider switches contaminating a successful-call EMA.
No change to the existing cost enforcement path.

## Consumer surfaces

### A. Approvals — first user-visible release

Estimate the **one proposed tool execution**, before or as an approval gate
is created. Use an optional, version-tolerant DTO in persisted pending-gate
state and `PendingGateView`, plus live `GateRequired`, so reconnect and fresh
SSE yield the same value. Keep the estimate additive: old persisted gates
without it must deserialize, and the decision token/request ID, expiry,
resume semantics and redacted parameter representation must not change.
The engine-v2 construction paths include normal turn pauses
(`ic/src/bridge/router.rs:377-439`), re-pauses after an approved action
(`:737-776`) and auth fallback (`ic/src/bridge/router/outcome.rs:271-302`);
only real single-tool approval gates receive an execution estimate. Leave
authentication and external-wait gates without a tool ETA. The shared
insertion path delivers both `GateRequired` SSE and `StatusUpdate::ApprovalNeeded`
(`ic/src/bridge/router.rs:242-375`); keep the two payloads consistent.
Separately add optional estimate display to legacy v1 `PendingApproval` /
`ApprovalNeeded` and history recovery instead of silently limiting this
feature to engine-v2 users. Extend the relevant gateway event DTOs in
`ic/src/channels/web/types.rs` additively.
If recomputed for an old gate, label it as a current estimate, not a frozen
prediction. Include source (`baseline` / `learned`), valid sample count and
an honest confidence label. Only render a duration when there is a bounded
single-tool operation; optional cost only with attributable, reliable data.
Present e.g. "Tool runtime: roughly 8 s (based on 24 recent runs)" rather
than "Approval will finish in 8 s." Never interpolate parameters into a
label or create HTML from untrusted names; render with safe text nodes.
Review `StatusUpdate::ApprovalNeeded` delivery separately for non-web
channels; add compact human-readable duration only if its adapters can
preserve redaction and existing message budgets. No added gate or bypass.
That includes XMPP DMs (`ic/src/channels/xmpp/mod.rs:1193-1214`), Signal,
REPL and WASM channel statuses; never broadcast approval details into shared
channels. Preserve `PendingGateStore::take_verified` channel/request-ID and
expiry checks (`ic/src/gate/store.rs:244-300`) verbatim. Gate expiry is a
fail-closed security timer, **not** a statistical estimate.
For performance diagnosis, separately observe gate wait as
`resolution_at - created_at` on approved/denied/expired outcomes, labeled
by outcome and channel family, never by parameters. The pending-gate store
already timestamps creation (`ic/src/gate/pending.rs:54-57`); resolution and
expiry have distinct paths (`ic/src/bridge/router.rs:2495-2798,5377-5404`).
Keep this statistic separate from tool/LLM runtime; optional wait-history
display is not a promise about when a human will approve a future request.

**Implemented under #318 (B4).** `ic/src/gate/wait_analytics.rs` accumulates,
in memory and bounded to 24 series, the wait per `(channel family, outcome)`.
Approved and denied gates are observed at their single resolution entry point;
expired gates use `expires_at - created_at`, the fail-closed deadline, so lazy
cleanup latency is not reported as user wait. Cancellations and credential
hand-offs are excluded. The owner-only `GET /api/estimates/gate-waits` returns
the aggregate with a note that it is not a response-time promise. Nothing here
is read by expiry, approval, scheduling or cost accounting.

### B. Bounded jobs and mission runs — second user-visible release

Separate **current elapsed** from **predicted remaining**. When a job has
an explicit plan/finite known step list, sum serial estimates and model
concurrent steps by critical path, not sum; make tool/LLM call counts explicit
assumptions. Once running, show completed-step actual time plus estimates
only for *known* remaining steps. Where work is iterative or open-ended,
show historical duration of the same bounded job kind and elapsed time,
not a made-up remaining ETA. Reuse one DTO across creation/status/history
where the same scope is available. Record terminal outcomes exactly once,
including cancellation/timeout metadata but train successful-run duration
only on comparable completions. Mission **run** duration can be estimated;
recurring mission lifetime cannot. Do not pass estimates into LLM prompts or
scheduling heuristics in the first releases.

Concretely, v1 `create_job` has **three distinct modes**, not one generic
completion path: local `Scheduler::dispatch_job_inner`
(`ic/src/agent/scheduler.rs:170-237`), sandbox/container
(`ic/src/tools/builtin/job.rs:557-860`) and external worker (`:872+`).
Local jobs persist via `save_job` before scheduling (`scheduler.rs:225-235`),
and can store a defensible optional estimate in `JobContext` before that
save. Sandbox jobs use a separate `SandboxJobRecord`, live job manager,
crash watcher and monitor; external workers have their own status/result
contract. Define a per-mode start/completion mapping and idempotent terminal
observation before claiming full job coverage; never assign a local-worker
estimate to a sandbox or external task solely because both use `create_job`.
Expose a provisional historical estimate at `job_status` and gateway job
detail/summary only where that mode and category have comparable completions
(`ic/src/tools/builtin/job.rs:1767+`,
`ic/src/channels/web/handlers/jobs.rs:18-137`). Tool/LLM step estimates
should be composed into a whole-job ETA **only** if a finite plan is truly
known; a max-iteration limit or 30-second worker timeout is not a plan.

Engine-v2 mission execution persists mission definitions in workspace JSON,
not the v1 SQL jobs table (`ic/src/bridge/store_adapter.rs:703-949`). A fire
reserves and spawns a mission thread (`ic/crates/lunarwing_engine/src/runtime/mission.rs:905-1115`);
terminal outcomes flow through its watcher (`:2107-2194`) and notification
path (`ic/src/bridge/router.rs:1751-1783,3871-3931`). Build a read-only
per-run observation there, keyed by a bounded mission *kind* only when the
kind is meaningful. Keep `lunarwing_engine` independent of `ic`'s
`estimation` module: consume existing engine outcome events in the host
bridge, or add a small engine-neutral observation callback if they lack
necessary identity/timing. Never add a synchronous DB write on the mission fire
path or treat repeated cron/event fires, retries, gate pauses and the
mission's whole lifetime as one run. If durability is justified, use a
separately designed workspace-compatible aggregate or append-only metric,
not a fake `agent_jobs` foreign key. The web gateway provides authenticated
read-only mission list, summary and detail endpoints at `GET /api/missions`,
`GET /api/missions/summary` and `GET /api/missions/{id}`
(`ic/src/channels/web/server.rs`); the detail view is the available
current-run surface. Extend its run view additively for any advisory
historical estimate rather than proposing an unrelated viewer; do not claim
general mission-definition CRUD or alter mission semantics as part of this
plan. The webhook ingress remains separate.

**Implemented under #318 (C1–C4).** `Scheduler::dispatch_job_inner` freezes a
historical duration into `JobContext.estimated_duration` before the first
persist, keyed by one of two non-sensitive job kinds under a private per-user
learner key: `routine` for full-job routine runs (marked
`estimate_kind = "routine"` in the job metadata by the routine engine) and
`user` for every other local dispatch, because the two are not comparable
completions. `Worker::run` records exactly one terminal observation
after one execution loop: only `Completed` trains the duration model;
`Failed`/`Cancelled` are counted separately; `Stuck` records nothing.
`job_status`, `GET /api/jobs` and `GET /api/jobs/{id}` report measured elapsed
separately from the frozen historical duration and report remaining time as
`unknown`, since a max-iteration limit or worker timeout is not a finite plan.
Sandbox/container and external-worker jobs are explicitly excluded: they keep
`estimated_duration` unset and never receive a local-job estimate.
`GET /api/missions/{id}` derives advisory run timing from the mission's own
loaded threads on read: the valid run count (finished successfully; a run that
went on to `Failed` does not count), the newest running run's elapsed, and,
only once five valid runs exist (A6.2), the median and last run's active
duration, read from each thread's state-change events with `Waiting` and
`Suspended` stretches subtracted; `lunarwing_engine` is untouched, there is
no second EMA writer, no extra store read and no fire-path write. Job and
mission timing follow the same opt-in display mode as tool hints; recording
continues in observe mode. `ic/src/estimation/jobs.rs` holds the bounded
counters and timing view; tests cover warmup withholding, failed-run
exclusion, disabled mode, user isolation, display gating and the mission-run
median.

### C. Diagnostics — optional read-only surface

Read-only diagnostics are a **required Phase A gate**, not an optional extra.
Show only non-sensitive category keys, sample counts, current and error EMAs,
confidence labels, and eligible/withheld/clipped/evicted counters. The shipped
models are per-user, so gateway access must use existing authentication and
expose only the caller's own models. Any cross-user operator view must be
aggregate-only. Never expose raw arguments, prompts, tokens, credentials or
cross-tenant data. No synchronous DB writes belong on the hot path.

**Implemented under #318 (A2/A3/A4 recording).** `GET /api/estimates` returns
the caller's own categories and active category policy.
`GET /api/estimates/counters` is owner-only and reports unlabeled registry-wide
totals, with eligibility counted at execution start even in observe mode.
`GET /api/estimates/evaluation` is owner-only and compares errors on the same
retained calls, with separate warmup and early cold-start coverage.
`LUNARWING_TOOL_ESTIMATES_CATEGORIES` gates display per canonical tool name
while recording continues. `LUNARWING_TOOL_ESTIMATES_TRACE` enables the
bounded, parameter-free records behind the evaluation route. Each category is
compared against two named baselines: `declared_baseline` from
`Tool::estimated_duration` where a tool supplies one (only `http` today), and
`comparison_baseline`, the mean of the model's own first five valid completed
runs, frozen before each call. The A4 gate is at least 10% lower median
absolute error than `comparison_baseline`; the declared comparison is reported
alongside rather than being required for a category to be judged. A missing or
empty comparison is not a pass. Running the evaluation on a real tenant and
enabling hints for passing categories remain A4/A5 operator steps; this code
does not change any approval, scheduling or cost behaviour.

## A4/A5 held-out evaluation and rollout runbook

This is the operator procedure for the two remaining Phase A steps. It needs a
tenant that runs a build containing the A4 trace and a window of that tenant's
**normal** workload; operator-generated or synthetic traffic is not evidence
for this gate.

1. **Start one observation window.** Edit the tenant's
   `env/lunarwing.env`: leave `LUNARWING_TOOL_ESTIMATES` unset or `observe` so
   no hints are shown. Use `LUNARWING_TOOL_ESTIMATES_TRACE=1` for 2,048 records or
   set `LUNARWING_TOOL_ESTIMATES_TRACE=<capacity>` (16–8,192). Restart with
   `sudo lunarwing-mt-admin.sh restart-tenant <tenant>`; the setting is read
   once at startup. Record the tenant, the deployed commit, and the window
   start and end. A restart resets both the models and the trace ring, so one
   window is one process lifetime — never pool samples across restarts.
2. **Collect.** As the owner, read
   `GET http://127.0.0.1:<gateway_port>/api/estimates/evaluation` (plus
   `/counters` and `/llm` for context) with the tenant's owner bearer token.
   Do not paste the token into the evidence doc or a shell history that is
   committed.
3. **Judge each category.** Read `categories[].gate_pass`: `true` means the
   category cleared the 10% comparison-baseline gate, `false` means it was
   judged but fell short of 10%, and `null` means it could not be judged (no
   paired sample or an undefined improvement) and is not a pass. Report
   `comparison_baseline` errors in seconds (`median_abs_error_*_ms / 1000`)
   alongside `coverage`, `samples`, and `comparison_baseline.paired_samples`.
   Suppress every category that regresses with
   `LUNARWING_TOOL_ESTIMATES_CATEGORIES=-<tool>`, and record the reason. The
   `declared_baseline` block is reported alongside but does not decide the
   gate. `evicted > 0` means the window outran the ring: record it and repeat
   the window with a larger capacity before trusting coverage.
4. **Feed the D1 decision.** Compare `cold_start` (`coverage`, `gate_pass`
   and paired error) with the overall metrics on the category itself. D1 is
   justified only if the measured cold-start loss materially misses the
   accuracy or coverage goal; otherwise
   the no-cache decision stands and both the data and the decision are
   recorded.
5. **Commit the evidence** as `docs/ops/history/<release>_A4_EMA_EVALUATION.md`:
   tenant, commit, window, non-secret settings, trace capacity/recorded/evicted,
   the per-category table, the pass/suppress decision for each category, and
   the D1 verdict.
6. **A5 rollout.** Set `LUNARWING_TOOL_ESTIMATES=1` with the passing set in
   `LUNARWING_TOOL_ESTIMATES_CATEGORIES`. If nothing passes, leave hints off;
   a blank allowlist permits every category. Restart and confirm hints appear
   only for allowed categories and only after the five-sample warmup. Widen to
   further tenants one at a time after their own windows. Rollback is
   `LUNARWING_TOOL_ESTIMATES=0` or unset plus a restart; never touch gates,
   grants, scheduling or cost accounting to roll back estimates.

## Persistence decision

**Initial release: daemon-local in-memory model and documented cold starts.**
This avoids hot-path I/O, schema churn and misleading stale coefficients.
After evidence of improved accuracy, consider versioned asynchronous snapshots
of bounded *aggregate* models, scoped to tenant, validated on load, with
schema/base-rate version and age limits. A corrupted/stale model must fall
back to cold start; never delay agent initialization on this optional cache.

`estimation_snapshots` and `Database::save_estimation_snapshot` /
`update_estimation_actuals` already exist in both backends, but require an
`agent_jobs` FK (`ic/migrations/V1__initial.sql:129-144`). Use them for
**persisted scheduled jobs** only, with correct job lifecycle and an
idempotent snapshot ID; do not fabricate jobs for chat tools or dump every
sample into that table. `scheduler.rs:225-235` already persists jobs before
scheduling when a store is configured. These rows are observation/history,
not the same thing as durable EMA coefficients. Activating analytics or
changing DB schema requires separate PostgreSQL/libSQL coverage and a
retention/privacy decision.

## Delivery order and acceptance gates

| Slice | Work | Exit criteria |
|---|---|---|
| 0: contract | Define clocks, keys, confidence, baselines and outcome taxonomy; unit-test learner numeric safety | Existing APIs still compile; no policy changes |
| 1: tool observations | Inject one shared estimator into common tool execution and audited bypasses | Integration tests with ≥5 real executions adjust a known tool's duration; missing cost remains unknown; no double counting |
| 2: LLM observations | Time and classify v1/v2 provider attempts; selectively add per-call cost | Failover, retry, streaming and cache tests demonstrate accurate attempt counts and stable model keys |
| 3: approval UI | Add optional estimate to persisted/live/recovered views; render in web UI and review channel notifications | Same redacted data live and after reconnect/restart; old gates still load; no auth/approval behavior change |
| 4: jobs/missions | Add bounded-run history and advisory status estimates; optionally activate job-linked snapshot analytics | Historical and running views clearly distinguish elapsed/known remaining/unknown; cancellation and concurrency tests pass |
| 5: persistence (conditional) | Only if accuracy/cold-start results justify it: versioned, bounded aggregate cache | Load/corruption/restart/multi-tenant isolation tests; no hot-path blocking |

### Rollout, rollback, and risk checks

1. Land numeric fixes and the shared observation hook with estimation **off**
   by default if compatibility requires a new setting. In a test tenant,
   enable recording only; compare estimate errors offline without displaying
   them. Ensure disabled mode does not allocate a model or change tool results.
2. Enable duration display for a small eligible set of known tools, then LLM
   calls and bounded runs only after each has its own calibration evidence.
   Count eligible/withheld estimates and dropped observations; never expose
   prompts, tool parameters or credentials in these diagnostics.
3. Keep a display/recording kill switch, independent of the existing approval
   and cost guards. Roll back by disabling estimates, **not** by modifying
   gate expiry, grant state, job scheduling, or historical cost accounting.
   If persistence is added later, version the cache so an older binary can
   ignore it safely and a rollback cannot poison future model state.

| Risk | Required guard |
|---|---|
| Lock contention on short tools or model calls | No lock held across an await, network call, UI delivery or DB write; measure p95 overhead under concurrent load |
| Fast failures bias an ETA low | Keep failure/timeout counts separate from successful runtime training; never train on a preflight denial |
| Simultaneous executions corrupt attribution | Capture the key and baseline per invocation; do not infer per-call charge from daily-counter deltas |
| Cold-start/wildly varying tasks look precise | Withhold or label provisional estimates; show sample count and uncertainty; do not promise deadlines |
| Persisted/live gate mismatch | One optional DTO, old JSON defaulting, and round-trip tests over both SSE and authenticated recovery |
| Cross-user/tenant or secret disclosure | Isolate per-daemon state; render only coarse estimates and canonical, non-sensitive category names |

Targeted regression map:

- Estimator mathematics: `ic/src/estimation/learner.rs` and `mod.rs` tests for
  minimum samples, unadjusted-baseline ratio, independent runtime/cost
  confidence, zero baselines, outliers, decimal conversion and duration bounds.
- Tool coverage: `ic/src/tools/execute.rs` tests, local worker parallel/tool
  tests in `ic/src/worker/job.rs`, routine execution tests in
  `ic/src/agent/routine_engine.rs`, and v2 adapter tests in
  `ic/src/bridge/effect_adapter.rs`. Assert *one* observation per invocation
  and unchanged sanitized output, timeouts, approval and action history.
- LLM coverage: provider/retry/failover/stream tests under `ic/src/llm/`,
  including v2 streaming cancellation tests under
  `ic/tests/engine_v2_tensorzero_streaming.rs`. Verify v2 zero-valued
  `cost_usd` never creates a free-cost training sample.
- Approval continuity: `ic/src/gate/store.rs` / `persistence.rs` JSON
  round trips, `ic/src/bridge/router.rs` SSE and authenticated recovery,
  `ic/tests/engine_v2_channel_delivery.rs`, web scenario
  `ic/tests/e2e/scenarios/test_engine_v2.py`, and v1 approval scenario
  `ic/tests/e2e/scenarios/test_tool_approval.py`. Check raw parameters and
  secret values are absent from estimate metadata and rendered UI.
- Jobs/missions: scheduler/local worker, container-manager crash watcher,
  external-worker integration, `job_status`/gateway job handlers, and
  `ic/crates/lunarwing_engine/src/runtime/mission.rs` fire/terminal watcher
  tests. Test retries, cancellation, duplicate terminal notifications,
  concurrent execution, mission reload and category isolation.

Use deterministic fake tool/provider clocks where feasible; compare frozen
prediction and actual on a held-out trace against the pre-EMA baseline
(median absolute error or symmetric percentage error, with near-zero guards),
report coverage and sample counts rather than only average error. Test
multi-tool concurrent calls, rapid model changes, timeout/cancellation,
unknown categories, extremely short/long operations, and model reset after
restart. Before enabling a public estimate for a category, require a
held-out evaluation with enough real observations to distinguish improvement
from noise: aim for at least 10% lower median absolute error than the
existing baseline, report the absolute error in seconds and the fraction of
eligible operations with an estimate, and suppress that category if it
regresses. This is a rollout gate, not a promise that the initial five-sample
EMA already achieves it. Benchmark disabled/enabled hot paths under concurrency; set an
explicit acceptable overhead budget before rollout (target <1% median and
<1 ms p95 added in a representative local workload, measured rather than
assumed). Release initially as observation-only with optional display if
necessary; have a flag/config to disable estimates without disabling tools.

Run focused tests first, then `taskset -c 0-11 cargo check -j12` and targeted
`taskset -c 0-11 cargo test -j12 ...` from `ic/`; use tmux for long runs,
`cargo fmt --all -- --check`, and clippy as resources permit. Never use a
full debug build for verification. Update relevant subsystem specs/API docs
alongside behavior; this document does not claim implementation is complete.

## Decisions confirmed for Phase A

Resolved by SweetieBot (autonomous) under #318 on 2026-11-20. These are rollout
choices, not evidence that the real-tenant accuracy or overhead gates have run.

1. **Cold start for the initial rollout; durable cache deferred.** The existing
   per-registry in-memory model remains the initial store and must relearn after
   a restart. Do not build durable EMA coefficients by default. Phase A's
   held-out evaluation must measure accuracy after warmup and again during a
   representative cold-start window, including the five-sample withheld period
   plus enough subsequent calls to bound warmup error and coverage loss. Durable
   aggregate snapshots are justified only if that measured loss materially
   misses the accuracy/coverage goals in a category or materially worsens the
   observed operational workload. If justified, reuse the bounded, non-sensitive,
   per-tenant snapshot design above with no synchronous hot-path I/O; otherwise
   record the no-cache decision and measurements in the rollout evidence.
   A restart during evaluation resets the observation window rather than
   silently mixing pre-restart and post-restart baselines.
2. **Keep learned hints hidden until five valid completed samples, including
   configured categories.** Do not display an unvalidated provisional baseline.
   The category allowlist controls which warmed learned estimates may display;
   it is not evidence that a static base duration is accurate. Cold-start
   behavior remains visibly unknown or withheld, never a made-up precise ETA.
   Held-out traces retain the unadjusted baseline for comparison even when no
   user-facing prediction was eligible.
3. **Initial hints remain web/WASM-only; native XMPP, Signal and REPL changes
   are deferred.** These adapters currently omit the approval description.
   When the initial category rollout has passed evaluation, the follow-up may
   derive only a short plain-text duration from the trusted frozen estimate
   snapshot, never from raw approval descriptions or parameters. The line is
   `Typical tool runtime: ~Xs; excludes approval wait; not a deadline.` and is
   capped at 120 UTF-8 bytes (truncate before adding an ellipsis if needed).
   Enforce the existing per-channel parameter/message budgets; drop the optional
   duration first and fail open to the existing prompt if the full hint would
   exceed the budget. Never add approval details to shared-channel messages:
   the existing XMPP groupchat-suppression guard must stay before any delivery,
   and the other channels must retain their direct, owner-scoped status
   routing. Add tests for the duration-only line, truncation and redaction, and
   do not duplicate the hint where a channel already renders the description.
4. **A4 compares against a model-derived baseline for every category
   (maintainer decision, 2026-11-21).** The evaluation trace records the mean
   of the model's own first five valid completed runs of the same user/tool
   model, captured at `begin()` with the prediction and never including the
   evaluated call or anything later. The gate is at least 10% lower median
   absolute error than that baseline, per category. Where a tool declares
   `estimated_duration` (`http` today), the trace keeps that comparison too;
   the summary names which baseline each figure uses. This stays in-memory and
   parameter-free, adds no storage or hot-path I/O, and leaves the warmup and
   cold-start cohorts reported separately.

All automatic timeout, scheduling, provider-selection, bid/value, and budget
decisions based on these estimates are **out of scope** until accuracy and
failure modes have been demonstrated separately. Changing any gate's authority,
expiry, channel/request-ID binding, decision token or redaction contract is
never a rollout mechanism.

## Decisions recorded for phases C–E

Recorded under #318 on 2026-11-22 from dead-code evidence; none of these
changes behaviour or claims an accuracy result.

- **C5 — the legacy job-estimate API is deleted, not composed.** `Estimator::estimate_job`,
  `Estimator::record_actuals`, `JobEstimate` and `ToolEstimate` had no callers
  outside `ic/src/estimation/`. They are removed, so
  `estimate_operation` / `record_operation_actuals` are the only estimate API.
  A future whole-job estimate must compose operation estimates and keep elapsed,
  known-remaining and unknown separate; it must not resurrect `value` or the
  unadjusted `tool_breakdown`.
- **D1 — no durable cache is built yet.** This stays exactly the A6.1 decision:
  the cache is conditional on the A4 tenant measurement showing that cold-start
  loss materially misses the accuracy or coverage goals. No snapshot code is
  added before that evidence, and a negative result is recorded in the rollout
  evidence doc rather than in code.
- **E1 — `ReliabilityTracker` is deleted.** `lunarwing_engine::ReliabilityTracker`
  had no consumers anywhere in `ic/`. It was a second, unrelated EMA writer;
  wiring it would have duplicated the estimator's clock and could be mistaken
  for ETA statistics. The module and its `lib.rs` re-export are removed; the
  engine crate keeps no reliability EMA.
- **E2 — tool-cost learning stays inactive.** No production tool supplies
  `estimated_cost`, and hints remain duration-only. Adding baselines is deferred
  until a real per-call charge exists to learn from; the absence is documented
  rather than replaced with a nominal price.

## Phase B implementation choice (B3 carrier)

Implemented under #318 on 2026-11-21; maintainer acceptance remains separate.

**Keep the estimator hook as the single carrier; the observability
tool/LLM start/end events stay unwired.** `LlmTimingProvider` decorates the
chain `build_provider_chain` already builds, so it observes exactly the calls
the caller awaits, needs no new correlation ID, and cannot run twice over the
same event. The `observability/traits.rs` events still lack a per-invocation
correlation ID, cost, and timeout classification, and wiring them would add a
second writer for the same clock. Cost is deliberately not learned here: the
real per-call charge stays with `CostGuard::record_llm_call`, and the v2
adapter's `cost_usd: 0.0` is "unavailable", never "free" (B2).

The LLM clock is **completed logical-call latency**, measured outside retry,
smart routing, failover and the circuit breaker, and inside the response cache.
A cache hit bypasses the timer. Both streaming clocks start before setup.
Blocking and streaming dispatches, retries, provider switches and first-chunk
latency are counted separately. A failed, partial, cancelled or switched call
does not train. A successful single-provider call (including same-provider
backoff) trains its selected configured provider/model pair, with a 256-byte
bound on each label. Diagnostics are the owner-only `GET /api/estimates/llm`;
`LUNARWING_LLM_TIMINGS=0` disables recording and unset observes. Gate-wait
analytics (B4) are implemented separately in `ic/src/gate/wait_analytics.rs`
with the owner-only `/api/estimates/gate-waits` view, and stay separate from
these runtime EMAs.
