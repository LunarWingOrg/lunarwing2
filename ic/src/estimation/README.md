# Runtime estimation (WI191)

The first live integration is **tool runtime calibration and advisory approval
text**. `ToolRegistry` owns one `ToolEstimates`, shared by chat, Engine V2,
local job workers, lightweight routines, container execution and scheduler
subtasks using that registry. There is no process-global estimator or database
write. Each registry starts cold; no learned factors survive a restart.

## Observation contract

`tools::execute::execute_timed_tool` is the execution-only boundary. Callers
retain their existing lookup, validation, hooks, allowlists and approvals.
The helper captures unadjusted `Tool::estimated_duration` / `estimated_cost`
hints before execution and measures elapsed time with a monotonic clock around
the existing timeout-wrapped `Tool::execute`. It records exactly one `Ok`
completion through `Estimator::record_operation_actuals` and the learner's
`record_actuals` **before** output serialization.
Reported `ToolOutput.duration` is not used: it may cover a different scope or
be zero. `Ok` means the tool returned normally, not that the user's task or a
shell command succeeded.

Failures, timeouts, canceled futures, preflight denials and approval/auth waits
do not train completed-call runtime. They are not converted to zero-duration
successes. The existing execution/audit paths still report those outcomes;
this cache does not add a second failure-counting system.

The canonical executed tool name and user identity form a private length-framed
key. No parameters, descriptions, result text, credentials or raw errors enter
the learner. Approval lookups resolve tool aliases and check tool visibility.
Different users and registries cannot see one another's estimates. Keys above
the input length limits (512-byte user / 256-byte tool name) are ignored by
estimation without preventing execution. At most 1,024 models are retained;
new categories evict the least recently observed category.

## Mathematics and presentation

- Alpha defaults to 0.1. A new factor is initialized from the first valid
  observation; later updates use `0.9 * previous + 0.1 * observation`.
- A declared positive duration baseline is calibrated with
  `actual / UNADJUSTED baseline`, clipped to `[0.01, 100]`. It is never
  divided by the last corrected prediction. Ratio calibration has its own
  valid-sample count.
- Without a declared baseline, successful elapsed times train a direct
  duration EMA. After initialization, each incoming duration is bounded to
  `[0.1, 10]` times the prior EMA before updating, to limit individual outliers.
  Zero durations and durations over one day are excluded; published runtime
  estimates are capped at one day. This is not a percentile or SLA.
- No runtime estimate is returned until five valid completed calls exist for
  that user/tool. The generic legacy five-second default is not used.
  WASM timeouts are safety ceilings, not duration baselines; WASM tools now
  use the default `None` hint and learn actual elapsed time.
- Duration confidence is a heuristic: `n/(n+20) * (1-error_ema)`, where
  normalized absolute error compares actual time with the prediction frozen
  before execution. A missing prediction contributes maximum uncertainty.
  Display uses only `low` or `moderate`, never a probability claim.
- Cost is optional and independently warmed. Only positive declared cost
  paired with a nonnegative actual `ToolOutput.cost` trains a ratio; `None`
  is unknown, not free. No cost is inferred from LLM budgets or v2's placeholder
  `cost_usd=0`. Decimal arithmetic is checked. Cost is not displayed in this
  performance-first slice. Explicit `Some(0)` is a known free call, not missing
  accounting, so cost ratios intentionally allow zero (unlike duration ratios).
  No production tool currently supplies an `estimated_cost` baseline; cost
  calibration is exercised by the integration fixture, and production costs
  remain unknown until a tool supplies both a baseline and an actual charge.

`Estimator::estimate_operation` is the new live consumer API. The older
`estimate_job` facade remains for compatibility but its placeholder prices,
value/margin arithmetic and static per-tool breakdown are **not** used for
live predictions. Its numeric `record_actuals` interface remains available
for compatibility; the new `record_operation_actuals` accepts `Actuals` to
represent absent measurements and a frozen forecast explicitly.

An approval description receives one short line such as:

> Typical tool runtime: ~2.00s (EMA, 5 completed calls, low confidence).
> Excludes approval wait; not a deadline.

