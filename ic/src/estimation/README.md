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
There are no background tasks, extra model calls or hot-path I/O. Only a
short bounded statistical update/read holds a synchronous mutex; it is never
held across a tool future, hook or other await. A poisoned cache is ignored
without changing execution results.

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
views, gate-wait analytics, durable aggregate caches, held-out workload
calibration and concurrency overhead evaluation. Tool calls *within* a job
are covered now; the whole job does not yet have an ETA. This cache never
changes scheduling, approval policy, timeouts, provider selection, or budgets.
