# Engine V2 Architecture

The V2 engine is a unified thread-capability-CodeAct execution model that lives in `ic/crates/lunarwing_engine/`. It replaces approximately 10 separate v1 abstractions (Session, Job, Routine, Channel, Tool, Skill, Hook, Observer, Extension, LoopDelegate) with 5 core primitives.

**Revalidated against source 2026-09-28 (v2.1.3.0).** Module map, gate types,
mission seeding, lease/policy behavior, env knobs, and bridge boundaries were
re-checked against the current tree, including the landed WI#227 sealed step
records; updates for 2.1.1.0 include the
best-effort `thread_start` event dispatch (item 9/WI#109 scope A) and the
`ENGINE_V2_CHANNELS` routing-policy clarification (there is no `none`
opt-out; unknown entries are ignored). Updates for 2.1.2.0 include the
WI#132 authenticity hardening (labeled/neutralized memory retrieval,
compaction summaries, and system-role content), the removal of the dead
`executor/context.rs` module, and the Goal 8 insights hardening
(host-validated `save_conversation_insight` writes, eligible-user-evidence
batching, project-scoped mission dispatch, restart-safe `InsightCursor`
batching, and explicit insight-mission opt-out with a Paused tombstone).
Updates for 2.1.3.0 include the WI#227 sealed step records (interpreter step
output carries the `SealedStepRecord` role through the inference transcript
only; `Thread::add_message` reroutes it out of the user-visible transcript,
the trace analyzer keeps `code_error` detection over sealed records, and all
wire surfaces use `MessageRole::wire_name()`).

Enabled at runtime via the `ENGINE_V2=true` environment variable. The gateway
uses Engine V2 by default when enabled. The bridge router (`ic/src/bridge/router.rs`)
keeps other channels on the legacy path unless an eligible channel name is
present in `ENGINE_V2_CHANNELS` (case-insensitive exact matching). Multi-tenant
provisioning (`ic/scripts/lunarwing-mt-admin.sh`, env generation in
`mt-admin-lib/06-env-generation.sh`) writes `ENGINE_V2=true` and
`ENGINE_V2_CHANNELS=xmpp,weechat,darkirc` into new tenant envs by default and
preserves existing tenant values across upgrade/rebuild/re-render.

## Five Primitives

| Primitive | Purpose | Replaces (v1) |
|-----------|---------|---------------|
| **Thread** | Unit of work with lifecycle, parent-child tree, capability leases | Session + Job + Routine + Sub-agent |
| **Step** | Unit of execution (one LLM call + its action executions) | Agentic loop iteration + tool calls |
| **Capability** | Unit of effect with actions, knowledge, and policies | Tool + Skill + Hook + Extension |
| **MemoryDoc** | Unit of durable knowledge (summaries, lessons, skills) | Workspace memory blobs |
| **Project** | Unit of context (scopes memory, threads, missions) | Flat workspace namespace |

## Module Map

```
ic/crates/lunarwing_engine/src/
  lib.rs                  Public API, re-exports
  types/                  Core data structures (no async, no I/O)
    thread.rs             Thread, ThreadId, ThreadState, ThreadType, ThreadConfig
    step.rs               Step, StepId, LlmResponse, ActionCall, ActionResult, TokenUsage
    capability.rs         Capability, ActionDef, EffectType, CapabilityLease, PolicyRule
    memory.rs             MemoryDoc, DocId, DocType (Summary/Lesson/Skill/Issue/Spec/Note/Plan)
    project.rs            Project, ProjectId
    event.rs              ThreadEvent, EventKind (20 variants for event sourcing)
    message.rs            ThreadMessage, MessageRole
    provenance.rs         Provenance enum (User/System/ToolOutput/LlmGenerated/etc.)
    conversation.rs       ConversationSurface, ConversationEntry, EntrySender
    mission.rs            Mission, MissionId, MissionCadence, MissionStatus
    error.rs              EngineError, ModelFailureKind, ThreadError, StepError, CapabilityError
  traits/                 External dependency abstractions (host implements these)
    llm.rs                LlmBackend trait
    store.rs              Store trait (33 CRUD methods)
    effect.rs             EffectExecutor trait
  capability/             Capability management
    registry.rs           CapabilityRegistry (register/get/list capabilities)
    lease.rs              LeaseManager (grant/check/consume/revoke/expire leases)
    policy.rs             PolicyEngine (deterministic allow/deny/approve)
    planner.rs            Capability planning logic
  gate/                   Execution gates (approval, auth, rate limits)
    pipeline.rs           GatePipeline (composes gates in sequence)
    mod.rs                GateDecision, GateResolution, ExecutionMode, ResumeKind
    tool_tier.rs          Tool-tier gate logic
    lease.rs              Lease-gated approval
  runtime/                Thread lifecycle management
    manager.rs            ThreadManager (spawn, stop, inject messages, join threads)
    conversation.rs       ConversationManager (routes UI messages to threads)
    mission.rs            MissionManager (long-running goals that spawn threads on cadence)
    tree.rs               ThreadTree (parent-child relationships)
    messaging.rs          ThreadSignal, ThreadOutcome, ThreadFailureKind, signal channels
    skill_feedback.rs     Skill usage feedback events (feature-gated)
  executor/               Step execution
    loop_engine.rs        ExecutionLoop (core loop replacing run_agentic_loop)
    structured.rs         Tier 0: structured tool call execution
    scripting.rs          Tier 1: embedded Python via Monty (CodeAct/RLM)
    compaction.rs         Context compaction when approaching model context limit
    prompt.rs             System prompt construction (CodeAct preamble/postamble)
    trace.rs              Execution trace recording and retrospective analysis
    llm_stream.rs         LLM streaming support
    orchestrator.rs       Self-modifiable Python execution layer (CodeAct orchestrator)
  memory/                 Memory document system
    store.rs              MemoryStore (project-scoped doc CRUD)
    retrieval.rs          RetrievalEngine (keyword-based context retrieval from project docs)
    skill_tracker.rs      SkillTracker (confidence tracking, versioned updates, rollback)
  reliability.rs          ReliabilityTracker (per-action success rate and latency via EMA)
```

## Thread State Machine

```
Created --> Running --> Waiting --> Running (resume)
                   --> Suspended --> Running (resume)
                   --> Completed --> Done | Failed (finalization error)
                   --> Failed
```

All transitions are validated by `ThreadState::can_transition_to()`. Terminal states: `Done`, `Failed`. Invalid transitions return `EngineError::InvalidTransition`.
`Created`, `Running`, `Waiting`, `Suspended`, and `Completed` can transition to
`Failed`; the last case closes failures during final persistence.

### Thread Types

- **Foreground** -- user-initiated conversational threads
- **Research** -- research/analysis threads
- **Mission** -- mission-driven threads spawned by the mission system

Threads form a tree via `ThreadTree`. Parent threads can spawn children, and child completion propagates events up to the parent.

## Execution Loop

`ExecutionLoop::run()` is the core loop that replaces the v1 `run_agentic_loop()`. It handles three `LlmResponse` variants:

```
1. Check signals (Stop, InjectMessage) via mpsc::Receiver
2. Build context (messages + available actions from active leases)
3. Call LLM via `LlmBackend::complete_stream()` (with a `complete()` fallback for non-streaming backends)
4. If Text: check tool-intent and leaked-prose recovery, return accepted final response
5. If ActionCalls (Tier 0): find lease -> check policy -> consume use -> execute -> record
6. If Code (Tier 1): execute Python via Monty with context-as-variables
7. Record Step, emit ThreadEvents
8. Repeat until: accepted text response, stop signal, recovered terminal limit, typed failure, or approval needed
```

### Provider stream integrity