With display enabled, both v1 approval creation paths and the v2 approval-insertion
funnel append the hint. Existing descriptions carry it through web live events, v1 history
and v2 persisted pending gates/reconnect without a new wire field or migration.
The snapshot does not change when later calls update the model. Authentication
and external-wait gates receive no tool hint. Web and WASM channels render
descriptions; native XMPP/Signal/REPL paths that omit descriptions retain their
current output. Gate IDs, expiry, redaction, channel checks and resolution
semantics are unchanged. Both legacy history builders now use the same
redacted `display_parameters` as live notifications rather than serializing raw
execution parameters. A legacy approval lacking that snapshot returns `null`
parameters in its history view; it never falls back to credentials. The same
rule now applies to v2 live delivery and recovery when a gate lacks a redacted
snapshot (including a legacy persisted gate or a tool removed while pausing).

## Diagnostics and category gating (WI#318)

The read-only gateway routes expose what observe mode learns. They use the
existing bearer-token middleware and add no listener. No response includes
parameters, results, credentials or private learner keys.

- `GET /api/estimates` is **user-scoped**: the token resolves the user and the
  response lists only that user's categories with `samples`,
  `duration_ema_ms`, `time_error_ema`, `confidence`, `confidence_label`
  (`none`/`low`/`moderate`), cost/duration sample counts and
  `hint_displayed`. It also reports the active `mode` and category-policy
  shape, but no registry-wide counts.
- `GET /api/estimates/counters` is **owner-only** (`owner_id`) and returns
  unlabeled totals for the registry, even with the trace off. `estimates`
  counts one `estimate_requests` per tracked execution attempt, partitioned
  into `estimates_returned` (a learned prediction was eligible) and
  `estimates_withheld` (no learned prediction yet). This includes observe
  mode, suppressed categories, and attempts that later fail or are canceled.
  Approval lookups do not increment these counters. `observations` counts only
  completed `Ok` calls submitted to the learner: recorded and dropped
  observations, clipping events, rejected duration values and model evictions.
- `GET /api/estimates/evaluation` is **owner-only** and reports the opt-in
  trace aggregate (see below). With the trace disabled it answers `404` for
  the owner. Both owner-only routes answer `403` for other authenticated users
  before checking registry or trace availability.

Learner `debug!` tracing covers model evictions and clipped or rejected
observations; no category key (which embeds the user) is ever logged.

### Per-category display gating

`LUNARWING_TOOL_ESTIMATES_CATEGORIES` gates **display only** and is read once
when each registry is built. Entries are comma-separated canonical tool names;
positive names form an allowlist, and `-name` or `!name` suppress a category.
Suppression wins. An unset or blank setting allows all categories; a valid
suppression-only list allows all other categories. Malformed rules (e.g. `-`
without a name), names over 256 bytes, or more than 256 distinct rules make
the policy `invalid`: warn at startup and withhold every hint. This avoids
silently dropping a restriction. Categories that are not displayed keep
recording. The independent `LUNARWING_TOOL_ESTIMATES=0` kill switch disables
recording and hints entirely.

### Held-out evaluation trace (A4 input)

`LUNARWING_TOOL_ESTIMATES_TRACE=1` enables a bounded in-memory ring (2,048
records; a number selects 16–8,192). Each record stores the canonical tool
name, unadjusted baseline, frozen EMA prediction, actual duration, and the
model's valid duration sample count **before the call started**. Only `Ok`
completions are traced; there are no parameters, user identities or trace
writes to logs, files or the database. The oldest record is evicted when full.
The owner route copies the retained records and aggregates them after releasing
the trace lock. Check `evicted` when assessing window coverage.

For each category, `coverage` is the fraction of retained completions with a
learned prediction. Both median absolute errors (in ms) and
`improvement_percent` use only the same `paired_samples`: calls with both a
usable declared baseline (positive and at most one day) and a frozen EMA
prediction. `warmup` reports counts and baseline-only error for calls started
before the model had five valid duration samples. `cold_start` reports the
same coverage and paired metrics as the overall category, but only for calls
started with fewer than `cold_start_sample_limit` (20) prior valid durations
in that user/tool model. This includes warmup and the first post-warmup
forecasts. It is based on model age, not completion order, and starts over
when a model is evicted or the registry restarts.

Only `http` currently declares a production duration baseline. Without paired
samples, both error medians and `improvement_percent` are `null`; a zero
baseline median also makes improvement undefined. None of these cases passes
the rollout gate. A comparison baseline for other tools (or a revised gate)
still needs a maintainer decision. Running the representative tenant
evaluation and enabling hints for passing categories are separate steps.

## LLM logical-call timing (WI#318 Phase B)

Tool hints answer "how long does this tool take". LLM calls need the same
treatment at a different boundary, so a separate daemon-wide cache records
**completed logical-call latency**.

