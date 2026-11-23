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

`Estimator::estimate_operation` is the only estimate API. The legacy
`estimate_job` facade, `JobEstimate`, `ToolEstimate` and its numeric
`record_actuals` were deleted under WI#318 (C5): they had no production callers,
and their placeholder prices, value/margin arithmetic and static per-tool
breakdown must not become a second job-estimate path.
`record_operation_actuals` accepts `Actuals` to represent absent measurements
and a frozen forecast explicitly.

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
name, the tool's declared duration baseline, a model-derived comparison
baseline, the frozen EMA prediction, the actual duration, and the model's
valid duration sample count **before the call started**. Only `Ok`
completions are traced; there are no parameters, user identities or trace
writes to logs, files or the database. The oldest record is evicted when full.
The owner route copies the retained records and aggregates them after releasing
the trace lock. Check `evicted` when assessing window coverage.

Every category is compared against two named baselines, so the rollout gate no
longer depends on `http` being the only tool that declares one:

- `declared_baseline` uses `Tool::estimated_duration`, where a tool supplies it.
- `comparison_baseline` is the mean of the model's own **first five valid
  completed runs** (the warmup window). It is derived only from earlier calls,
  frozen at `begin()` with the prediction, and never includes the call being
  evaluated or anything later. It is `null` until one valid run exists.

For each category, `coverage` is the fraction of retained completions with a
learned prediction. Each baseline block reports its own median absolute errors
(in ms), `samples` and `improvement_percent`, computed only over the calls where
both that baseline and a frozen EMA prediction existed. The two blocks are
paired over the same retained records but not necessarily the same subset.
`warmup` reports counts and baseline-only error for calls started before the
model had five valid duration samples, for both baselines. `cold_start` reports
the same coverage and paired metrics as the overall category, but only for
calls started with fewer than `cold_start_sample_limit` (20) prior valid
durations in that user/tool model. This includes warmup and the first
post-warmup forecasts. It is based on model age, not completion order, and
starts over when a model is evicted or the registry restarts.

**Gate definition (per category):** at least 10% lower median absolute error
than the `comparison_baseline`, with absolute error in seconds, `coverage` and
sample counts reported. A declared baseline is reported alongside where one
exists. Without paired samples, an error median or `improvement_percent` is
`null`; a zero baseline median also makes improvement undefined. None of these
cases passes the gate. Each cohort carries the verdict as `gate_pass`: `true`
only when `improvement_percent >= 10.0` against the comparison baseline,
`false` when the category was judged and regressed, and `null` when it could
not be judged (no paired sample, or an undefined improvement). `gate_pass` on
`overall` decides the rollout; the `cold_start` verdict feeds the D1 decision.
Running the representative tenant evaluation and enabling hints for passing
categories are separate steps.

## LLM logical-call timing (WI#318 Phase B)

Tool hints answer "how long does this tool take". LLM calls need the same
treatment at a different boundary, so a separate daemon-wide cache records
**completed logical-call latency**.

- **One boundary.** `LlmTimingProvider` (`ic/src/llm/timing.rs`) decorates the
  chain built once by `build_provider_chain`, so v1 chat, local jobs, routines,
  container execution and the v2 `LlmBridgeAdapter` are measured by the same
  timer. There is no per-delegate timer and no second writer: the
  `observability` tool/LLM events remain unused in this implementation.
- **What trains.** One sample per successful logical call with a known
  provider/model and no switch. A successful same-provider retry contributes
  one sample including its backoff, not separate attempt samples. The timer
  sits outside retry, smart routing, failover and the circuit breaker, but
  inside the response cache and stream integrity. Cache hits bypass it.
  Streaming time starts before setup and ends at `Done`; failed, partial or
  dropped calls do not train.
- **What is counted separately.** `calls_started`, `completions`, `failures`
  and `cancellations` count logical calls. `provider_attempts` counts concrete
  provider dispatches; `retried_calls` and `retry_attempts` count same-provider
  retries that actually started, including blocking calls and retries that
  later fail. Attempt counters update when the logical call ends.
  `provider_switches` counts calls that change provider or model after the first
  dispatch, including failover and smart-routing escalation; those calls never
  train. If cooldown selects a fallback as the first and only provider, there
  is no switch. First content chunk latency, including setup, is summed
  separately (`first_chunk_samples`, `first_chunk_sum_ms`).
- **Attribution.** The selected provider freezes a forecast at its first
  dispatch using the configured provider ID and model (up to 256 bytes each).
  Concurrent calls do not share attribution, and providers serving the same
  model name have distinct keys. A LunarWing Cloud model override not matching
  that provider's configured model still runs, but is counted without training
  a named model. No request-supplied model label, prompt, result, user identity
  or URL is stored.
- **No cost.** Duration only. A real per-call charge stays with
  `CostGuard::record_llm_call`; the v2 adapter's `cost_usd: 0.0` means
  "unavailable", so it never becomes a free-cost sample.
- **Diagnostics.** `GET /api/estimates/llm` is owner-only (403 for other
  authenticated users, 401 without a token) and reports `mode`,
  `min_samples`, the counters above, and per-provider/model `duration_ema_ms`,
  `time_error_ema`, `confidence`, `confidence_label` and the warm
  `estimate_ms` (absent until five valid samples). No user identity or private
  key is included.
- **Setting.** `LUNARWING_LLM_TIMINGS=0` disables recording; unset or any
  other value observes. It is read once when the chain is built, is not
  DB-backed, and resets on restart. Nothing here changes behaviour: an
  estimator failure can never change a call's result.

