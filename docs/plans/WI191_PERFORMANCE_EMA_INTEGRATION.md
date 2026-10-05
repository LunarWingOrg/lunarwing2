# WI #191 — Performance-first EMA estimation integration plan

Status: **tool/approval slice implemented on `cmc/g10/v2.1.4.0`; later slices remain planned**.
Implementation contract: [`ic/src/estimation/README.md`](../../ic/src/estimation/README.md).
The initial integration covers slices 0/1 and the approval-description portion
of slice 3: per-user registry-owned learning, all three tool execution paths,
five-sample warmup, and frozen v1/v2 approval hints. Recording defaults to
observation-only; `LUNARWING_TOOL_ESTIMATES=1` enables hints and `=0` disables
the cache. It uses existing description
fields rather than adding estimate DTOs across channel protocols. LLM timing,
whole-job/mission-run estimates, gate-wait analytics, held-out workload
evaluation and durable caches remain follow-ups; tool calls within those
jobs are covered by this slice. Costs are only learned when actually reported,
and are not displayed. See the
[goal-10 evidence record](../ops/history/v2.1.4.0_GOAL10_EMA_VALIDATION.md) for checks run.

Original plan reviewed against
`wire-in-ema` at `05ddcc5cd` (2026-09-26). Tracker:
[work item #191](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/191).
The work item says implementation starts **after the 2.1.3.0 release**; verify
that milestone before executing this plan. This branch's base matching `master`
does not, by itself, prove the release has happened.

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

- `ic/src/estimation/mod.rs:45-117` exposes `Estimator::estimate_job` and
  `record_actuals`, with **no production callers**. `learner.rs:45-108` keeps
  an in-memory per-category EMA (`alpha=0.1`, `min_samples=5`).
- `ic/src/estimation/cost.rs:16-45` and `time.rs:12-38` know only `http`,
  `echo`, `time`, `json`; unknown tools get a nominal $0.001 / 5 seconds.
  `ToolEstimate.confidence` is a literal 0.7 (`mod.rs:71-78`). The LLM price
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
Do not serialize the current `JobEstimate` wholesale into a UI: its `value`
is not a useful performance metric and its `tool_breakdown` is presently
unadjusted even though the job total can be adjusted
(`ic/src/estimation/mod.rs:71-97`). Introduce a separate optional public
runtime-estimate DTO or evolve the facade with explicit compatibility tests;
the sum of displayed components must have the same semantics as the displayed
aggregate.
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
not a fake `agent_jobs` foreign key. Mission management currently has no
general `/api/missions` REST CRUD path, only a webhook ingress; specify a
real viewer before promising a new mission status field in the web gateway.

### C. Diagnostics — optional read-only surface

Aggregate top slow tool/model categories and calibration error with bounded
cardinality. Expose only non-sensitive category keys and aggregate counts to
authorized operators; no raw arguments, prompts, tokens, credentials or
per-user cross-tenant data. This is useful for diagnosing bottlenecks, but
not a reason to add synchronous DB writes to the hot path.

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

## Decisions to confirm before implementation

1. Ship cold-start in-memory learning first (recommended) or require durable
   coefficients on day one?
2. Does the first UI show a clearly labeled cold-start baseline, or hide
   ETA until five valid samples? Recommendation: hide unknown-tool ETA;
   show explicitly provisional baseline only for intentionally configured
   categories.
3. Should later non-web approval notifications display the short duration,
   or limit phase 3 to web pending-gate SSE/recovery while testing channel
   message sizes and translations?

All automatic timeout, scheduling, provider-selection, bid/value, and budget
decisions based on these estimates are **out of scope** until accuracy and
failure modes have been demonstrated separately.