- **One boundary.** `LlmTimingProvider` (`ic/src/llm/timing.rs`) decorates the
  chain built once by `build_provider_chain`, so v1 chat, local jobs, routines,
  container execution and the v2 `LlmBridgeAdapter` are measured by the same
  timer. There is no per-delegate timer and no second writer: the
  `observability` tool/LLM events remain unused, which settles the plan's
  "choose one carrier" decision in favour of the estimator hook.
- **What trains.** Exactly one sample per completed call the caller awaited.
  The timer sits outside retry, smart routing, failover, circuit breaking and
  the response cache, and inside stream integrity. A cache hit never reaches
  the inner provider, so it cannot train. Streaming calls train only on the
  terminal `Done` chunk; a failed, partial or dropped stream is counted
  (`failures` / `cancellations`) and trains nothing.
- **What is counted separately.** Retry and failover layers already report
  recovered attempts as `LlmStreamObservation::Retried`; the timer records
  `retried_calls` and `retry_attempts` from those and never converts them into
  duration samples. First-chunk latency is summed separately
  (`first_chunk_samples`, `first_chunk_sum_ms`) and never feeds the EMA.
- **Attribution.** Keys are canonical `Llm(model)` labels with bounded length
  (256 bytes). The entry label is the chain's active model; the completion
  label is the model that actually served the call, so a failover trains the
  fallback rather than the primary. Keys never contain prompts, results,
  parameters, user identity or URLs.
- **No cost.** Duration only. A real per-call charge stays with
  `CostGuard::record_llm_call`; the v2 adapter's `cost_usd: 0.0` means
  "unavailable", so it never becomes a free-cost sample.
- **Diagnostics.** `GET /api/estimates/llm` is owner-only (403 for other
  authenticated users, 401 without a token) and reports `mode`,
  `min_samples`, the counters above, and per-model `duration_ema_ms`,
  `time_error_ema`, `confidence`, `confidence_label` and the warm
  `estimate_ms` (absent until five valid samples). No user identity or private
  key is included.
- **Setting.** `LUNARWING_LLM_TIMINGS=0` disables recording; unset or any
  other value observes. It is read once when the chain is built, is not
  DB-backed, and resets on restart. Nothing here changes behaviour: an
  estimator failure can never change a call's result.

## Disable and performance

The default is **observation-only**. Set **`LUNARWING_TOOL_ESTIMATES=1` before
starting the daemon** to opt into advisory approval hints after the five-sample
warmup, or **`LUNARWING_TOOL_ESTIMATES=0`** to disable both recording and new
hints. Unrecognized values (including `observe`) select observation-only.
This follows the plan's staged rollout: representative held-out accuracy
evaluation is still required before enabling hints broadly; the five-sample
threshold is not proof of prediction accuracy. The environment is read once
when each registry is built; this is not a DB-backed setting. Existing persisted approval descriptions
keep their frozen hint, including after a restart with estimation disabled.
There are no background tasks, extra model calls or storage I/O. Cache
updates and snapshot copies are bounded; no mutex is held across a tool
future, hook or other await. A poisoned cache is ignored without changing
execution results.

## Coverage and follow-ups

Tests cover five real timed completions, unknown/free/priced costs, concurrent
calls, cancellation/timeouts/failures, job-worker and routine paths, user
isolation, alias lookup, numeric bounds, bounded storage, both v1 approval
creation paths after real chat turns, and a v2 gate's live SSE/status text versus
file-backed recovery (including missing redaction snapshots). An ignored microbenchmark reports
begin/complete overhead separately from tool execution, with 1 and 12 concurrent
threads and disabled/enabled comparisons. Deployment target: <1 ms p95 added
overhead and <1% median on a representative workload; the microbenchmark alone
does not establish the latter workload-level result.

The later slices in
[`WI191_PERFORMANCE_EMA_INTEGRATION.md`](../../../docs/plans/WI191_PERFORMANCE_EMA_INTEGRATION.md)
remain follow-ups: LLM logical-call timing, bounded whole-job/mission-run
views, gate-wait analytics, durable aggregate caches and the real-tenant
held-out evaluation. The diagnostics surface, per-category display gating and
the opt-in evaluation trace (WI#318 A2/A3/A4 recording) are implemented; running
A4 on a tenant and enabling hints (A5) are still operator steps. Tool calls
*within* a job are covered now; the whole job does not yet have an ETA. This
cache never changes scheduling, approval policy, timeouts, provider selection,
or budgets.