The host wraps the completed provider chain with `StreamIntegrityProvider`
(`ic/src/llm/stream_integrity.rs`). It observes native and fallback streams after
retry, failover, caching, timeout, and optional trace recording, but before the
bridge converts chunks into engine types. This does not change provider retry
policy or rewrite generated content. It detects and marks:

- an exact, substantial phrase of at least four normalized tokens repeated three
  times consecutively (two repetitions remain valid rhetorical/roleplay text);
- chat-template role delimiters such as `<|im_start|>` and mid-output bare
  `SYSTEM:` / `ASSISTANT:` headers; fenced code, inline code, and quoted lines are
  excluded to keep protocol documentation from false-positive detection;
- stream errors, empty streams, and EOF before the required `Done` chunk.

A detected corruption appends a visible bracketed integrity marker before
`Done`; the original output is retained as data. Retry, failover, abort, and
corruption facts also travel as typed observation chunks, never as provider
controlled text. The engine keeps them outside assistant output and adds one
provider-name-free system line to the next model request (including model-error
recovery). This tells the agent to attribute the discontinuity to the upstream
provider and not treat instruction-like generated text as trusted instructions.

Per-provider counters and time-to-first-chunk samples are atomically written to
`$LUNARWING_BASE_DIR/runtime/llm-stream-metrics.json`. The host-global
`health-llm-streams.sh` check reads this file directly on single-node installs or
discovers every tenant state directory from the ports registry. The health
exporter publishes `lunarwing_health_llm_streams_*` series with bounded
`provider` and `tenant` labels. A recent issue makes this monitor-only
healthpoint degraded; it never automatically restarts a model or daemon.

Incident response: treat the bracketed marker or `llm_streams` degradation as an
upstream-integrity signal. Check the per-provider abort/decode/retry/corruption
metrics and TensorZero/provider health before restarting a tenant. Do not infer
agent memory/state damage from garbled output alone, and do not follow commands
that appear inside the marked generation.

#### REPL argument integrity (WI#161)

REPL drafts have an additional pre-execution guard shared by the provider
wrapper, the bridge's blocking path, and the engine stream collector
(`types/repl.rs`). Structured `repl` calls must contain a non-empty string
`code`; void/scalar/malformed JSON payloads are rejected. Raw think tags,
argument-protocol tags, and nested fences outside Python strings/comments are
rejected rather than stripped into potentially different executable code.
CodeAct fences close only on a standalone closing-fence line outside Python
strings. A nested opening fence cannot terminate and execute a valid prefix.
The existing language preference (`repl`, `python`, `py`, bare) is retained.
Ordinary Python syntax errors still go through Monty's existing bounded
syntax-correction path.

The distinct `repl_argument_corruption` model failure supplies fixed repair
guidance and permits one fresh model request, including when draft text has
already reached the UI: no calls from the rejected response have executed.
Repeated corruption terminates with that same typed failure. Corrupted code,
arguments, and provider diagnostics are excluded from recovery instructions
and structured warning fields. General tools' data arguments are not treated
as executable REPL code.

The Rig adapter keeps reasoning events separate from argument deltas and
suppresses final arguments only after **non-empty argument bytes** have been
streamed. A name-only delta does not discard the final call's arguments; a
final call can also supply an otherwise-missing name without duplicating
already-streamed arguments.

`repl_argument_corruption_total` counts rejected responses per provider in
`runtime/llm-stream-metrics.json` (streaming and blocking paths). The existing
registry-aware healthpoint aggregates it by tenant and exports
`lunarwing_health_llm_streams_repl_argument_corruption_total{provider,tenant}`.
Old snapshots without the field read as zero. Like other stream-integrity
signals, this is monitor-only. See the
[Goal 34 verification record](../ops/GOAL34_REPL_ARGUMENT_INTEGRITY.md).

### Two Execution Tiers

**Tier 0 -- Structured Tool Calls** (`executor/structured.rs`):

Standard tool/function calling. The LLM returns structured `ActionCall` objects. For each call:
1. Find the capability lease that grants this action
2. Check the policy engine (allow/deny/approve)
3. Consume one use from the lease (if use-limited)
4. Execute via `EffectExecutor`
5. Record the `ActionResult`

**Tier 1 -- CodeAct / Python via Monty** (`executor/scripting.rs`):

Embedded Python execution following the RLM (Recursive Language Model) pattern. The LLM generates Python code that the Monty interpreter executes.

Key characteristics:
- **Context as variables**: thread messages injected as `context` Python variable, thread goal as `goal`, step index as `step_number`, prior action results as `previous_results` dict
- **Tool dispatch**: unknown function calls suspend the VM, go through lease check + policy check + `EffectExecutor`, then result returns to Python
- **Recursive LLM calls**: `llm_query(prompt, context)` suspends the VM, spawns a single-shot LLM call, returns text as a Python string. Results stay as variables (symbolic composition), not injected into the parent's attention window
- **Compact output metadata**: between code steps, only a summary is added to chat context (`[FULL OUTPUT: N chars]`, `[TRUNCATED: last 8000 of N chars shown]`, a short return-value preview, or `[code executed, no output]`) to prevent context bloat
- **Explicit termination**: `FINAL(answer)` or `FINAL_VAR(name)` for output
- **Resource limits**: user CodeAct blocks default to a 30s timeout, 64MB memory,
  and 1M allocations. The orchestrator VM has a separate 600s/128MB/5M budget.
  Monty execution is wrapped in `catch_unwind` for panic safety.
- **Action limit**: each model step can attempt at most 50 tool actions. The cap
  applies independently to structured batches, CodeAct blocks, and the combined
  single/parallel action host calls made by the Python orchestrator.
- **Host-clock OS calls**: `date.today()` and `datetime.now(tz)` are served from
  the host clock (`OsFunction::DateToday`/`DateTimeNow` — Monty has no clock, so
  these suspend to the host like tool calls). Naive `datetime.now()` returns UTC;
  passing a `datetime.timezone` shifts to that fixed offset. Every other OS call
  (filesystem, `os.getenv`, network) is still denied with `OSError` (WI#227: the
  denial used to fire for the date functions too, pushing a "stale date system
  error" `OSError` line into the step output that the orchestrator renders as a
  user-visible message).
- **Sealed step records (WI#227)**: interpreter step output blobs (`[stdout]`,
  `[name ERROR]`, `[return]`) are inference data, not conversation. The default
  orchestrator flags freshly produced `format_output` blobs with a transient
  `_step_record` marker; the host decoder (`classify_wire_role`) mints those —
  and only those, plus the persisted `StepRecord` wire spelling — as
  `MessageRole::SealedStepRecord`. `Thread::add_message` reroutes sealed records
  into `internal_messages`, so the user-visible transcript (`thread.messages`)
  can never contain them; the LLM adapter still feeds them to the model as
  `Role::User` step output, and delegated-worker prompt context excludes them.
  The Python VM cannot mint the role: unknown wire spellings and unflagged
  writes decode as `User` exactly as before. Trace diagnostics treat sealed
  records like legacy `User` code blobs, so `code_error` warnings (and the
  self-improvement missions keyed on them) keep firing for sealed interpreter
  failures. All wire surfaces — orchestrator JSON, Monty `context`, bridge
  DTOs, trace records, thread events — spell roles via
  `MessageRole::wire_name()` rather than Rust `Debug` formatting.

### Python Orchestrator

The orchestrator (`executor/orchestrator.rs`) is a self-modifiable Python execution layer that can replace the compiled Rust `ExecutionLoop::run()` loop:

- Executes via Monty with resource limits
- Exposes host functions: `__llm_complete__`, `__execute_code_step__`,
  `__execute_action__`, `__execute_actions_parallel__`, `__check_signals__`,
  `__emit_event__`, `__save_checkpoint__`, `__transition_to__`,
  `__retrieve_docs__`, `__check_budget__`, `__get_actions__`,
  `__list_skills__`, `__record_skill_usage__`, `__propose_skill_patch__`, and
  `__propose_skill_prune__`. Skill mutation/usage tracking is inert unless
  `SKILL_SELF_IMPROVEMENT=true`.