## Gate-wait analytics (WI#318 Phase B4)

Gate wait is observed separately from tool and LLM runtime EMAs, and nothing
here feeds an estimate or a decision.

- **What is recorded.** For an approved or denied gate, `resolution_at -
  created_at`; for an expired gate, `expires_at - created_at`, i.e. the
  fail-closed deadline rather than the moment a later ingress noticed the
  expiry. Cancellations and credential hand-offs are not approval waits and are
  not recorded. Waits are clamped to one day, and clock skew yields zero.
- **Labels.** Only the outcome (`approved`, `denied`, `expired`) and a bounded
  channel family (`web`, `gateway`, `xmpp`, `signal`, `repl`, `http`,
  `bootstrap`, `extension`). Extension channel names and everything derived from
  them collapse to `extension`; no parameters, users, threads or request IDs
  are stored.
- **Storage.** In-memory, process-lifetime, bounded to 24 series, in the
  pending-gate store. No persistence, no I/O on the resolution path. Recording
  never changes `take_verified`'s request-ID, channel or expiry checks, and
  expiry remains a fail-closed security timer that reads none of this data.
- **Diagnostics.** `GET /api/estimates/gate-waits` is owner-only (403 for other
  authenticated users, 401 without a token) and reports per series the sample
  count, mean, maximum and EMA of the observed wait in milliseconds, plus a
  `note` stating that this is not a promise about human response time.

## Job and mission-run estimates (WI#318 Phase C)

Phase C estimates a **bounded run**, never an open-ended conversation, a
mission's lifetime, or a whole coding job's unknown future steps.

- **Local scheduler jobs only (C1).** `Scheduler::dispatch_job_inner` freezes
  one historical duration into `JobContext.estimated_duration` before the
  first persist. The learner key is a private per-user key with one of two
  non-sensitive kinds: `routine` for full-job runs the routine engine
  dispatches (it marks them with `estimate_kind = "routine"` in the job
  metadata), and `user` for every other local dispatch (`create_job`, `/job`,
  a gateway restart, which carries no old metadata). A routine's recurring
  run and a job a user starts by hand are not comparable completions, so
  they never train one model. Job titles, descriptions and free-form metadata
  never become keys, so a user has at most two job models. The worker reads
  the same kind from the same metadata for the terminal observation. Cost is
  never estimated because no job declares a cost baseline.
- **One terminal observation.** `Worker::run` records after exactly one
  execution loop, so the several completion paths cannot train one run twice.
  Only `Completed` trains the duration model. `Failed` and `Cancelled` are
  counted separately because their elapsed time is not a comparable run;
  `Stuck` records nothing so self-repair can still finish the job.
- **Sandbox and external-worker jobs are excluded (C2).**
  `ContainerJobManager` and `ExternalWorkerManager` jobs have a different
  start/terminal mapping; they never receive a local-job estimate just
  because both paths use `create_job`. Their `estimated_duration` stays unset.
- **Surfaces (C4).** `job_status`, `GET /api/jobs` and `GET /api/jobs/{id}`
  report measured elapsed separately from the frozen historical duration and
  report remaining time as `unknown`. A known remaining time is deliberately
  not invented from a max-iteration limit or worker timeout, because neither
  is a finite plan. `GET /api/jobs/summary` stays counts-only: an aggregate
  ETA over unrelated jobs has no defined scope, so none is produced.
- **Mission runs (C3).** `GET /api/missions/{id}` derives a single run's
  advisory timing from that mission's own threads on read. A valid run is a
  thread that finished successfully (`Completed` or `Done`; one that went on
  to `Failed` does not count), and its duration is its **active** time read
  from its own state-change events: from its first entry into `Running` to
  its first entry into `Completed`, minus every stretch spent `Waiting` (an
  approval, a child) or `Suspended`. A run whose events do not show when it
  started and finished is not counted. The view reports the valid run count
  and the newest running run's elapsed; the median and the last run's
  duration stay withheld until five valid runs exist, the same warm-up as
  every learned hint (A6.2). `lunarwing_engine` is untouched, there is no
  second EMA writer, no extra store read beyond the threads already loaded
  for the detail view, and no write on the fire path. A recurring mission's
  lifetime is never estimated.
- **Display gating.** Job and mission timing follow the same opt-in display
  mode as tool hints (`LUNARWING_TOOL_ESTIMATES=1`). Recording continues in
  observe mode; `0` disables both recording and display. The per-tool category
  allowlist is not consulted for jobs or missions, whose categories are not
  tool names.
- **Never an input to behaviour.** Nothing here feeds scheduling, timeouts,
  approvals, cost accounting or provider selection. In display mode the
  frozen job estimate does reach the agent, but only as a field of a
  `job_status` tool result the agent asked for (C4); it is never added to a
  prompt on its own, and observe mode keeps it out of `job_status` too.

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
still open: the real-tenant held-out evaluation (A4), enabling hints (A5) and
the conditional durable aggregate cache (D1). Phase B (LLM timing, gate-wait
analytics) and Phase C (bounded local-job and mission-run views) are
implemented; sandbox and external-worker jobs are explicitly excluded from
job estimates. The diagnostics surface, per-category display gating and the
opt-in evaluation trace (WI#318 A2/A3/A4 recording) are implemented; running
A4 on a tenant and enabling hints (A5) are still operator steps. Whole-job
remaining time is reported as unknown unless a finite plan actually exists.
This cache never changes scheduling, approval policy, timeouts, provider
selection, or budgets.