- Can be patched by the self-improvement mission and versioned in the Store when
  `ORCHESTRATOR_SELF_MODIFY=true`
- The same opt-in controls prompt-overlay consumption when assembling a system
  prompt. Stored overlays, including those with `source: compiled_in`, are inert
  while disabled. Compiled v0 seeding and failure bookkeeping use the host-only
  `Store::save_internal_memory_doc` path; model metadata is never write authority.
- Falls back to compiled-in default (v0) if disabled or patches fail (3+ consecutive failures triggers rollback)
- Nudges short roleplay or retry prose back into a `repl` block for non-conversational
  goals, with a default two-attempt cap; substantive prose and short conversational
  goals remain valid terminal responses. Syntax-invalid `repl` blocks use the same
  bounded correction before terminal delivery through shared channels such as XMPP.
- Parallel dispatch of tool calls via the `__execute_actions_parallel__` batch host function (Rust executes the batch concurrently with a JoinSet)
- Default orchestrator source: `ic/crates/lunarwing_engine/orchestrator/default.py`

### Automatic memory recall

`RetrievalEngine::retrieve_context` supplies ordinary conversational context.
It includes user-owned and shared knowledge in the same project, but excludes
skills (which have a separate activation path) and engine-maintenance records:
`fix_pattern_database`, `prompt:codeact_preamble`, `orchestrator:main`, and
`orchestrator:failures`. Either a reserved title or a maintenance tag
(`fix_patterns`, `prompt_overlay`, `orchestrator_code`, `orchestrator_meta`)
excludes a document, including legacy untagged and renamed tagged records.
Reserved `prompt:` and `orchestrator:` title prefixes are also excluded.

Relevance uses case-insensitive whole tokens, retaining `_` and `-` inside
identifiers. Query terms are deduplicated and common conversational filler is
ignored: `hi` cannot match `hint`, and `web` cannot match `web_search`.
Conversational recall requires positive keyword overlap; empty/stop-word-only
queries and zero-match queries return no excerpts rather than unrelated recent
memories. Partial matches remain eligible when a query has additional terms.
An empty automatic recall is not evidence that no memory exists; explicit
`memory_search`/document access is still available.

General mission planning, mission threads, and their host-marked mission children use
`retrieve_mission_context`: maintenance records remain eligible and zero-match
queries retain type-priority/recency fallback. The host selects this policy from
thread configuration; `__retrieve_docs__` arguments cannot opt a conversation
into it. **Conversation-insights missions are the exception:** the host assigns
`MissionContext::ConversationInsights` with a query derived only from the
trigger batch's complete user statements. Both meta-prompt construction and
first-step `__retrieve_docs__` use conversational relevance and maintenance
exclusions; an empty batch recalls nothing. The context is inherited by managed
and inline mission children. Model arguments and mutable REPL state cannot
switch policies or replace the batch query. Retrieved documents retain
untrusted source labels. Dedicated overlay/orchestrator loading has its own
self-modification gate.

Insight-shaped documents have their own recall-eligibility rule (WI #183
G8-F4): automatic recall surfaces only host-minted, current-version `explicit`
insight records whose captured sources are complete and equal the doc content.
`inferred` records and pre-hardening insight-shaped lessons stay inspectable
through explicit tools but never ride automatic recall.

### Authenticity hardening (WI#132)

Memory-retrieval and compaction content is LLM-adjacent data, so it is labeled
and neutralized before it can masquerade as engine-authored state:

- `__retrieve_docs__` is the live retrieval path (the dead `executor/context.rs`
  doc-neutralizer was removed in 2.1.2.0 — its only callers were tests, so it
  projected false assurance). Its handler neutralizes role-token openers in
  returned doc content and stamps each doc with
  `"provenance": "untrusted_memory_retrieval"`, document ID, optional source
  thread ID, visibility scope, and update timestamp. The default orchestrator
  renders these source fields and a fixed untrusted-reference label, with quoted
  excerpts capped at 500 characters. It explicitly distinguishes retrieved
  records from current user messages/instructions and notes that records may be
  stale or mistaken. Missing legacy source fields are labeled `unknown`, never
  assumed to come from a completed thread; persisted metadata cannot upgrade
  the trust label. The complete rendered section is role-token-neutralized.
  Mission meta-prompts also label and quote their retrieved records.
- Compaction summaries are LLM output that replaces real history. The summary
  is prefixed with an explicit `[Compaction summary — LLM-generated, replaces
  earlier conversation history]` label and its role-token openers are
  neutralized before it is inserted as an assistant message (compaction
  laundering seam).
- Both LLM adapters (`ic/src/llm/rig_adapter.rs`,
  `ic/src/llm/lunarwing_cloud_chat.rs`) neutralize role-token openers in
  system-role content as a final backstop (idempotent on already-neutralized
  content).
- The engine-internal `memory_search` tool labels its results with
  `"provenance": "engine_internal"` so they are never silently
  ordinary-looking memory.

### Failure Recovery

Engine V2 keeps provider retry, fallback-model selection, cooldown, and circuit
breaking in the host LLM stack. When those paths return a final provider error,
the bridge maps it to `ModelFailureKind`. The Python orchestrator receives only
that safe class and fixed recovery text, never provider-supplied diagnostics.

- By default, recoverable model failures get one extra model request. Context-overflow
  recovery first replaces protocol history with a bounded plain-text view of
  the original goal and recent context. A second failure becomes a typed
  `ThreadFailureKind::Model` outcome with actionable user text.
- Mid-stream transport failures (`transient`, `unavailable`) remain recoverable
  after partial text has been streamed (WI#244). The failed response never
  reaches assistant-history or action commitment, so retrying cannot execute
  its partial code or tool calls. Failed streams do not synthesize `Done` or
  commit usage. The existing per-step and lifetime recovery limits still apply,
  as do the per-kind caps (at most two transient retries, one unavailable retry).
  Rate limits remain provider-owned; the engine does not add a retry loop for
  them.
- Streamed deltas are provisional **Live work** in the web UI. Deltas from the
  failed and recovered attempts can accumulate there until terminal/final
  reconciliation; only the successful final answer is committed as a response.
  Model recovery does not emit a `stream_gap` or reset event. `stream_gap` is
  the separate subscriber-lag recovery path.
- Authentication and rejected-request failures skip model retry because an
  unchanged request cannot repair credentials or compatibility. Their terminal
  text names the operator action required.
- Reaching `max_iterations` schedules one final tool-capable model request before
  returning `ThreadOutcome::MaxIterations`.
- Three consecutive code steps or structured action batches with no successful
  work schedule one no-progress warning. Repeating failure on warning turn
  returns `ThreadFailureKind::NoProgress`; successful work resets detector.
- Missing/exhausted capability leases and policy denials return actionable model
  observations naming allowed next steps instead of opaque denial strings.

One-shot accounting lives in `state["_failure_recovery"]` with pending and active
warning states. It is written before recovery request, consumed only after
successful model response, and retained when approval/authentication resume
discards rest of live checkpoint. Terminal cleanup removes it with normal
runtime checkpoint state.

## Capability Leases

Threads don't have static permissions. Instead, they receive **leases** -- scoped, time-limited, use-limited grants:

```rust
CapabilityLease {
    thread_id,
    capability_name,
    granted_actions,
    expires_at: Option<DateTime>,   // time-limited
    max_uses: Option<u32>,          // use-limited
    revoked: bool,
}
```

### Lease Lifecycle

1. **Grant**: `LeaseManager::grant()` creates a lease for a thread
2. **Check**: `LeaseManager::check()` verifies the lease is valid (not expired, not revoked, uses remaining)
3. **Consume**: `LeaseManager::consume_use()` decrements the use counter
4. **Revoke**: `LeaseManager::revoke()` permanently disables the lease
5. **Expire**: `LeaseManager::expire_stale()` cleans up time-expired leases

### Policy Engine

The `PolicyEngine` evaluates actions against leases deterministically:

- **Priority**: `Deny > RequireApproval > Allow`
- **Effect types**: `ReadLocal`, `ReadExternal`, `WriteLocal`, `WriteExternal`, `CredentialedNetwork`, `Compute`, `Financial`
- Every action declares its side effects via `EffectType`. The policy engine uses these for allow/deny decisions
- **Provenance types**: origin labels exist for user, system, tool, LLM, and
  memory-retrieval data. `PolicyEngine::evaluate_with_provenance()` implements
  targeted approval rules for LLM-generated or tool-sourced data. Structured,
  scripting, and orchestrator action dispatch all call the provenance-aware
  evaluator for LLM-generated action inputs.

## Execution Gates

Unified gate abstraction for all pre-execution checks (approval, auth, rate limits):

```
GateDecision:
  Allow                                    -- proceed immediately
  Pause(reason, ResumeKind)                -- suspend thread, wait for resolution
  Deny(reason)                             -- reject, do not execute

ResumeKind:
  Approval { allow_always }                -- user must approve
  Authentication { credential_name, instructions, auth_url }  -- user must authenticate
  External { callback_id }                 -- external system callback

GateResolution:
  Approved { always }
  Denied { reason }
  CredentialProvided { token }
  Cancelled
  ExternalCallback { payload }

ExecutionMode:
  Interactive | InteractiveAutoApprove | Autonomous | Container
```

**Gate pipeline** (`GatePipeline`): composes multiple gates in sequence. The first `Pause` or `Deny` wins.

**Resolution flow**:
1. Gate returns `Pause` -- `PendingGate` is inserted into the in-memory store
   and persisted through `FileGatePersistence` under
   `$LUNARWING_BASE_DIR/pending-gates.json`
2. SSE broadcasts `AppEvent::GateRequired` to the user
3. Web UI shows approval/auth prompt
4. User responds with `GateResolution`
5. Thread resumes with the result injected as a message

The authenticated `GET /api/chat/pending-gates` endpoint exposes the caller's
non-expired `PendingGateView` records, including redacted display parameters and
typed approval/auth requirements. Clients can reconcile after reconnect or
`stream_gap`; SSE itself remains a bounded, ephemeral stream. The browser keeps
gate state independently of the history DOM, so either history/snapshot response
order preserves the prompt. Live gate events supersede in-flight snapshots;
resolved request IDs cannot be resurrected by stale responses. Engine startup
unavailability and transient recovery failures are retried. Authentication is by
bearer header (query-token fallback is reserved for streaming routes).

Recovered authentication controls send both `request_id` and `thread_id` to
`POST /api/chat/auth-token` and `POST /api/chat/auth-cancel`. The pair is optional
for legacy clients, but an explicitly supplied pair must match a live auth gate
owned by the caller and its credential name; partial identities return 400 and
stale/mismatched identities return 409. Cancellation atomically takes that gate,
finalizes its waiting thread, persists removal, and emits `gate_resolved`.
Token configuration queues a process-local, secret-wrapped control payload for
the selected gate. Dispatch revalidates the gate before resolution; credentials
never become ordinary chat input if the gate expires or changes while queued.
The browser waits for cancellation confirmation before tombstoning its cache,
and for a resolution event or authoritative snapshot after token submission.
Extension-wide auth completion cannot dismiss a different pending request for
the same extension.

Startup restore removes expired gates from disk as well as skipping them in
memory; pruning failure is logged and never revives an expired approval.
Conversation lookups and creation share a store-backed hydration path, using
the same conversations-before-index lock order and rechecking after I/O. Auth
resolution accepts an exact UUID thread hint as a fallback while still enforcing
user ownership, source/trusted channel checks, and ambiguity rejection.

Engine V2's effect adapter consumes resolved `auto_approve_tools` configuration.
It only bypasses `UnlessAutoApproved`: disabled tools, `Always` requirements,
shared-channel rules, and supervised mode retain their gates. See
`docs/guides/ENABLING_DEV_TOOLS.md` for environment aliases and diagnostics.

## Learning Missions

### Mission Scheduling

Mission cron schedules run at one-minute resolution. User-facing mission tools
accept `manual`, `hourly`, `daily`, minute intervals such as `30m`, hour
intervals such as `6h`, and 5-7 field cron expressions whose seconds field is
zero. Invalid cadence values fail instead of silently becoming manual missions.

Cron missions receive `next_fire_at` when created, updated, or resumed. Startup
repairs older valid cron missions missing this value. Scheduler fires advance
the next occurrence even when the daily thread budget blocks a run; manual
fires do not shift the cron schedule. Daily thread budgets reset on UTC day
boundaries. Startup and each scheduler tick persist the rollover for active
missions of every cadence, including idle manual/event/webhook missions and
cron missions that are not yet due (WI#243, v2.1.3.0). This maintenance does not
spawn a thread or change the schedule. Fire-time rollover remains in place for
triggers arriving before the next tick. Maintenance reloads and saves under
the same per-mission lock as spawn reservations so it cannot erase a concurrent
fire's budget usage. Undated legacy counters are stamped with today's UTC date
without resetting their count, conservatively preserving upgrade-day usage;
same-day counters are unchanged. Paused and terminal missions are not swept.
A failed maintenance save skips that mission for the tick and is retried on
the next tick without preventing other missions from receiving maintenance.
The store's global mission listings enumerate the missions map directly
rather than deriving from the projects index: missions are discovered by
directory scan (`engine/projects/*/missions/*/mission.json`), so a mission
whose `project.json` is missing or unparseable still loads while its project
does not. Project-derived listings dropped such missions from the active
index entirely — no rollover, no dispatch, and a `threads_today_date` pinned
at its creation date (WI#243 recurrence, v2.1.4.0).

The daily thread budget (`max_threads_per_day`,
default 30 via `DEFAULT_MAX_THREADS_PER_DAY` since work item #189, 0 =
unlimited) is configurable at creation time through `mission_create` and
afterwards through `mission_update`. The default moved from 10 to 30 because
an hourly mission otherwise got only 10 hours of daily coverage; 30 covers a
full day of hourly fires plus retry headroom. Every successfully spawned
thread increments `threads_today`, so retries that respawn a thread draw from
the same budget; only spawn-failure rollback refunds a slot. `mission_list`
and the web mission list/detail views expose `threads_today`,
`max_threads_per_day`, and a derived `budget_exhausted` flag so an
active-but-exhausted mission is distinguishable from a dead one. Budget
exhaustion is logged at warn level with the mission name and counters; the
mission stays Active and resumes spawning at the next UTC day rather than
going terminal. `mission_fire` reports the precise reason when nothing fires
(terminal, paused, or budget exhausted).

When a mission thread terminates without completing — a signal-stopped thread
(killed inference, `stop_thread`), a lost join handle, or a terminal
`ThreadOutcome::Failed` — the mission manager records the interruption in the
approach history, notifies the mission's `notify_channels` as an error, and,
when the mission's `retry_policy` has attempts left, schedules a delayed
re-fire with exponential backoff (default 1m → 2m → 4m, capped at 1h;
`initial_delay_secs = 60`, `backoff_multiplier = 2.0`, `max_delay_secs = 3600`,
`max_retries = 3`; all four scalars are exposed by `mission_update`, or can
be set through the persisted `retry_policy`). Each retry respawn passes
the normal budget gate and increments `threads_today`, so retries draw from
the same daily budget — a budget-exhausted re-fire is abandoned, and a paused
or terminal mission suppresses its own pending retry. A terminal
non-completion (`Failed`, `Stopped`, or `MaxIterations`) releases its trigger
dedup key so an external redelivery of the same event/webhook payload can fire
the mission again (at-least-once for
`event:`/webhook cadences). `GatePaused` retains the key while approval is
pending, preventing a duplicate fire on redelivery. Retry attempt numbers
are stored on the spawned thread's metadata, not in model-visible trigger
payloads. If the daemon restarts mid-backoff, the pending retry marker on the
failed thread's metadata lets the post-restart sweep enumerate persisted
missions (even ones missing from the in-memory active index) and re-arm the
re-fire with a fresh full backoff window (work items #188 and goal #38).

`MissionManager::ensure_learning_missions()` provisions two baseline missions
and, when `SKILL_SELF_IMPROVEMENT=true`, three additional skill-improvement
missions. They are event-driven; not every mission runs after thread completion.
These system-managed missions are recurring: one successful event run cannot
mark them complete. Startup reconciliation repairs older terminal records and
refreshes their compiled prompt, cadence, success criteria, and daily budget
while preserving operator pauses and mission history. When the
`SKILL_SELF_IMPROVEMENT` gate is off, the gated missions are paused with a
gate-driven marker; re-enabling the toggle resumes exactly those missions,
while missions paused by an operator stay paused.

The event listener also dispatches a best-effort `thread_start` event on the
initial `Created -> Running` transition of top-level Foreground threads only
(item 9, WI#109 scope A). Mission and Research threads and child threads never
dispatch, so mission recursion and research fan-out cannot re-trigger the
event. The payload carries `source_thread_id`, `project_id`, `thread_type`, and
`goal`. This is asynchronous plumbing, not a synchronous pre-execution hook —
`mission_create`
cadence descriptions document that `event:<pattern>` is text-matching, not an
engine-lifecycle subscription.

WI#143 / WI#109 follow-ups closed the "born-dead cadence" gap: the bridge
cadence grammar gained `system:<source>/<event_type>` (e.g.
`system:engine/thread_start`) as the way to subscribe to engine lifecycle
events, and `create_mission`/`update_mission` reject an `OnEvent` cadence whose
pattern is exactly a known system event type (`thread_start`,
`thread_completed_with_issues`, `thread_completed_with_learnings`,
`conversation_insights_due`) — such a mission could never fire as intended.
Mission listings surface `last_fired_at` / `never_fired` so an "Active" status
cannot be mistaken for a dispatching one (registry honesty).

The v1 `routine_history` tool resolves mission names too (WI#265): when the
v1 routines registry misses a name, the tool queries the bridge's
`get_engine_mission_history_by_name`, which searches the default project's
missions (own + shared) and then falls back to a global ownership-filtered
scan, rendering recent fires from the mission's `thread_history` with
char-safe (never byte-index) summary truncation. A name found in neither
registry errors with a message naming both registries and pointing at
`routine_list` / `mission_list`.

WI#109 scope B (synchronous pre-execution sanitation) landed in 2.1.2.0
(item 11): `ExecutionLoop::run` purges stale cached tool output from the
loaded checkpoint **before** the `Running` transition and the first model
call. Tier 1 keys (`time`, `date`, `datetime`) are always removed; Tier 2
keys (`web_search`, `http`, `shell`) are removed only when older than 30
minutes, judged by a `_cached_at` RFC 3339 timestamp inside the value (values
without a valid timestamp are never purged). The Python orchestrator stamps
`_cached_at` onto Tier 2 dict outputs at the persisted-state write site
(`stamp_cached_at`, `orchestrator/default.py`, mirroring
`TIER2_STALE_STATE_KEYS`), so Tier 2 aging is fully effective; scalar
outputs stay un-stamped and un-purged (fail-open). When anything is purged, the
sanitized state is written back to `runtime_checkpoint.persisted_state` and a
bounded `sanitized_at` marker is stamped on the thread metadata for
observability. This is a deterministic synchronous hook at the execution
boundary — not a mission trigger — so it applies to every thread type
including Mission threads, and the first inference (and every repl-code step)
sees sanitized state. See `docs/ops/v2.1.2.0_WI109_WI126_SANITIZE_MEMORY.md`
for the full verification record.
Event payloads are seeded into the mission thread's initial persisted state as
`state["trigger_payload"]` before execution. Terminal thread events are published
after final thread persistence so listeners can load complete source state.
Initial mission state remains available across approval/authentication pauses,
while normal terminal cleanup removes it. System-event dispatch is
project-scoped: `fire_on_system_event` takes the dispatching project as a
host-supplied argument, and a mission only fires when its project matches — so
a same-user event in project A cannot fire that user's mission in project B
(WI #183 G8-F6).

Trigger payloads are deduplicated at fire time: a payload carrying a
`source_thread_id` and/or `content_hash` that the mission has already processed
spawns no thread and consumes no daily budget. The processed-key set is
persisted on the mission (FIFO-bounded to 512 keys), so re-delivered events are
skipped even across daemon restarts (work item #69). Conversation-insights
batches are selected durably instead of counted: the mission persists an
`InsightCursor` (last reserved `completed_at`/`thread_id`) with its thread
reservation, and batch identity is content-derived from the batch's sorted
message ids (`insights:v2:<project>:<ids>`), so a restart can neither re-fire
the consumed batch nor collide a genuinely new one with an old processed key
(WI #183 G8-F7). The periodic tick also scans for eligible sources directly, so
completions that raced their event's persistence or arrived during broadcast
lag still produce runs.

### Error Diagnosis (`self-improvement`)

- **Trigger**: fires when a thread completes with actionable Warning/Error trace
  issues. Info observations remain trace telemetry and do not consume mission
  budget.
- **Action**: diagnoses root cause and applies prompt overlays or orchestrator patches

### Skill Extraction (`skill-extraction`, feature-gated)

- **Trigger**: fires when a thread succeeds with 5+ steps and 3+ tool actions,
  and only when `SKILL_SELF_IMPROVEMENT=true`
- **Action**: extracts reusable skills with activation metadata, evidence-backed
  optional CodeAct snippets, and domain tags. The bounded trigger summary cannot
  be expanded into invented commands, arguments, outputs, regexes, or snippets.
- **Output**: stored as `DocType::Skill` MemoryDoc

### Conversation Insights (`conversation-insights`)

- **Trigger**: a batch of five newly completed, eligible source threads in the
  project, or the durable source scan on tick
- **Evidence**: each top-level Foreground thread captures its ingested User-role
  statements at ingestion time as complete, untruncated `InsightSource` records
  (message/thread/conversation identity + timestamp). Generated goals, steer
  notes, system injections, research/mission threads, failed threads, other
  users' or projects' threads, legacy pre-upgrade threads, and truncated
  statements are never sources; a single oversized statement (over the
  per-statement or per-thread bound) disqualifies its entire conversation —
  fail-closed, so partial evidence is never extracted. The host selects the
  batch from persisted threads under the mission fire lock; caller-supplied
  payload content is ignored.
- **Context**: the current source batch and relevant non-maintenance knowledge.
  Existing generated `current_focus` and `approach_history` are excluded from
  the next extraction prompt, including legacy history. FINAL reports are
  retained for audit, but insights do not update focus or request a generated
  next agenda.
- **Host writer**: insight threads write only through the atomic
  `Store::save_conversation_insight` entry point. Every citation is re-validated
  against the persisted source statement (whole-statement quote, same
  project/user, still-eligible thread, reserved run with current status and
  generation). The host mints the title (`insight:<kind>:<DocId>`), the
  `insight` tag, and empty metadata; model-supplied identity/trust fields
  cannot pass through. Explicit statements upsert by exact content and
  inferred records by evidence set, so duplicates are impossible; at most five
  distinct insights per run are accepted. A quote records what was said, not
  that its claims are true.
- **Recall eligibility**: automatic recall surfaces only host-minted,
  current-version `explicit` insight records whose sources are complete and
  equal the doc content. `inferred` records persist for inspection via
  explicit tools but never ride automatic recall, so prior generated
  insights cannot become independent user corroboration. Contradictions are
  kept: a newer complete statement is its own explicit insight.
- **Opt-out**: deleting the mission persists a `Paused` tombstone
  (`deleted_at`), bumps `insight_generation` so stale writers fail the gate,
  stops in-flight runs, and survives both startup reconciliation and
  same-name recreation. `mission_delete` with `purge_insights` removes only
  host-minted insight records in that project/user scope — user- or
  model-authored docs that merely carry an `insight:` title or `insight` tag
  survive it. An explicit `mission_resume` re-enables extraction from the
  resume point forward (skip-until-resume): statements made while the mission
  was paused or tombstoned are never learned, so no disabled-window backlog
  fires on re-enable.

### Expected Behavior (`expected-behavior`)

- **Trigger**: explicit `user_feedback/expected_behavior` events
- **Action**: investigates user-reported expectation gaps and proposes or applies
  an appropriate correction

### Skill Maintenance (`skill-maintenance`, feature-gated)

- **Trigger**: dedicated `skill_maintenance_due` events, emitted only when at
  least one prune candidate exists and `SKILL_SELF_IMPROVEMENT=true`
- **Action**: identifies confidently dead or quarantined skills and stages
  user-approved prune/archive proposals

`self-improvement`, `skill-extraction`, and `skill-maintenance` are all
feature-gated. With the flag disabled, those missions are paused (and resume on
re-enable unless an operator paused them), and skill usage tracking, automatic
demotion, patch proposals, and prune proposals are inert.

Capability leases are revoked in memory and persistence when their owning thread
reaches `Done` or `Failed`. Startup cleanup also reconciles valid leases left by
older versions for terminal or missing threads; waiting and suspended thread
leases remain active for resume. Spawn/resume persistence failures roll back any
leases granted during that attempt. Startup also revokes persisted leases absent
from their owner's `capability_leases` list. Per-thread resume locks prevent
duplicate approval/authentication callbacks from starting concurrent loops.

## V2 Skill System

V2 skills (`lunarwing_skills::v2`) differ fundamentally from v1:

| Aspect | V1 | V2 |
|--------|----|----|
| Selection | Deterministic in Rust (`skills/selector.rs`) | Python orchestrator (`score_skill()`) |
| Storage | Filesystem `SKILL.md` files | MemoryDocs via the Store |
| Trust | Binary (Trusted/Installed) | Policy engine controls via leases |
| Creation | Manual authoring | Auto-extracted by skill-extraction mission |
| Tracking | None | Confidence metrics (success/failure ratio) |

### V2 Skill Types

```rust
V2SkillMetadata {
    name, version, description,
    activation: ActivationCriteria,
    source: V2SkillSource (Authored | Extracted | Migrated),
    trust: Trusted | Installed,
    code_snippets: Vec<CodeSnippet>,   // Python function bodies
    metrics: SkillMetrics,              // usage_count, success_count, failure_count
    parent_version: Option<u32>,        // for rollback
    content_hash: String,
    patch_history, pending_patch,       // staged, auditable patch lifecycle
    deprecated_at, deprecation_reason, automatic_demotion,
    archived_at, archived_reason, pending_prune,
    registry_url, registry_publisher, registry_slug, registry_version,
    pulled_at, published_at, registry_content_hash, pending_update,
}
```

All lifecycle fields use serde defaults so older skill metadata remains
readable. Demotion and archival are soft state transitions; skill documents are
retained for audit and recovery.

Confidence: `1.0` if no recorded outcomes (benefit of the doubt), else `success_count / (success_count + failure_count)`.

## Bridge Adapters

The daemon integrates the engine via three adapters in `ic/src/bridge/`
(the directory also hosts the SSH subsystem — `ssh.rs`, `ssh_agent.rs`,
`ssh_api.rs`, `ssh_client.rs`, `ssh_hostkeys.rs`, `ssh_secrets.rs` — plus
`auth_manager.rs` and `skill_migration.rs`, which are not engine adapters):

### LlmBridgeAdapter (`ic/src/bridge/llm_adapter.rs`)

Wraps `LlmProvider` as `LlmBackend`:
- Converts between `ThreadMessage` and `ChatMessage`
- Converts `ActionDef` to `ToolDefinition`
- Supports cheaper provider for sub-calls (depth > 0)
- Returns provider text to the engine; `LlmResponse::from_text()` in
  `types/step.rs` classifies fenced Python as CodeAct code

### EffectBridgeAdapter (`ic/src/bridge/effect_adapter.rs`)

Wraps tool execution and safety layer as `EffectExecutor`:
- Maps engine action calls to tool registry execution
- Applies safety checks, timeout, serialization
- Keeps action discovery split by purpose: `registered_actions()` returns the
  unscoped live tool catalog only for late-tool lease planning, while
  `available_actions(leases)` advertises only actions covered by a supplied,
  currently valid lease. Execution prompts therefore cannot expose a tool that
  the thread is not allowed to call.
- The thread-type-aware `LeasePlanner` still decides which registered actions a
  resumed thread may gain. In particular, mission leases exclude administrative
  and autonomous-denylisted actions such as `create_job`; foreground leases keep
  them behind their normal interactive gates.

### HybridStore (`ic/src/bridge/store_adapter.rs`)

Wraps the `Workspace` as the engine's `Store` trait:
- 34 Store-trait methods for threads, steps, events, projects, docs, leases,
  missions, and conversations (29 overridden by `HybridStore`; five shared-scope
  and global-list helpers use trait defaults)
- In-memory `HashMap` caches backed by workspace files under `engine/`
- Human-readable knowledge/orchestrator files plus JSON runtime state under
  `engine/.runtime/`
- Fallback to workspace files on cache miss; terminal cleanup evicts memory but
  preserves persisted output
- MemoryDoc saves propagate workspace content-write failures. Persistence must
  succeed before publishing the document/cache path or deleting a rename's old
  file. Writes are serialized through a store-local lock so cache publication
  follows durable write ordering. Workspace-less stores remain in-memory.
- `save_internal_memory_doc` is a separate native-only entry point for compiled
  seeds and engine bookkeeping. Ordinary saves cannot bypass the control-doc
  gate using `source: compiled_in` or the failure-tracker title.

The router dual-writes compatibility conversation history to the relational
`Database` separately. That database is not the backing store for `HybridStore`.

### Channel Routing And Delivery

`should_route_to_engine_v2()` owns the rollout policy. `ENGINE_V2=false`
disables every Engine V2 route. With Engine V2 enabled, the gateway is always
eligible; `ENGINE_V2_CHANNELS` can additionally select the exact names `xmpp`,
`darkirc`, and `weechat`. Values are comma-separated, trimmed, and matched
case-insensitively by exact channel name. Empty and unknown values are ignored.
`ENGINE_V2_MODEL_RECOVERY_ATTEMPTS` is another env var
example: `ENGINE_V2_MODEL_RECOVERY_ATTEMPTS=2`. Note there is no opt-out
value: any entry that is not one of the eligible channel names — including
something like `none` — is simply ignored, and the gateway always routes
while `ENGINE_V2=true`. The only durable opt-out is `ENGINE_V2=false`.

Engine `ResponseDelta` events flow through
`ChannelManager::send_status(StatusUpdate::StreamChunk)`. The gateway maps each
chunk to SSE. WASM channels deliberately treat chunks as a no-op because the
current WIT has no message-editing contract. `router/outcome.rs` transforms
completed terminal text before recording engine and compatibility history. The
agent's outbound handler then uses the shared `ResponseDelivery` path:
`BeforeOutbound`, suppress-empty, optional coordination coalescing, and
`ChannelManager::respond()` with the original incoming metadata.

At the initial 610-second outcome deadline, delivery transfers to an owned late
watcher, retaining the engine event receiver and original reply route. The caller
returns an empty sentinel and emits a waiting status, not a terminal reply. The
thread stays interruptible in `ThreadManager` while the watcher drains progress.
On completion it joins once and uses the same transform, history, authentication
fallback, and durable gate handling as an on-time result. Authentication-marker
text is subject to response hooks; only a valid credential request in the
transformed output becomes an auth gate. It is not recorded as assistant prose.

The watcher has a second 600-second budget, then requests stop with a bounded
60-second grace and reports a terminal timeout if its stop succeeds. It never
joins a still-running handle, which would hide it from
`/interrupt`. Cancellation failure reports an error and leaves the live handle
addressable. These are in-process delivery guarantees; the watcher is not a
durable cross-restart outbox.

Engine `message` tool calls receive execution-local `notify_thread_id`; gateway
calls also carry their execution's user/channel instead of mutable conversation
defaults. Proactive gateway delivery therefore meets the channel's required
thread-routing contract.

If a gateway SSE or WebSocket subscriber falls behind and the broadcast ring
drops events it would have received, the subscription converts the lag into an
explicit `stream_gap` event (carrying the missed-event count) instead of
silently swallowing it; the browser clears partial live-stream state and
reloads persisted history on receipt (WI#127 server-side gap signaling).

Rate-metered channels (DarkIRC) carry a host-side message budget
(`ic/src/channels/budget.rs`). The bridge injects the budget's guidance into
new engine threads via `ThreadConfig::channel_context` (rendered as a
"Channel Constraints" system-prompt section — the engine crate stays
channel-neutral), and the bundled `BeforeOutbound` hook
(`builtin.channel_message_budget`) truncates over-budget terminal responses at
a UTF-8-safe boundary with an explicit continuation marker before delivery.

Approval, authentication, interrupt, steer, clear, and new-thread controls use
the same user and engine-conversation scope as ordinary input. This includes
non-UUID XMPP, DarkIRC, and WeeChat scope keys. `/interrupt` cancels in-flight
provider streams and tool futures before signaling the next loop boundary.
When an interrupt overtakes a prompt still queued in the daemon's message
scheduler (no engine thread yet), the queued prompt is cancelled in its
conversation lane and the user gets `Interrupted.`; queued follow-ups behind a
running turn are kept — the engine stops the running turn and the follow-ups
run afterward.
`/steer <note>` injects into a running thread at its next model-call boundary;
when the conversation is idle or completing an interrupt, up to four notes are
persisted on the conversation and injected into its next foreground turn,
including resume from a waiting or suspended thread.

Steer is deliberately **not** a tool-preemption control (the WI#105 scope
decision, see `docs/ops/v2.1.1.0_GOAL18_WI105_STEER_PREEMPTION_CONTRACT.md`).
A note queued while a tool future is pending does not abort that call: the
inject signal only surfaces at the next `__check_signals__` poll, after the
in-flight tool's result. Only `/interrupt` (`ThreadSignal::Stop` / the run
token) cancels an in-flight tool. To stop a long-running tool, interrupt; to
redirect the turn afterward, steer — an interrupt followed by a steer note
lands the note on the next turn.
Approval and auth prompts are statuses; gate pauses, auth pauses, and stopped
turns return an empty no-reply sentinel so the outer handler does not send or
persist a duplicate terminal response.

### Workspace Prompt Editing and Lifetime (WI#220)

The bridge reads `agent.workspace().system_prompt()` once during thread-manager
initialization and supplies it through `set_identity_preamble()`. This uses
direct-context composition with UTC daily dates. The executor assembles its
complete prompt only when the thread has no System message; existing persisted
threads retain that message on reuse.

The gateway's **Settings → System Prompt** editor writes primary-scope workspace
documents. Its authenticated `/api/prompts/preview` endpoint independently
composes current saved workspace content with optional group/timezone inputs.
It is not a live engine snapshot and excludes separately assembled engine
templates, tool documentation, skills, channel constraints and runtime overlays.
For edited workspace instructions to reach Engine V2, restart the tenant via
mt-admin and start a new conversation. See
[Tenant System Prompts](../guides/TENANT-SYSTEM-PROMPTS.md).

### Attachment Input

Engine V2 parses submissions and resolves pending authentication against the
original message text. Only an ordinary user-input turn is then augmented with
the host's sanitized `<attachments>` representation. The effective text includes
document extraction and audio transcription, excludes source URLs and host
storage paths, and passes through input validation, policy checks, and inbound
secret scanning before the engine starts.

Images with bounded channel-provided bytes become engine
`TransientContentPart::Image` values. These parts and their opaque lookup IDs
are skipped by serde and use redacted debug output. The Python orchestrator
carries only the opaque ID through its working transcript; `LlmBridgeAdapter`
converts bytes to provider-native data-URL content at the final provider
boundary. Durable engine state, traces, and V1 compatibility history retain the
sanitized effective text but never image bytes or data URLs. Consequently, a
process restart preserves attachment descriptions and extracted text but not
visual bytes; an image must be resent if visual analysis is needed afterward.

Separately from engine state, the host-side durable attachment store
(`crate::agent::attachment_store`) persists every inbound attachment —
sanitized metadata, extracted text, and bounded inline bytes (base64, capped
per attachment) — into workspace documents under `attachments/<channel>/...`
so a file sent on one channel stays retrievable from all other channels and
the WebUI. Stored documents go through the normal workspace write path
(prompt-injection scan, search indexing).

#### Action-result images

Multimodal tool output (MCP servers returning `image` content blocks) uses
the same transient delivery. The host effect adapter decodes bounded,
allow-listed images out of the tool result and returns them on
`ActionResult::transient_images` — a serde-skipped field, so sealed step
records and checkpoints never contain image bytes. The executor stashes
them on the thread keyed by the action's `call_id` (newest three calls,
~8 MiB total, runtime-only) and attaches them to the matching
`ActionResult` message for each provider request until evicted. The
orchestrator never sees image data; Python working transcripts and
sanitized tool-output text are unchanged. Restart or recovery drops the
stash, degrading to text-only results. Error results and gate pauses never
carry images.

Attachment count, MIME, per-file, and aggregate limits remain owned by the
ingress channel/host implementations. Attachments on approval, interrupt, and
credential replies stay on those control paths and are not added to model
history or copied into authentication retries.

### Local Compatibility Proof

The hermetic local compatibility matrix exercises production boundaries rather
than source-only stand-ins: real MCP HTTP transport and OAuth resume, Wasmtime
execution and capability denial, v1-to-v2 skill migration and deterministic
selection, TensorZero-shaped RigAdapter SSE streams, channel routing, and the
browser gateway. Gateway text is incremental through SSE; the current WASM
channel contract remains final-response-only and ignores stream chunks.

These tests do not prove a deployed XMPP, DarkIRC, or WeeChat protocol bridge.
Those channels remain gateway-first and exact-opt-in, with live validation kept
in the separate disposable-tenant rollout.

## External Trait Boundaries

The engine defines three traits that the host crate implements. This boundary ensures the engine has no dependency on the main daemon crate and is testable in isolation.

| Trait | Signature | Host wraps |
|-------|-----------|------------|
| `LlmBackend` | `complete(...) -> LlmOutput`; `complete_stream(...) -> LlmStream` | `LlmProvider` |
| `Store` | 34 CRUD methods for all engine types | Workspace-backed `HybridStore` |
| `EffectExecutor` | `execute_action(name, params, lease, ctx) -> ActionResult` | `ToolRegistry` + `SafetyLayer` |

## Data Retention Policy

Thread messages, steps, and events are **never deleted** from the engine
workspace store. Runtime JSON lives under `engine/.runtime/`; knowledge and
orchestrator artifacts live in human-readable paths under `engine/`. Raw
attachment bytes are the deliberate exception: only sanitized attachment text
is durable, while provider-bound image parts remain transient — except for the
bounded inline copies persisted by the durable cross-channel attachment store
under `attachments/` (see Attachment Input). The router also
dual-writes V1-compatible conversation history to the relational database.

Webhook mission documents contain only versioned secret references. Plaintext
webhook secrets live in the encrypted host `SecretsStore`, are hydrated into
runtime memory when Engine V2 starts, and are omitted from serialization and
debug output. Startup migrates legacy plaintext mission documents in place so
workspace search chunks no longer contain the credential.

"Cleanup" of terminal threads means writing a compact archive and evicting
in-memory cache entries to bound RAM; workspace files remain. `load_thread()`,
`load_steps()`, and `load_events()` reload persisted files on a cache miss.

## Event Sourcing

Threads persist significant lifecycle and execution events via `ThreadEvent`.
The 20 `EventKind` variants cover state, step, action, lease, message, child,
approval, self-improvement, skill-activation, and orchestrator-rollback events.
`ResponseDelta` is broadcast for live delivery but is not persisted.

This enables:
- Inspection of recorded thread execution history (mid-thread replay/resume is
  not generally supported)
- Retrospective analysis via `executor/trace.rs`
- Learning mission triggers based on event patterns
- Debugging and auditing

## Configuration

```bash
ENGINE_V2=true                 # Enable Engine V2 (runtime default: false;
                               # multi-tenant provisioning writes true)
ENGINE_V2_CHANNELS=            # Gateway only (runtime default)
ENGINE_V2_CHANNELS=xmpp        # Add XMPP
ENGINE_V2_CHANNELS=xmpp,weechat # Add exact eligible channels
# Multi-tenant lunarwing.env default: ENGINE_V2_CHANNELS=xmpp,weechat,darkirc
# (existing tenant values preserved on re-render; ENGINE_V2=false is the only
#  durable opt-out — a value like `none` is an ignored unknown entry)
SKILL_SELF_IMPROVEMENT=false   # Gate skill feedback/patch/prune lifecycle and the
                               # self-improvement/skill-extraction/skill-maintenance
                               # missions (paused while off, resumed on re-enable)
ORCHESTRATOR_SELF_MODIFY=false # Runtime default: compiled orchestrator v0, no
                               # stored prompt overlays or model control-doc writes.
                               # Multi-tenant provisioning writes true for new tenants.

# ThreadConfig env overrides (read at thread spawn):
ENGINE_V2_TOOL_ASYNC_TIMEOUT_SECS=300   # Per-future timeout for deferred tool/LLM
                                        # calls awaited in a ResolveFutures round; on
                                        # expiry the task is aborted and the call
                                        # resolves as an error. Default: 300
ENGINE_V2_MODEL_RECOVERY_ATTEMPTS=1     # Model failure recovery attempts per step.
                                        # Default: 1; clamped to 0-3
ENGINE_V2_MODEL_RECOVERY_TOTAL_ATTEMPTS=4 # Total recovery attempts per thread lifetime.
                                        # Default: 4; clamped to 16
```

**Pending-gate expiry (host daemon knob, WI#111).** How long a pending
approval/auth gate waits before the background sweep expires it and resumes
the paused turn (WI#108) is configured on the daemon, not in `ThreadConfig`:
`AGENT_PENDING_GATE_EXPIRY_SECS` env (read via `AgentConfig::resolve`) takes
precedence over the DB-backed setting `agent.pending_gate_expiry_secs`;
default **1800s (30 minutes)**. Values outside the inclusive range **30s to
2592000s (30 days)** fall back to the default. The lower bound prevents windows
shorter than the 60-second gate sweep can observe; the upper bound prevents
unsigned-to-signed wraparound and Chrono duration overflow (WI#115).

**Prompt-overlay audit and operator lifecycle (WI#268).** The full contract —
overlay storage identity and bridge path, the three gated writers, consumption
semantics, the existing-thread retention caveat, safe enable/disable, and a
read-only tenant inspection runbook (`mt-admin daemon-pid` / `mt-admin psql`)
— is published in
`docs/ops/v2.1.4.0_WORK_ITEM_268_PROMPT_OVERLAY_AUDIT.md`. Key operator facts:
`ORCHESTRATOR_SELF_MODIFY` unset or any value other than `true`/`1` makes
stored overlays inert regardless of database contents; multi-tenant provisioning
writes `ORCHESTRATOR_SELF_MODIFY=true` for new tenants while preserving an
explicit operator opt-out on re-render. `SKILL_SELF_IMPROVEMENT` is a separate
flag and never authorizes prompt alteration; disabling the gate does not
rewrite System messages already persisted in existing threads; and a failed
audit probe (unresolvable daemon PID, unreadable `/proc/<pid>/environ`, errored DB query) is always classified
**uninspected** — never as evidence of gate-off or absence. On OpenRC the
daemon's live environment is probed via OpenRC's `child_pid` record — never
the supervise-daemon pidfile, which holds the supervisor's PID — and the
record is validated as-written with tenant-association checks on the resolved
process.

The engine inherits most configuration from the host daemon (LLM provider settings, database config, tool registry, safety settings). Engine-specific behavior is controlled through `ThreadConfig` at thread spawn time. `supervised_timeout_secs` (the fixed 300s fail-closed approval auto-deny) has no env override.

**WI#88 timeout policy — intentional, not deferred (closes Kageho's "double-timeout" review question).** The orchestrator's hardcoded policy for the two timeout surfaces that work item #88 exposed is by design: every deferred tool join and every deferred LLM join awaited in a `ResolveFutures` round is bounded by the *same* hardcoded deadline — `tool_async_timeout_secs` (default 300s, env escape hatch `ENGINE_V2_TOOL_ASYNC_TIMEOUT_SECS`) — so the round's two suspended surfaces, deferred tool joins and deferred LLM joins (the two hang shapes #88 reported), share one fixed "double-applied" deadline. On expiry the wedged task is aborted and the call resolves back to the model as an ordinary tool/LLM error; the orchestrator does not retry the future, re-arm it with a larger deadline, or split the deadline per surface. Christopher's answer to Kageho's "unblock the double-timeout knob (or document it as deferred)" review question is that this single fixed deadline covering both suspended surfaces is **intentional, not deferred** — a split knob (separate tool-vs-LLM deadlines) or a retry/re-arm-with-larger-deadline policy was considered and deliberately rejected — so future readers should treat the hardcoded 2-surface/one-deadline policy as a settled decision and not re-litigate it without fresh evidence that the 300s default causes real user harm.

## Build & Test

Run these commands from `ic/`:

```bash
taskset -c 0-11 cargo check -j12 -p lunarwing_engine
taskset -c 0-11 cargo clippy -j12 -p lunarwing_engine --all-targets -- -D warnings
taskset -c 0-11 cargo test -j12 -p lunarwing_engine -- --test-threads=12
```

## Key Design Decisions

1. **No dependency on main crate** -- clean separation, testable in isolation
2. **No safety logic in engine** -- sanitization/leak detection applied at the adapter boundary (`EffectExecutor` impl)
3. **Event sourcing from day one** -- threads persist significant lifecycle and execution events
4. **Tier 0 + Tier 1** -- structured tool calls and embedded Python via Monty coexist
5. **Engine owns its message type** -- `ThreadMessage` is simpler than `ChatMessage`; bridge adapters handle conversion
6. **RLM pattern** -- context as variable (not attention input), recursive `llm_query()`, compact output metadata between steps
7. **Fail-closed by construction** -- `GateDecision` has no `None` variant, `ResumeKind` is a closed enum
8. **Never delete LLM output** -- engine workspace files are preserved; only
   in-memory caches are evicted (V1 compatibility history is dual-written to the
   relational database)
