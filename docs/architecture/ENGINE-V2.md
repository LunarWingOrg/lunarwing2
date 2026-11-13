# Engine V2 Architecture

The V2 engine is a thread/capability/CodeAct execution model in
`ic/crates/lunarwing_engine/`, organized around five core primitives. The host
bridges it to the existing channel, tool, workspace, and approval systems;
legacy execution remains active on channels outside the rollout allowlist.

**Source revalidated 2026-11-10 for v2.2.0.0.** Read the behavior sections below
as the current contract. Relevant release changes include:

- Mission persistence: WI#317 serializes boot/reconciliation writes with fires;
  WI#321 warns when stored records cannot load and when default projects
  duplicate; WI#490 makes default-project creation idempotent; WI#322 reserves
  Engine project/mission documents from memory-tool and web-write mutation;
  WI#394 heals settled-stop dedup keys without refiring.
- Memory write-lane reservation: WI#455 (v2.1.7.0) widens the memory-tool and
  web-write reservation from `engine/projects/**` + `engine/.runtime/**` to the
  whole `engine/**` tree, so a forged `engine/knowledge/**` or
  `engine/orchestrator/**` document cannot shadow the id→path mapping
  boot-loaded by `load_knowledge_docs`. Reads/tree listings keep the narrower
  WI#322 reservation, so those Engine documents stay searchable. See
  SEMANTIC-MEMORY-SEARCH.md for the memory-tool view.
- Thread ownership: WI#502 (v2.1.7.0) pins thread ownership on store write
  (`save_thread` refuses to replace a record owned by a different real user),
  refuses a foreign `preferred_thread_id` at engine spawn, and closes the
  gateway forged-`thread_id` path — see the HybridStore section below.
- CodeAct: WI#364 pre-injects missing imports for the supported standard-library
  module set before Monty execution. Sealed step records remain inference-only.
- Gates: WI#392 shares tool-sensitive/HTTP display redaction across hooks and
  approval prompts; WI#473 drains turn-bound gates on terminal outcomes;
  WI#472 resolves approval threads server-side; WI#481 routes expiry replies to
  the gate's own gateway thread, with SSE only as delivery fallback there.
- Conversation continuity: WI#442 imports bounded pre-switch v1 history only
  into a newly created Engine surface after ownership checks. The import is
  best-effort and never replays v1 tool-call bookkeeping.
- Provider responses: WI#545/WI#572 make cache keys include canonical request
  metadata and a model identity known before dispatch. Smart routing and
  failover bypass the outer cache; empty, reasoning-only, corruption-flagged,
  and tool-call responses are not cached. Stream hits retain the original finish
  reason without replaying discarded retry usage.
- CodeAct final answers: WI#571/WI#585 consume uppercase `FINAL…</arg_value>`
  pseudo-actions before lease enforcement, recovering the supplied answer rather
  than looping on `No lease for action`. Ordinary structured action names such as
  `finalize_order` are unchanged.
- Mission load diagnostics: WI#576 adds an allowlisted `missing_field` to the
  boot data-error warning. Unknown variants, invalid values, and complete Serde
  errors stay out of WARN; full detail remains DEBUG-only.
- Retry cost bound: WI#578 adds a per-provider-leg cumulative input-token budget
  for billed-but-empty stream retries, configured by
  `LLM_EMPTY_STREAM_RETRY_INPUT_TOKEN_BUDGET` (see Configuration).

Source ownership: `runtime/mission.rs` and `runtime/manager.rs` in the engine;
`ic/src/bridge/{router,effect_adapter,store_adapter}.rs` and the `router/`
submodules in the host; `ic/src/gate/` for pending-gate storage; and
`ic/src/channels/web/handlers/chat.rs` for HTTP approval routing.

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
    event.rs              ThreadEvent, EventKind (22 variants for event sourcing)
    message.rs            ThreadMessage, MessageRole
    provenance.rs         Provenance enum (User/System/ToolOutput/LlmGenerated/etc.)
    conversation.rs       ConversationSurface, ConversationEntry, EntrySender
    mission.rs            Mission, MissionId, MissionCadence, MissionStatus
    insight.rs            Captured source evidence, insight records, durable batch cursor
    repl.rs               REPL argument validation and fenced-code parsing
    error.rs              EngineError, ModelFailureKind, ThreadError, StepError, CapabilityError
  traits/                 External dependency abstractions (host implements these)
    llm.rs                LlmBackend trait
    store.rs              Store trait (persistence, scoped listings, evidence-checked insights)
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
    insights.rs           Insight-mission source selection and lifecycle helpers
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
    insights.rs           Evidence validation and insight recall eligibility
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

The outer response cache (`ic/src/llm/response_cache.rs`, WI#545/WI#572) is off
by default (`RESPONSE_CACHE_ENABLED=false`). When enabled it defaults to one
hour and 1000 LRU entries. Keys cover the pre-dispatch model identity,
messages, canonical request metadata, and response-affecting parameters.
Providers that choose a model during dispatch (smart routing and failover) and
all tool-completion calls bypass it. Only responses with visible, uncorrupted
content and no tool calls are stored. A stream hit replays cached reasoning
before text and preserves the original finish reason and stream observations,
but does not replay usage from retries discarded before the cached response.

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

Provider reasoning follows a separate opt-in live path (WI#546): textual and
summary reasoning blocks become `LlmStreamChunk::ReasoningDelta`, cross the
host/engine bridge unchanged, and accumulate into live
`EventKind::ReasoningDelta` snapshots. After an immediate first snapshot, each
LLM stream emits at most one every 300 ms; pending reasoning flushes before
answer text, at `Done`, and on stream termination. Every snapshot is only the
trailing 16 KiB of the cumulative reasoning, cut at a UTF-8 boundary. The
router forwards these snapshots only for the gateway when
`ENGINE_V2_STREAM_REASONING=true` (or `1`); the default is off. The gateway
maps them to the existing `reasoning_update` SSE event with `decisions: []` for
both browser and TUI clients, which replace the narrative with the latest
bounded tail. Wingtip maps empty-decision snapshots to `turn.reasoning` and
replaces a sanitized multiline field on the matching turn, independent of the
tool-summary activity ring; one updating block is shown until the turn is
sealed. WASM channels (including XMPP) and the local REPL discard the
distinct internal `ReasoningStream` status, while `ReasoningUpdate` remains the
tool-selection summary. Encrypted and redacted provider payloads stay opaque
and are not rendered. Leading reasoning is replayed before final text by
stream opening, but does not suppress the existing empty-response or
output-budget retry when no final content arrives.
Blocking Rig responses preserve displayable reasoning separately and attach it
to their tool calls; worker proxy responses carry the same optional field.
`reasoning_tokens` is retained through host and engine usage accounting as a
subset of `output_tokens`, never as an additional term in token totals or cost.

`repl_argument_corruption_total` counts rejected responses per provider in
`runtime/llm-stream-metrics.json` (streaming and blocking paths). The existing
registry-aware healthpoint aggregates it by tenant and exports
`lunarwing_health_llm_streams_repl_argument_corruption_total{provider,tenant}`.
Old snapshots without the field read as zero. Like other stream-integrity
signals, this is monitor-only. See the
[Goal 34 verification record](../ops/history/GOAL34_REPL_ARGUMENT_INTEGRITY.md).

### Two Execution Tiers

**Tier 0 -- Structured Tool Calls** (`executor/structured.rs`):

Standard tool/function calling. The LLM returns structured `ActionCall` objects. For each call:
1. Find the capability lease that grants this action
2. Check the policy engine (allow/deny/approve)
3. Consume one use from the lease (if use-limited)
4. Execute via `EffectExecutor`
5. Record the `ActionResult`

Before lease lookup, `LlmResponse::from_action_calls` consumes CodeAct
final-answer pseudo-actions (`FINAL`, `FINAL(...)`, `FINAL_VAR`, and malformed
uppercase `FINAL…</arg_value>` names). An all-FINAL batch becomes text carrying
a parseable `FINAL(answer)` using common answer fields or text embedded in the
malformed name; FINAL-shaped junk mixed with real capability calls is dropped
while the real calls continue. The malformed-XML arm requires uppercase `FINAL`
at the start, so an ordinary action such as `finalize_order` remains structured
(WI#571/WI#585).

`ActionResult` content is role-token-neutralized and bounded to
`MAX_ACTION_OUTPUT_CHARS` (8000) by the orchestrator's
`format_action_output` before entering LLM history. The bound keeps head AND
tail and elides the middle (WI#349): list-tool envelopes serialize
pagination/completion metadata (`next_cursor`, `truncated`, `complete`,
`resume_page`) at the very tail, and the earlier head-only cut amputated it —
the model never saw truncation state or a real cursor and improvised one
(WI#344 GitLab 500s). The kept budget splits evenly between head and tail: a
pagination block needs only ~100-300 chars of tail, while head-heavy outputs
(HTTP bodies, file reads) still keep the front half. The Tier-0 string caps
and the routine cap use head+tail truncation:
the safety layer's `sanitize_tool_output` size cut
(`SAFETY_MAX_OUTPUT_LENGTH`, 100 KB default) covers the v1 agent live loop,
the v1 routine loop, and the engine effect bridge (the v1 live loop is capped
there, not uncapped as WI#349 stated), and the v1 routine tool loop adds its
own 8192-byte cap (`ic/src/agent/routine_engine.rs`). Safety truncation scans
bounded 2 KiB windows around both cuts and removes crossing secrets/token
runs. When a run crosses a window edge, the cut walks to the run boundary with
a plain byte loop (no regex over the run) and scans one more bounded window
there — at most two 2 KiB windows per cut
([WI#355](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/355)).
Only a second window that still cannot establish a safe boundary discards the
retained side, so a long inline base64/hex blob (e.g. an inline image in a
list item or a repository-file `content` field) no longer costs the
pagination block or `file_name`/`ref`/`blob_id` metadata. The final retained
text is still leak-scanned; PEM BEGIN and END markers block.
Thus secret protection can reduce the retained payload below its budget.
The notice separates itself
from retained text with blank lines and describes "retained edges", since
either edge can be empty. The remaining detector gap is tracked separately:
[WI#354](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/354)
for END-less PEM tails and encrypted/PGP private keys. The WI#353 gap is
closed: `glpat-` GitLab PATs have their own Block pattern, and the Bearer
and Authorization-header value classes cover the full token grammar
(`.`-separated JWT segments, `~`-separated tails, base64 `+`/`/`/`=`), so
uncut tokens are blocked or redacted completely rather than only at cuts.
The Bearer scheme match is case-insensitive, the truncation-cut snapper
treats `~` as part of a token run, and a `glpat-` body with fewer than
three distinct characters (documentation placeholders) is not flagged, so
fetched GitLab docs are not blocked wholesale.
WASM (10 MB), MCP (4 MiB), and built-in HTTP (5 MiB) size limits reject the
call rather than truncate; tail preservation is not an end-to-end guarantee.
Follow-ups that still
cut head-only: `shrink_context_for_model_recovery`'s per-message cut after a
context_overflow recovery (the model again sees `{items: [...` with no
cursor), the stored-result preview in `ic/src/agent/thread_ops.rs:1018-1019`,
the Engine v2 500-byte preview in `ic/src/bridge/router.rs:4264-4294` built
from the 1000-byte event preview in
`ic/crates/lunarwing_engine/src/types/event.rs:235-248`,
shell capture (64 KiB), SSH capture (1 MiB), and sandbox stdout/stderr caps.
The v1 earlier-turn 1000-byte tool-result cut in
`ic/src/agent/session.rs:540-550` is no longer on that list: since
[WI#351](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/351) (fixed 2026-10-12) it keeps the head and
the tail, so a later "next page" turn still sees its cursor. Still to follow
up: CodeAct's 500-character `format_output` action previews in `default.py`. `MAX_ACTION_OUTPUT_CHARS` still has no
ThreadConfig/env knob.

**Tier 1 -- CodeAct / Python via Monty** (`executor/scripting.rs`):

Embedded Python execution following the RLM (Recursive Language Model) pattern. The LLM generates Python code that the Monty interpreter executes.

Key characteristics:
- **Context as variables**: thread messages injected as `context` Python variable, thread goal as `goal`, step index as `step_number`, prior action results as `previous_results` dict
- **Tool dispatch**: unknown function calls suspend the VM, go through lease check + policy check + `EffectExecutor`, then result returns to Python
- **Recursive LLM calls**: `llm_query(prompt, context)` suspends the VM, spawns a single-shot LLM call, returns text as a Python string. Results stay as variables (symbolic composition), not injected into the parent's attention window
- **Compact output metadata**: between code steps, only a summary is added to chat context (`[FULL OUTPUT: N chars]`, `[TRUNCATED: last 8000 of N chars shown]`, a short return-value preview, or `[code executed, no output]`) to prevent context bloat
- **Explicit termination**: `FINAL(answer)` or `FINAL_VAR(name)` for output
- **Missing-import repair (WI#364)**: before each model-generated block reaches
  Monty, `inject_missing_imports` adds missing bare imports for dotted references
  to `asyncio`, `json`, `re`, `math`, `datetime`, and `sys`. Imports do not persist
  across fresh Monty runs. A differently named alias or `from module import name`
  does not bind the module's bare name. A bare function call such as `json(...)`
  suppresses injection for that name so the built-in tool is not shadowed.
  Error line numbers are shifted back to the original block. The line/boundary
  scan is not a Python parser and can add an extra import for literal text;
  arbitrary modules and OS access remain unavailable.
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
  recovery instead gets the policy's full shrink ladder (two attempts, 0.60 then
  0.35 of the recovery budget) regardless of `ENGINE_V2_MODEL_RECOVERY_ATTEMPTS`
  unless that is `0`: each attempt replaces protocol history with a bounded
  plain-text view of the original goal and recent context. The failure after
  the last allowed attempt becomes a typed `ThreadFailureKind::Model` outcome
  with actionable user text; `ENGINE_V2_MODEL_RECOVERY_TOTAL_ATTEMPTS` bounds
  all of this per thread lifetime (mt-admin tenants are provisioned with 2/2,
  see `docs/ops/TENANT-CONFIGURATION.md`).
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
  text names the operator action required. The same fail-fast rule covers the
  two deterministic provider-empty kinds (WI#297): `content_filtered` (the
  provider refused; rephrase or switch models) and `output_budget_exhausted`
  (the model spent its whole output budget before visible text). Both are
  produced by the host retry/failover empty-stream detection and reach the
  engine as their own kinds, never as the generic rejected-request text.
  The output budget is the host adapter's per-call `max_tokens`:
  `ENGINE_V2_MAX_OUTPUT_TOKENS` (default 8192; a fixed 4096 before 2.1.5.0)
  unless the orchestrator's call config sets one. Thinking models count their
  reasoning against it, so the adapter retries an exhausted budget once at
  twice the budget before the kind reaches the engine — surfaced to the model
  as a `Retried` provider observation that carries the discarded billed usage
  — and only a second exhaustion ends the turn with the terminal text (raise
  `ENGINE_V2_MAX_OUTPUT_TOKENS`, 16384 or more for thinking models, or switch
  models). A stream that produced visible text before hitting the budget is
  not an empty stream and is delivered as-is; the adapter only logs that
  truncation at `warn` (model, budget, token counts) so it is visible in the
  daemon log.
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
   `$LUNARWING_BASE_DIR/pending-gates.json`. Writes are atomic: state goes to
   a unique `pending-gates.json.tmp-*` sibling (mode 0600 — the file carries
   unredacted tool parameters), is fsynced, and is renamed over the live
   file, so a crash mid-write keeps the last complete state; a best-effort
   directory fsync follows the rename. Mutual exclusion uses a persistent
   `pending-gates.lock` sibling because rename replaces the data file's
   inode, which cannot carry the flock. The data file exists only after the
   first save. An unparseable live file is renamed aside to
   `pending-gates.json.corrupt-<timestamp>` (warn-level log naming both
   paths) and reads fall back to empty state so saves proceed; boot
   reconciliation then fails the orphaned Waiting threads — the designed
   degraded path.
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

**Approval state in `GET /api/chat/history` (WI#520).** The `pending_approval`
envelope is populated for Engine V2 threads too. Engine V2 dual-writes history
and leaves the legacy in-memory session turn untouched, so before WI#520 the
field was `null` for the whole life of a Live Engine V2 approval gate and a
client polling only history could never learn the `request_id` required by
`POST /api/chat/approval` — a live highwinds gate expired at its 30-minute
window that way. Both history handlers now run
`bridge::get_engine_pending_approval(user, thread)` for every return path
(including paginated and DB-backed history) and map the matching gate into
`PendingApprovalInfo` (`request_id`, `tool_name`, `description`, and the
**redacted** display parameters; the executable parameters never reach the
envelope). The lookup is:

- **user-scoped** — another user's gate is invisible, and the endpoint still
  never resolves another user's gate on approval;
- **thread-exact** — the gate's own `thread_id` must equal the requested
  thread, so a sibling thread that only shares the conversation cannot surface
  an approvable id;
- **live-only** — expired gates and gates parked on a thread that is gone or
  no longer `Waiting` are dropped (the same WI#332 filter the pending-gates
  endpoint uses);
- **approval-only** — `ResumeKind::Approval` gates. Authentication and
  external gates are not resolvable through the approval endpoint and appear
  only in `pending-gates`/SSE.

A gate-store read failure degrades to `null` with a warn-level log: history must
still render. `null` therefore means "no live approval gate for this thread in
this envelope", not "nothing is pending anywhere" — automation that needs every
gate (auth included) must poll `GET /api/chat/pending-gates` or follow the
`approval_needed` SSE event, exactly as the browser UI does.

Pending gates are filtered by live thread state (WI#332): a gate parked on a
thread is only listed while that thread exists, is still `Waiting`, and belongs
to the gate's user — the same validity rule boot-time reconciliation enforces.
Authentication-fallback gates are exempt: they are inserted after their thread
completed and are resolved by re-dispatching the original message, so they
remain listed until expiry. Terminal turn outcomes synchronously drain that
thread's turn-bound gates, including already-expired entries, while paused
turns keep their gates (WI#473). The 60-second pending-gate sweep remains the
backstop for threads terminated outside the shared completion path and for
ordinary expiry handling. Both cleanup paths log each discard at warn level
with user/thread/request ids and broadcast `gate_resolved` with resolution
`thread_terminated` so open approval prompts clear. Their discards are
request-id checked so a replacement gate parked after the cleanup snapshot is
never removed. A gate resolution that races a thread termination fails closed:
the gated action is not executed and the caller is told the thread ended. An
already-delivered live `gate_required` SSE card remains visible until the
terminal-outcome broadcast or, for out-of-band termination, the next sweep
broadcast. Approvals on engine-routed channels that match no engine gate fall
through to the legacy path with a warn-level log naming the request id and
conversation scope, so an unmatched approval is diagnosable from logs.

Approval prompts and `ToolCall` hook payloads use the shared
`redact_display_params` path (WI#392): registered tool-sensitive fields and HTTP
headers, URLs, and bodies are redacted, including JSON embedded in string-form
arguments. An unregistered-tool fallback still applies the shared credential
redactor. This protects display/hook copies; the mode-0600 pending-gate file
retains executable parameters, and transcript storage has its own at-rest
exposure class. A redacted prompt is not evidence that credentials were removed
from every persisted surface.

`POST /api/chat/approval` resolves the owning thread server-side (WI#472).
`thread_id` is optional: the handler looks up the live approval gate by
`request_id` — engine pending gates first, then the caller's own legacy
session — and scopes the submission to that thread, so `approve`, `always` and
`deny` all reach their gate without the caller supplying it. Both lookups are
user-scoped, so an approval never resolves another user's gate, and resolution
still re-checks ownership in `take_verified`. A request id that matches no live
approval returns 404 instead of the previous `202 accepted` no-op (which left
the gate pending, most dangerously for `deny`); a supplied `thread_id` that is
blank or does not name the resolved owner returns 400.

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
memory; pruning failure is logged and never revives an expired approval. A
restore failure (real I/O error, e.g. an unreadable state file) is logged at
warn level with the file path — never at debug, where a corrupt store would
be invisible in production logs (WI#299).
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

Five-field cron is `minute hour day-of-month month day-of-week`; six fields
prepend seconds and seven append year. The mission tools use UTC: daily 10:15
is `15 10 * * *` or `0 15 10 * * *`, whereas `0 10 15 * * *` is **15:10 UTC**.
Creation, listing and history responses include `project_id` and `schedule`
(cadence, effective timezone, cron field order, and the persisted
`next_fire_at`). Creation also returns the actual `mission_status`, including
when name deduplication returns an existing mission. The next-fire timestamp
is not recomputed for display: an overdue value remains visible. It describes
the stored schedule, not proof that the ticker is healthy. Webhook secrets are
excluded from these diagnostics.

`never_fired` means neither a last-fire timestamp nor thread history exists.
Older persisted missions may have history but lack `last_fired_at`; they report
`never_fired=false` without fabricating a timestamp. A terminal status plus a
missing timestamp alone does not establish that a mission was born terminal.
Tool listings remain project- and owner-scoped (including shared missions),
so absence from another project's list does not establish deletion.

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

At daemon startup, `HybridStore::load_state_from_workspace` reconstructs its
mission map from persisted workspace records; `bootstrap_project` and
`bootstrap_all_projects` rebuild the manager's active index. Ticks iterate that
index and reload each mission from the store, rather than rescanning workspace
files on every minute. Creation and resume register missions in the same
manager. A foreground conversation/session change does not own or recreate
this scheduler. Goal 42's [investigation](../ops/v2.1.5.0_GOAL42_MISSION_DIAGNOSTICS.md)
records the build and cron interpretation behind the WI#311 follow-up reports.
An externally incomplete or malformed mission document is still dropped rather
than reactivated with schema defaults. Its boot WARN carries the validated
mission id, operator path, `error_kind`, and a `missing_field` that names the
field only when it is in the `MISSION_SCHEMA_FIELDS` allowlist and is `unknown`
otherwise;
unknown variants, invalid values, and full parse errors are confined to DEBUG so
user-authored goal text cannot leak into ordinary logs (WI#576).

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
be set through the persisted `retry_policy`). The interruption path above
describes a mission that is still Active: a thread signal-stopped while its
mission is no longer Active (the operator pause that stops its workers, a
retirement) is instead recorded quietly as `STOPPED: mission <status> while
the thread ran` — no error notification, no retry (below). Each retry respawn passes
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
Recovery considers only the latest mission fire: older markers cannot replay
work after a newer success. Timer admission retains the durable marker until
spawn and is deduplicated within the process, so another restart during backoff
does not silently lose the retry.

Budget exhaustion (tokens, time, or cost) produces `Failed(BudgetExhausted)`,
never completion, and does not schedule a fresh-budget mission retry. Missing
or unknown orchestrator outcomes fail as `Orchestrator` and count toward
rollback. Stopped engine threads persist as Failed while retaining the Stopped
runtime outcome. Legacy workers persist cancellation on stop and bound three
consecutive unconfirmed text-only responses as failure, rather than success.

Restart recovery for in-flight threads (work item #301): at boot,
`ThreadManager::recover_all_threads_global` sweeps every project's persisted
threads — a per-project sweep used to leave non-owner projects' in-flight
threads as permanent `Running` zombies. Threads holding a
`runtime_checkpoint` are suspended for checkpoint resume; `Waiting` threads
are spared unconditionally because pending-gate reconciliation owns them (it
restores the persisted gate or fails gateless ones itself). Every other
non-terminal thread is failed with reason "engine restart before thread
completion" and stamped with a permanent `restart_failure` metadata marker,
distinct from Failed-by-error. `MissionManager::record_restart_interrupted_missions`
then records each restart-lost mission fire in the mission's approach history
("INTERRUPTED: engine restart before thread completion") and emits an error
notification to the mission's `notify_channels`; a persisted
`restart_failure_recorded` flag acknowledges the saved history. History is saved
first and contains the thread ID for crash-safe deduplication; a failed save
leaves the acknowledgement unset for the next boot. Notifications are best-effort
and may repeat after a crash before acknowledgement. Restart-lost fires are
deliberately NOT auto-refired: the interrupted
run's side effects are partially applied and payload-less cron/manual fires
carry no dedup key, so an automatic re-fire could duplicate real work.
Refiring is an operator decision — manual re-fire, or the next cadence tick
for cron missions.

A graceful daemon shutdown while a mission thread is mid-run follows the
retry path instead (WI#276). The outcome watcher waits with
`ThreadManager::wait_thread_or_stop`, which keeps the thread registered in
the running set while it runs — the old watcher joined first, taking the
handle out of that set, so its shutdown stop never landed, the run was
dropped mid-await at runtime teardown, and the fire vanished without a
record, retry, or dedup-key release. On `MissionManager::shutdown` the
watcher now stops the run and gives it 3 s to settle (the shutdown wait is
the grace plus a margin for the post-stop writes; the stop request itself
shares the grace budget so a wedged signal channel cannot stall it); the
real outcome is then processed exactly as in normal operation — for a
stopped run that records `INTERRUPTED: signal-stopped mid-run`, releases
the trigger dedup key, and persists the retry marker on the now-terminal
thread. The in-process retry timer is skipped (it would die with the
daemon); the post-restart sweep re-arms the retry from the marker, with the
usual fresh backoff and attempt accounting (an interrupted fire costs one
retry attempt). Outcome writes are ordered so the retry marker is the final
commit write: the mission row (approach history plus the dedup release) is
saved first, so an abort in the window leaves either no mission-row update
or a mission row without a marker (the key already released for redelivery)
— never a marker whose release never saved. The no-update leg is not always
the clean pre-WI#188 loss: the settled stop persists the thread row
`Failed("execution stopped")` before the watcher's mission save, so an
abort in that narrower span can leave the dedup key recorded on the old
mission row with no retry marker or shutdown stamp. At the next boot,
`recover_pending_mission_retries` releases that key (WI#394): the candidate
must be the mission's newest history entry, a Mission thread with matching
project/owner, `Failed` with a host-stop terminal reason, and neither a
retry marker nor a restart-failure marker, and no interruption stamp for
that thread. The mission and thread are reloaded and the newest-fire and
payload checks repeated under the fire lock, so a concurrent fire cannot
lose its own key or have a stale snapshot overwrite it. The heal runs for
every mission status and independently of retry policy; it never schedules
a retry or emits a notice. The lost fire stays record-only, while an
external redelivery of the same payload can fire again. Stamped fires stay
with shutdown resolution, restart-failed fires with WI#301, and superseded
fires untouched. The abort-window test
`outcome_processing_writes_no_marker_when_the_mission_save_fails` pins the
input state and the `settled_stop_abort_heal_*` tests pin the recovery.
Markers written in the opposite order by older builds can still pair a live
marker with a recorded key; the sweep releases that stale key before
re-arming.
The shutdown-time "retry scheduled" broadcast has no live consumer, so the
marker records the notice as undelivered and the boot sweep emits it once
when re-arming, acknowledging the delivery on the marker itself (the same
pattern as the WI#301 acknowledgement). A non-retry outcome settling in the
same window — a completion, MaxIterations, budget exhaustion, or an
exhausted retry chain — has no marker to carry that flag, so the owed
notice is stamped on the mission row as `pending_outcome_notification`
(keyed by thread id) and the boot-time
`deliver_pending_outcome_notifications` sweep emits it once the bridge
consumer is wired, with the stamp removal as the acknowledgement (WI#339).
A run that finished during the shutdown window has its real outcome
recorded too. A run that does not settle within the grace cannot have its
thread row written (its own final save lands during teardown), so the
watcher records the mission row only —
`INTERRUPTED: shutdown while the thread was still stopping (<thread_id>)`,
the dedup release, and a `pending_shutdown_interruption` stamp in mission
metadata, keyed by thread id so two runs of one mission both unsettled at
shutdown each keep their stamp (WI#339; the pre-WI#339 single-slot shape is
still read and is folded into the map on the next write). At boot,
`resolve_pending_shutdown_interruptions` resolves each stamp against the
thread's final persisted state — one unreadable row or unresolvable stamp
warns and skips rather than aborting the sweep (WI#339): a stop that
settled during teardown (host-stop terminal reason) earns the retry the
watcher would have planned, with the boot-time notice; a thread the
restart sweep
stamped stays WI#301's record case; a run that completed needs no retry; a
run that failed on its own during teardown is record-only, because the
persisted row does not carry the failure kind and WI#302 forbids retrying
budget exhaustion; a suspended or waiting thread stays with checkpoint or
gate recovery. When two overlapping fires are both stamped, only the
newest earns the retry: the older one is a superseded fire (below) and is
resolved record-only, matching the retry sweep's one-pending-retry rule
(the retry payload is the mission's last trigger payload, so re-arming the
older fire would re-run the newer fire's work). A stamp whose resolution
fails (for example, its mission save errors) is restored in memory before
the mission's next stamp resolves, so a sibling's successful save never
persists its clear; it stays for the next boot.

The stamp is also cleared WITHOUT a retry whenever the fire's future
already has an owner (WI#337): the stamped thread carries a persisted
retry marker (the retry is already armed — the boot sweep re-armed it this
boot, or the pre-crash watcher persisted it before dying), the thread is
`Running` again (checkpoint recovery resumed it ahead of the resolution,
so its live watcher owns the outcome), or the stamped thread is no longer
the mission's newest fire (a later fire superseded it). Resolving any of
those into a fresh retry would arm the same retry twice — payload-less
cron/manual fires would then run twice, contradicting the "suppresses the
armed retry instead of doubling" invariant — so the resolution only
clears the stamp and records a `SHUTDOWN RESOLUTION: ... already owned
...; not auto-refired` approach entry naming the owner. A stamp is also
resolved record-only when its entry is older than 24 hours
(`MAX_SHUTDOWN_INTERRUPTION_STAMP_AGE`; the age is read per thread from the
map entry, and a legacy single-slot stamp supplies its top-level
`recorded_at` for the thread it names): the resolution is a next-boot
affair, and a stamp that old was left behind by an older build or by a
downtime long enough that cadence or operator re-fires already superseded
the interrupted fire. A missing or unparseable `recorded_at` counts as
stale (every writer stamps it, so its absence means the stamp is
malformed); a future-dated `recorded_at` is never stale, so clock skew
cannot silently drop a fire. The stale and already-owned clears are
deliberately silent toward channels — no interruption notice is emitted
for a fire the mission does not re-arm — and both are recorded in the
approach history. Note the asymmetry with the marker path: the boot sweep
re-arms a persisted retry marker of any age, so after a downtime longer
than the stamp window a fire that settled within the grace is still
retried (marker path) while one that settled after the grace is
record-only (stamp path).

For the settled-stop arm the resolution's durable writes follow the
outcome processor's Q-4 order: the mission row — stamp cleared — saves
BEFORE the retry marker, so no crash prefix can leave a marker beside a
still-stamped mission (the combination the next boot's sweep and
resolution would otherwise arm twice). The trade-off is an accepted loss
window: a crash between that save and the marker write loses the fire
silently, the same accepted class as the pre-WI#188 loss (no trace, or a
mission row without a marker) — with the already-owned guard above, the
reverse order would be lossless only for the double-arm it used to risk,
so the mission row stays the first write. The interruption notice is sent
only after the marker persists, so a crash can never leave a delivered
"retry scheduled" notice without the persisted retry behind it; the
notice's acknowledgement rides a second marker write, and a crash before
it simply lets the next boot's sweep re-emit the notice. A retry armed by
the resolution is admitted into the same process-local `recovered_retries`
set the sweep uses, so a later sweep pass in the same process cannot arm
it a second time.

Because the watcher no longer hides the thread, stop
requests, injected messages, and `is_running` now reach watched mission
threads during normal operation as well — so pausing or retiring
conversation-insights now really stops its workers. A thread stopped while
its mission is no longer Active is recorded as `STOPPED: mission <status>
while the thread ran` and is neither retried nor reported as an error.

Whether an interrupted fire re-fires therefore depends on how it was
interrupted, and the asymmetry is deliberate — but the stop itself is NOT
cooperative. `request_stop` cancels the run token, so any active provider
stream or tool future is dropped mid-flight; a gracefully stopped run's
side effects can be partially applied exactly like a restart-killed run's.
The difference is observability, not cleanliness: the graceful path records
the real outcome and releases the dedup key before the process exits, so
the retry policy can re-arm with attempt accounting and a released trigger,
while a restart kill is discovered only at boot with no outcome and stays
record-only (WI#301). Re-firing remains at-least-once in both cases: side
effects performed before the interruption may repeat, and for payload-less
cron/manual fires the armed retry can run beside the next cadence tick —
accepted under the WI#188 retry contract, which already retries failed
fires on the same terms. Webhook/event fires carry a dedup key, so an
external redelivery suppresses the armed retry instead of doubling the
work. Operators of side-effecting missions should treat an interruption
retry like any at-least-once redelivery.

These recovery markers are additive metadata and need no database
migration — thread metadata for the retry marker and the restart stamp,
mission metadata for the shutdown-interruption stamps
(`pending_shutdown_interruption`) and the owed outcome notices
(`pending_outcome_notification`). Downgrading restores the older
completion/recovery behavior and does not reconstruct interrupted work. An
older build does not resolve the newer stamps. A pre-WI#276 build ignores
both keys. A v2.1.5.0 build ignores `pending_outcome_notification` and
reads only its own single-slot interruption stamp (`{"thread_id",
"recorded_at"}`), so the per-thread map a newer build writes sits unread —
until that build's own next grace-expired shutdown of the same mission
replaces the whole `pending_shutdown_interruption` value and drops the
newer entries (those fires keep their `INTERRUPTED` entry and dedup
release but are never retried). A re-upgrade's boot sweep resolves each
surviving interruption stamp younger than 24 hours
(`MAX_SHUTDOWN_INTERRUPTION_STAMP_AGE`) — record-only beyond that — and
emits every owed outcome notice it finds, with no age bound. Each stamp
first passes the WI#338 scan-time parity checks: the stamped thread must
be a Mission-type thread of the mission's own project and owner (unless
the mission is shared) and listed in the mission's `thread_history`; a
mismatch clears the stamp silently (a warn log only — no approach-history
entry, retry marker or notice), and the retry-marker sweep and the
recoverable-thread resume skip history threads that fail the same
project/owner checks. These checks remain defense in depth against partial
forgeries and store corruption. WI#322 reserves the complete
`engine/projects/**` tree from memory-tool reads/writes/tree listings and web
writes; WI#455 (v2.1.7.0) widens the *write* reservation to the whole
`engine/**` tree on the same lanes (reads/tree keep the narrow WI#322 set), so
those lanes cannot replace a boot-hydrated `mission.json` to bypass mission
creation gates and cannot forge an `engine/knowledge/**` or
`engine/orchestrator/**` document that shadows the boot-loaded id→path mapping.
Internal Store writes and exact-path operator recovery remain available; direct
database/operator mutation is still a trusted boundary.

Boot/reconciliation writers also reload each mission under its per-mission fire
lock before saving (WI#317), preserving a concurrent fire's history, budget,
dedup keys, and recovery metadata. This applies to managed-definition
reconciliation, retry/shutdown recovery, restart recording, and owed notices;
it does not make already-applied external side effects transactional.

The upgrade direction has its own wrinkle: every live retry marker written by
a build before WI#276 (pre-v2.1.5.0) deserializes with `notified: false`
(the field is newer than those markers), so the first boot after the
upgrade emits "Mission fire
interrupted; retry N of M re-armed after daemon restart" once per marker —
including markers whose original notice was delivered before the upgrade,
and markers of runs that genuinely failed inside the shutdown grace rather
than being interrupted. The boot wording can therefore mismatch a marker's
origin; carrying the stop reason in `RetryState` so the notice can name it
is a possible follow-up. Operators should inspect interrupted fires
before manually refiring tasks with external side effects.

All non-manual missions (cron, channel events, system events and webhooks)
remain active after successful runs (WI#311), even if a final
response declares `Goal achieved: yes` or `Mission complete`. Their prompt asks
about this run's success rather than overall goal completion, and names
`mission_pause` as the way to stop future runs. Only Active missions dispatch;
cadence does not override persisted status. User-facing intentional stops are
pause or deletion; the completion APIs have no production callers today.
Older user recurring missions parked in Completed were subject to phrase-driven
completion. They are not automatically revived because the owner may have accepted
that completion report; revival could unexpectedly run tools and send notifications.
Global startup logs a warning for each such record and sends best-effort recovery
guidance through its configured notification channels when a last thread exists.
This repeats at startup until the owner resumes, pauses, or deletes the mission.
One caveat before resuming (WI#319): before 2026-07-26, `mission_delete` recorded
deletions as `Completed` rather than removing or tombstoning the record, so a
parked Completed mission may be an intentional legacy delete rather than a
phrase-driven completion. The boot warning names an `updated_at` before
2026-07-26 as consistent with a legacy delete (the legacy delete went through
`update_mission_status`, which stamps `updated_at`) — a clue, not proof, since
a phrase completion before that date stamps `updated_at` the same way, and
`created_at` alone does not discriminate either (any mission created before
that date and phrase-completed in September satisfies it). The restart
recorder and the legacy user-id migration can re-stamp `updated_at`, so for
such rows the last history thread's `created_at` is the robust check; the
warning carries that thread's id and `created_at` (loaded best-effort,
otherwise pointing the operator at `mission_history` to read it). Rule out an
old delete first, then inspect the history via `mission_history` and use
`mission_resume(id=...)` as the owner to recover paused, completed or failed
missions. If the review confirms an intentional legacy delete, call
`mission_delete(id=...)` instead: for the missions this warning reaches that
is a hard delete — the record is removed and `mission_resume` cannot restore
it (only conversation-insights missions get a `deleted_at` tombstone, and they
never reach this warning) — whereas `mission_pause` keeps the record but
re-stamps `updated_at` and erases the clue. Cron recovery computes its next
future fire; missed occurrences/events are not replayed. Manual, non-managed
missions retain the standalone goal-declaration completion contract. Downgrading
restores the old phrase-driven completion behavior for recurring missions; no
schema change is needed.

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
`routine_list` / `mission_list`. That fallback serves v1 threads only — in
engine-v2 threads the bridge now blocks `routine_history` outright and points
the model at `mission_history` (WI#269).

Engine-v2 threads get their history surface from the `mission_history` tool
(WI#269), registered in the `missions` capability alongside `mission_list`
and `mission_update`. It resolves a mission by name or UUID — both paths
scoped like `mission_list`: the name path resolves through the calling
project's own + shared missions, and the UUID path re-checks project and
ownership (own or shared-owner) after the store load, returning
`access denied` for any other project's or user's mission UUID — and renders
the newest fires from the mission's `thread_history`
(default 10, clamp 1-50) in the `routine_history` result shape: per-run
status, `started_at`/`completed_at`, `duration_secs`, a char-safe
200-character result summary from the last assistant message, and
`tokens_used`, plus `total_runs`, `last_fired_at`, and `never_fired` at the
top level. `routine_history` itself is now in the bridge's v1-only tool
list, so a v2 thread that asks for a name which shadows both registries
gets an honest "use mission_history" error instead of silently returning
the v1 object's history for a same-named routine.

The two registries stay separate by design (WI#278); what crossed the
boundary was v1 vocabulary. In a v2 **foreground** thread **every**
`routine_*` tool now errors toward its mission equivalent with the call
shape (`routine_create` → `mission_create(name, goal, cadence)` with the
routine prompt as the goal and the schedule/`event:`/`webhook:` form as the
cadence, `routine_list` → `mission_list()`,
`routine_fire`/`pause`/`resume`/`delete` → the `mission_*(id)` verbs,
`routine_update` → `mission_update(id, ...)` plus the host command
`lunarwing routines edit --name <routine> --max-tool-rounds <n>` for a
lightweight routine's tool-round budget, `routine_history` →
`mission_history(name)`), instead of the slash-command advice the remaining
v1-only tools (`cancel_job`, `build_software`) keep — bundled skills written
for v1 (`routine-advisor`, `delegation`, `lunarwing-workflow-orchestrator`)
still name the routine tools, so the redirect is what keeps a v2 agent on
the working path. Autonomous threads (Research/Mission) hold leases for the
mission verbs, so the **mutating** `routine_*` verbs are **not** redirected
there — a prompt-injected `routine_delete` must not become a working
`mission_delete`; those get the plain v1-only error. The read-only pair
(`routine_history`, `routine_list`) redirects in every thread type:
`mission_history`/`mission_list` leak nothing an autonomous thread is not
already leased to see, and slash-command advice is unusable in a thread
with no user (WI#278 review). Hyphenated aliases (`routine-delete`) take the
same gate as the registered underscore names. In the other direction, v1
`routine_list` appends the caller's Engine V2 missions read-only under
`engine_missions` — the union of the engine default project's own+shared
missions and the caller's own project's own+shared missions (both queries
owner/shared-filtered, so nobody else's private missions appear) — with a
note that they are not routines and that `routine_history` shows their runs,
so a v1 agent no longer reports a live `self-improvement` mission as
missing, owner or not.

The `mission_*` functions are capability actions, not registry tools: the
eight definitions live in `mission_action_defs()` beside
`EffectBridgeAdapter::handle_mission_call`, and the router registers them as
the `missions` capability — Stateful today, so the planner grants it to
Foreground, Research and Mission threads at spawn. CodeAct reaches them
because Monty compiles a direct call to an undefined name into an external
call that `preflight_action` matches against the thread's leases; they are
absent from `available_actions`, the tool registry, `tool_list` and CodeAct's
globals, so only a direct call works. So that the registry's "No tool named …
is registered" is not read as "this function does not exist" (WI#295; the
false negative behind closed-invalid WI#281), the bridge answers `tool_info`
for a mission function itself, from the same definitions: the usual
name/description/parameters (plus summary or schema by `detail`) and
`kind: "engine_capability"`, `capability: "missions"`,
`registered_tool: false`, `callable`, and a note. `callable` reports a
capability — whether the asking thread holds the lease to call the function
— not guidance on whether it should: it mirrors the executor's lease
preflight and is true only while that thread holds a valid lease covering
the function in `MissionManager::leases()`, the thread manager's lease set
that preflight consults. WI#308 intersects the dispatch gate's categorical
refusal: a Research thread holding the lease is still answered
`callable: false` for the six mutating verbs, and a Mission thread for the
three lifecycle verbs (create, delete, resume — WI#356), because the gate would refuse
the call (and an executor-time refusal fails the whole code step). A thread
without one gets the same definition with
`callable: false` and a note saying a call from it would be refused, including
threads whose lease is currently revoked or narrowed. Resume refresh re-grants
missing actions under the current planner, including on same-process approval
resumes; it does not preserve a separate per-thread revocation policy (none is
currently exposed in production). Persistent revocation/narrowing would need its
own recovery contract before being exposed.
On restart resume (WI#309), the bridge's unscoped `registered_actions` catalog
includes the same eight mission definitions when a mission manager is wired.
`refresh_resumed_thread_leases` restores missing grants through the ordinary
thread-type lease planner, alongside live registry tools; it does not revive
persisted lease IDs or grant a wildcard. Owner validation happens before this
refresh, and failed resume persistence rolls back newly granted leases. The
scoped model-visible catalog and `tool_list` remain registry-only. No schema
change is needed; older binaries lose these mission grants again on restart.
`rlm_query` children run on a private copy of their parent's leases that the
host cannot see, so a thread the host holds no leases for is answered `true`
rather than a false "not callable". The answer is static definition text,
produced after every `tool_info` gate (permissions, approval, supervision,
rate limit, auth, hooks) and before output sanitization; it skips
`execute_tool_with_safety` because nothing runs that could time out and its
inputs are a fixed-name lookup plus the `detail` enum. It is produced only
while a mission manager is wired — the condition under which the functions
dispatch — and v1 keeps the truthful registry miss. The CodeAct special
functions (`llm_query`, `llm_query_batched`, `rlm_query`, `FINAL`) are
sandbox builtins that `tool_list`/`tool_info` never describe; the preamble
tells the model to call them directly. `tool_info` also accepts CodeAct's
positional form (`tool_info("x")`) and resolves legacy hyphen/underscore
tool aliases like dispatch does.

Dispatch of the eight verbs carries its own gate subset at the adapter
(WI#308), because `handle_mission_call` returns before the registry-tool
gate stack that a registry tool handle keys. In the registry stack's order:
Research threads — every `rlm_query` child included — are refused the six
mutating verbs outright (create, fire, pause, resume, update, delete);
Mission threads are refused the three lifecycle verbs (create, delete,
resume) outright (WI#356) — a mission thread may steer existing missions but
never creates, deletes or resumes them (run-111 HB-2: resume un-deletes the
conversation-insights opt-out tombstone and clears gate-pause markers) — and
a Mission thread's `mission_update` may carry the lookup keys and steering
fields only (`id`/`_args`, or `name` on its own, plus goal, current_focus,
success_criteria and the retry-policy scalars): every other field — cadence,
webhook_secret, max_threads_per_day, new_name, notify_channels — is refused
by an allowlist, and `name` together with `id`/`_args` is refused as a
rename (run-107 GM-1, run-111 HB-1); a
persisted `tool_permissions.<verb>` = disabled denies the call, and a
settings-store read error fails closed for the mutating verbs (read-only
verbs keep the registry path's deny-the-grant fallback); supervised mode
pauses every verb for human approval with `allow_always: false` (a
capability action has no always-approval grant to persist, so the dialog
does not offer one) and an approved resume re-enters with the grant and
executes; a per-user per-verb rate limit (20/minute, 200/hour) applies,
plus an aggregate per-user bucket across the four run-spawning verbs
(create, fire, resume, update — 60/minute, 600/hour; WI#352 AQ-12) so
their per-verb budgets cannot sum to 80 run-spawning mutations/minute.
The stop verbs pause/delete use per-verb **caller-class** buckets: Mission
threads cannot spend Foreground stop tokens, while each class still has a
20/minute, 200/hour cap for each stop verb. (After WI#356 a Mission thread
never reaches the delete bucket — the gate refuses the verb first — so the
Mission-class delete bucket is defensive keying.) Caller class comes from
the host execution context, not tool parameters — and it is not a trust class
(WI#359 CN-3): all of one user's Mission-class threads share one
`@Mission` stop bucket per stop verb (the mission prompt steers threads to
`mission_pause`), and the list/history verbs and the run-spawning
aggregate are still shared between Foreground and Mission. Read-only
verbs stay per-verb only
so inspection cannot starve a mutation budget. Per user, total dispatches
are bounded at 180/minute (60 aggregate + 40 stop calls per permitted caller
class, Foreground and Mission + 20 each for list/history); with only one
caller class the bound remains 140/minute. Multi-bucket admission checks
and records under one write lock: a call the limiter refuses spends no
tokens in any bucket (the limiter charges before the `ToolCall` hooks run,
so a hook refusal is a different, post-charge path — WI#359 CN-2), and
concurrent batches cannot overshoot either cap. Both limiters
use fixed windows (a window boundary can briefly admit up to 2x the
minute budget) and reset on restart. AQ-12 deliberately leaves open:
`mission_create` alone still allows 200 creates/hour (its per-verb
hour budget), there is no active-mission cap, and `max_threads_per_day=0`
(unlimited) remains accepted on create and update; and
`ToolCall` hooks run with credential-like parameter values redacted (the
shared WI#359 CJ-10 predicate, recursive into positional `_args[0]`
payloads; the approval prompt's
display parameters are redacted the same way, while the persisted gate keeps
the real parameters for execution). The approval-tier gates deliberately do
not map: the verbs carry `ApprovalRequirement::Never` semantics by this
gate's own choice — every v2 `ActionDef` declares `requires_approval: false`
by construction, so the flag itself says nothing about the tier — because
autonomous Mission threads must steer their own missions and routines are
created from shared channels by design; the shared-conversation,
non-Foreground-`Always` and needs-prompt gates stay off, and the pre-flight
auth gate is a no-op for capability actions, which hold no credential
registrations. The coordination gate (a disabled agent executes nothing)
applies to mission calls as before.

Related secret-handling and webhook-ownership rules (WI#352, the !1462
run-33 residual): the generic fallback of `summarize_params` skips values
whose keys look credential-like (the shared WI#359 CJ-10 normalized
predicate — `webhook_secret`, `api_key`, `hmac_secret`, `apiKey`,
`access_token`, …), and `mission_*` verbs summarize as name/id/`_args[0]`
only, so a verb's `webhook_secret` cannot surface in the
ToolStarted/ToolCompleted channel status summary (AQ-5); the persisted
`ApprovalRequested` thread
event stores display parameters with the same predicate redacted,
recursively (a positional `_args[0]` payload included), and for
`http`/`web_fetch` also with credential-like header values blanked (matched
by header name against the union of the header heuristic and the shared
credential-key list and normalized names ending in `key` except
`idempotencykey`, or by an auth-scheme value prefix; the string form of
`headers` is parsed, redacted inside, and re-serialised, failing closed on
unparseable input) and URL credentials stripped (userinfo removed,
credential-like query values replaced by `[REDACTED]`, the fragment
dropped). String-form HTTP bodies are parsed as JSON, recursively redacted
and re-serialised; non-JSON strings use the form/query credential-key
redactor, preserving untouched pairs and empty values. The header `…key`
extension is display-only (WI#359 CJ-11, run-91 FI-1..FI-4, run-97 FS-1/FS-2).
The event keeps this redacted copy rather than the raw call parameters —
the pending gate still holds the raw copy
for resume, and the thread transcript's action arguments plus the
`MessageAdded` content previews remain pre-existing at-rest classes (AQ-4,
WI#359 CL-4); the webhook single-owner rule (a fire
matches every Active mission on a path **across projects** — the route is
one public endpoint — with each candidate checked against its own
configured secret in constant time, so a non-terminal path may only have
one owner) is enforced — per project, not globally (WI#359 CJ-5 keeps the
documented per-project scope; cross-project conflicts on one path are
possible and each mission's own secret remains the fire-time control) — on
create, on an
update that switches to a Webhook cadence from another cadence, changes
the path, or adds a usable
secret to a dormant claim, and on resume
(AO-1); the claim check and the save are serialized by the shared
`mission_create_lock` (held as `_create_guard` in create and
`_claim_guard` in update/resume) on every claim-making transition —
create, update, and resume alike — so two racing transitions cannot both
pass the check and both save (CJ-4); updates that leave the claim untouched are not re-checked (a
same-path re-set is deliberately not checked), so
under a legacy two-owner conflict either owner can still rotate a leaked
secret — but a dormant row's owner cannot lower a budget or make any other
update without first supplying a usable secret, because validation rejects
the row otherwise (WI#359 CO-3) — "usable" here and everywhere above is
the shared `usable_webhook_secret` predicate in
`lunarwing_engine::types::mission` (non-empty, no leading/trailing
whitespace, visible ASCII with an interior HTAB allowed — values httparse
trims or `HeaderValue` rejects can never arrive on the wire). Dormant (secret-less or unpresentable
whitespace/non-visible-ASCII secret) non-terminal rows still block other
users' claims on their path (WI#359 CO-3); startup logs a warning for
every legacy multi-owner path it re-activates (WI#359); two conflicting
Paused missions cannot resume — each resume re-checks the rule and finds
the other non-terminal holder, so both are refused until one is deleted,
completed, or moved off the path (WI#359 CL-4, run-91 FH-5); and
create-dedup compares webhook cadences by
path only, so a rotated secret is not a spurious cadence difference (AO-7)
— the dedup hit discards the supplied secret and flags
`webhook_secret_applied: false` with a pointer to `mission_update`.

The built-in HTTP tool strips the full request URL from reqwest send errors
before producing `ExternalService` text (run-97 FS-5). This covers both
simple-GET redirect hops and non-simple requests, including query credentials
injected from the secrets store under arbitrary names. Response-body errors
also drop any attached URL; timeouts keep their existing typed classification.

Two scope properties follow and are stated deliberately (WI#308 asked for
the supervised-mode consequence, not just the gate ordering). First,
supervision does not propagate into mission-spawned threads: mission
threads are built from `ThreadConfig::default()`, which is unsupervised,
and `AGENT_SUPERVISED_MODE` is applied only to foreground message threads —
one approved `mission_create`/`mission_fire`/`mission_resume` (cron or
webhook cadence) starts an autonomous, unsupervised agent whose own mission
calls are limited by the rate limit, hooks, and the WI#356 lifecycle
refusal. The approval description for the run-spawning verbs (create, fire,
resume, update) says so and discloses the fan-out: the run may steer any of
the owner's missions (fire, pause, and goal, focus, success-criteria or
retry-policy updates) without
asking. `rlm_query` children, by contrast, inherit the parent's
`supervised_mode`/`shared_conversation`/`supervised_timeout_secs` into
their config, so a supervised parent's child gates fail closed. Second, the
mission verbs are owner-scoped, not thread-scoped: any Foreground or
Mission thread may steer *any* of the owner's missions by name or UUID
(`resolve_mission_id` carries no thread scoping), and the planner grants
the verbs to every thread type under the Stateful tier. A Foreground
thread's update may re-target (goal, cadence, notify channels, webhook
secret, budget, rename); a Mission thread's update is limited to the lookup
keys and steering fields (goal, current_focus, success_criteria, retry
policy) — every other field, and `name` together with `id`/`_args` (a
rename), is refused at the gate by an allowlist (WI#356, run-107 GM-1,
run-111 HB-1), so a
mission run cannot turn any mission into a webhook-triggered one with a
secret the model chose, nor lift its daily budget, nor rename it. The v1 equivalents
(`routine_create`/`update`/`delete`/`fire`) sit on
the autonomous-execution denylist; the v2 verbs do not — the bound placed
on them (WI#356, run-33 AQ-3 / run-40 BF-1) is the dispatch gate's
categorical refusal of `mission_create`/`mission_delete`/`mission_resume`
to Mission threads
alongside the Research-thread refusal — partial v1 parity (create/delete
only; v1 also denies update/fire, which v2 keeps as steering) — so creating,
deleting or resuming a mission stays with the foreground conversation (supervised
only when the tenant sets `AGENT_SUPERVISED_MODE`), plus the fan-out
disclosure on the run-spawning approval prompts. The owner-scoped steering
reach that remains is deliberate and stays open. One related seam remains
by design: the structured-tier policy preflight
(`executor/structured.rs`) evaluates policy from `available_actions`, which
stays registry-only so the model-visible catalog does not duplicate the
verbs — policy is therefore not consulted for the mission verbs, neither
per call nor at lease grant.

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
sees sanitized state. See the
[verification record](../ops/history/v2.1.2.0_WI109_WI126_SANITIZE_MEMORY.md).
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

### Scheduler Liveness Signals (WI#323)

Because a stored `next_fire_at` says nothing about whether the scheduler is
running, the mission cron ticker emits its own liveness trail at info level:

- one `mission cron ticker started` line naming the active-index size, the
  tick interval (60 seconds) and the heartbeat interval (3600 seconds);
- `mission fired` per successful fire, carrying the mission id, thread id,
  cadence kind (`cron`, `channel_event`, `system_event`, `webhook`, `manual`)
  and stored `next_fire_at`. The kind describes the cadence, not how this fire
  was triggered; non-tick fires do not advance the stored schedule;
- `mission goal declared achieved; auto-completing mission` when a run's own
  goal declaration completes the mission — distinct from the info-level
  `mission explicitly completed` line of an operator `complete_mission`;
- a `mission scheduler heartbeat` on the first tick after the 30-second
  startup warmup, then at most hourly. It reports `active_missions` (rows
  actually loaded as Active), `load_failures`, the earliest scheduled fire
  among Active missions (`next_fire_at=none` when nothing is scheduled), and
  `overdue_cron_missions`;
- a `warn` when an Active cron mission's `next_fire_at` is at least two
  minutes overdue. It reports `mission_id`, `scheduled_for`, and
  `overdue_seconds` before attempting the fire, not as proof of success.
  The warning is de-duplicated per mission and scheduled occurrence in this
  process: repeated failed attempts for the same occurrence do not repeat it.
  A new overdue occurrence (or a process restart) can warn again. The heartbeat
  continues to count overdue missions even when this warning is suppressed.

The per-mission fire-failed warn uses a static `error_kind` (`invalid_cadence`
or `engine_error`), and the ticker-level tick-error warn uses the same
classification. Full error text, which can quote stored cadence or timezone
values, stays at debug. A failed fire is skipped without stopping the rest
of the tick. The heartbeat uses monotonic `Instant` elapsed time, not tick
counts: a stalled loop delays the beat. Seeding its clock one hour in the
past makes the first post-warmup tick emit a heartbeat immediately; a loop
that never gets through its first tick still leaves only the start line.

None of these info/warn lines carry user content — no mission names, goals,
prompts, cadence expressions, event patterns or trigger payloads. Only ids,
static kinds/classes, counts and timestamps are logged.

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
  (message/thread/conversation identity + timestamp). Statements ingested
  through `/steer` carry `Steered` provenance and are captured as fresh user
  evidence on the same terms as direct `User` input. Generated goals, system
  injections, research/mission threads, failed threads, other users' or
  projects' threads, legacy pre-upgrade threads, and truncated
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
- Before executing a registry tool, including an approval resume, the adapter
  rejects non-foreground calls when `allows_autonomous_execution()` is false.
  Both secrets tools opt out; `secret_delete` additionally requires approval
  for each foreground deletion and ignores persistent `always_allow` grants.
  Daemon discovery and engine lease planning share the autonomous denylist,
  including `ssh`/`ssh_git`; per-tool opt-outs are independently rechecked.

### HybridStore (`ic/src/bridge/store_adapter.rs`)

Wraps the `Workspace` as the engine's `Store` trait:
- Implements persistence and scoped listings for threads, steps, events,
  projects, docs, leases, missions, and conversations, including evidence-checked
  insight writes. Shared-scope helpers can use Store defaults; global thread and
  mission enumeration use the store's own maps so orphan-project rows remain visible.
- **Thread ownership on write (WI#502).** `save_thread` is a keyed write
  (`thread_path(thread.id)`), so it refuses to replace a record whose owner is a
  different real user — the durable half of the forged-`thread_id` boundary,
  independent of which caller reaches it. Same-owner rewrites stay allowed (the
  gateway reuses one UI thread id across turns), as does stamping the serde
  default `legacy` owner that `migrate_legacy_user_ids` rewrites at boot, and
  shared-owner records.
- In-memory `HashMap` caches backed by workspace files under `engine/`
- Human-readable knowledge/orchestrator files plus JSON runtime state under
  `engine/.runtime/`
- Fallback to workspace files on cache miss; terminal cleanup evicts memory but
  preserves persisted output.
- Boot read/parse failures warn with escaped stored paths and validated UUID
  identities, excluding record content (WI#321/WI#493). Duplicate `default`
  projects warn once per owner at boot; selection consistently chooses the
  oldest, with UUID as tie-breaker. The duplicate warning does not merge rows.
  New `default` projects cannot appear in pairs any more (WI#490): both
  creation sites derive the id as `uuid v5` of the owner's user id under a
  fixed namespace, so concurrent first messages and a boot racing them write
  the same record instead of two. The deterministic id applies only when the
  owner has no loaded default, so existing loaded projects (and the missions
  already under them) are untouched; an existing duplicate pair is left in
  place and keeps resolving to the oldest.
- **Unloadable default-project recovery (WI#535).** On owner boot or a user's
  next message, re-creating a default at its deterministic path first preserves
  any existing unloaded or unparseable `project.json` content byte-for-byte at
  `backups/engine/projects/default--<id8>/project.json.unloadable-<UTC>-<UUID>`.
  These are workspace documents, not host filesystem files. The reserved
  backup namespace keeps the evidence out of search, model writes and the
  project loader; operators can read it by exact workspace path. Copying before
  replacing keeps the original in place if the replacement fails. A read or
  backup-write failure aborts the save; an unreadable record is never treated
  as absent. Empty, truncated and schema-invalid records are preserved too.
  A record skipped by a transient boot-read failure is also preserved even if
  the pre-write read now succeeds.
  After successful persistence, a `warn!` names the project id, original path
  and backup path without logging record content. The id and mission paths
  stay unchanged, so missions re-attach. Each recovery gets a new backup;
  earlier evidence is retained without automatic pruning.
  Project saves serialize preservation, persistence and cache publication
  within the store, and propagate persistence failures before changing the
  cache. This is not a cross-process lock against external workspace edits.
- MemoryDoc saves propagate workspace content-write failures. Persistence must
  succeed before publishing the document/cache path or deleting a rename's old
  file. Writes are serialized through a store-local lock so cache publication
  follows durable write ordering. Workspace-less stores remain in-memory.
- `save_internal_memory_doc` is a separate native-only entry point for compiled
  seeds and engine bookkeeping. Ordinary saves cannot bypass the control-doc
  gate using `source: compiled_in` or the failure-tracker title.

The router dual-writes compatibility conversation history to the relational
`Database` separately. That database is not the backing store for `HybridStore`.

The turn-history reference the completion path uses to correlate recovery
markers is stored per engine thread: `engine_turn_history.<thread_id>` in the v1
conversation's metadata, holding `{thread_id, user_message_id, started_at}` for
the turn's persisted v1 user row. Both backends merge a single metadata key
atomically, so overlapping engine turns on one conversation cannot overwrite
each other's reference (WI#517). The pre-WI#517 single `engine_turn_history`
slot is still read as a fallback, guarded by a `thread_id` match, so a turn that
started before an upgrade still records its terminal markers. Keys are written
once per thread and never reused.

#### Pre-switch history import (WI#442 F7)

The dual-write is forward-only: rows created before a tenant switched to
Engine V2 are not in the engine surface that seeds each new thread, so without
an import the first thread after the switch would start cold. The router runs
one import per v1 conversation (`router/v1_history_import.rs`) that reads that
conversation's `user` and `assistant` rows and prepends them to the surface as
imported entries (`EntrySender::ImportedAgent` / `imported_from: v1`
metadata), which `build_history_from_entries` maps back to user/assistant
history.

Four properties make it safe:

- **Ownership gate** — the v1 conversation must belong to the message's user
  before anything is read from or written to it. A caller-supplied `thread_id`
  is not proof of ownership, so a forged one cannot copy another user's history
  into the caller's engine context. This mirrors the v1 hydrate boundary in
  `agent/thread_ops.rs`. `get_or_create_scoped_conversation` also fails
  (`DatabaseError::NotFound`) instead of returning a conversation owned by
  another user or channel, which closes the same class of gap on the forward
  dual-write path.
- **New-surface gate** — the import runs only when the current message created
  the engine surface (`get_or_create_conversation_with_created`). Tenants
  already on Engine V2 get the marker recorded without a backfill, so this
  change neither surfaces weeks-old context in a thread that never had it nor
  undoes `/clear` (which empties entries but keeps `created_at`).
- **Marker gate** — `engine_v2_v1_history_imported` in the v1 conversation's
  metadata short-circuits later messages.
- **Creation-time cutoff plus a bounded read** — only rows older than the
  surface's `created_at` are imported, so the forward dual-write's own v2 rows
  can never be re-imported even when the marker write fails. The read goes
  through `list_conversation_messages_paginated` with the surface's own entry
  bound, so an oversized v1 conversation never has to be loaded whole before
  the bound applies.

The cutoff compares two clocks (Postgres rows take `created_at` from the
database's `NOW()`; the surface's comes from the daemon — libSQL uses the
daemon clock for both). Skew shifts the boundary, but the marker keeps that
from re-importing rows and the new-surface gate normally puts the boundary at
"now" with only genuinely pre-switch rows on the far side.

`tool_calls` and `interrupted` marker rows are skipped: they are v1
bookkeeping, not conversation context. The import is best-effort — a database
or store failure is logged and the turn proceeds without the extra context. The
marker lookup costs one extra query on each turn of a conversation that has not
been imported yet.

#### Durable role census (WI#518)

`conversation_messages.role` is free text shared by the classic loop, the
engine dual-write, and the turn-recovery train. The vocabulary lives in one
place, `ic/src/history/roles.rs`:

- **Conversation roles** (`user`, `assistant`) — replayed into model context
  and importable into an engine surface.
- **Tool transcripts** (`tool_calls`) — JSON reconstructed into tool messages
  by the v1 hydration path, never replayed as text.
- **Markers** (`interrupted`, `turn_recovery`) — display state only; never
  replayed, never used as a title or message count.

Every consumer classifies through `classify_conversation_role` /
`is_bookkeeping_role`: the v1 history import, `rebuild_chat_messages_from_db`,
the gateway turns builder, and the FIFO status interpreter
(`router.rs::interpret_message_event`, which rejects bookkeeping roles before
its wire-role ladder). Conversation previews and counts keep the SQL
`role = 'user'` predicate, which is safe by construction because markers can
never be a user row. Engine `MessageAdded` wire roles are a separate namespace
(`MessageRole::wire_name()`), classified by `classify_engine_role` with a
positive ladder, so an unregistered wire role is never surfaced.

A new durable role is added to `ALL_ROLES` **and** `classify_conversation_role`;
the census tests fail until both agree and until all four consumers handle it.

### Channel Routing And Delivery

`should_route_to_engine_v2()` owns the rollout policy. `ENGINE_V2=false`
disables every Engine V2 route. With Engine V2 enabled, the gateway is always
eligible; `ENGINE_V2_CHANNELS` can additionally select the exact names `xmpp`,
`darkirc`, and `weechat`. Values are comma-separated, trimmed, and matched
case-insensitively by exact channel name. Empty and unknown values are ignored.
Note there is no opt-out value: any entry that is not one of the eligible
channel names — including
something like `none` — is simply ignored, and the gateway always routes
while `ENGINE_V2=true`. The only durable opt-out is `ENGINE_V2=false`.

The exact-name allowlist is **by design** (WI#442 F8), not a gap: non-opted
channels keep running the v1 loop until engine coverage for them lands under
WI#221/#222. The consequence for operators is that v1-era findings remain
relevant on those lanes even at full Engine V2 adoption — they are not retired
by the switch. Expanding the set means adding an eligible channel to
`ENGINE_V2_OPT_IN_CHANNELS` and these docs together; retiring the v1 loop is a
separate, larger decision. WI#442 F8's acceptance is the recorded decision to
keep the allowlist, which this paragraph is.

Engine `ResponseDelta` events flow through
`ChannelManager::send_status(StatusUpdate::StreamChunk)`. The gateway maps each
chunk to SSE. WASM channels deliberately treat chunks as a no-op because the
current WIT has no message-editing contract. `router/outcome.rs` transforms
completed terminal text before recording engine and compatibility history. The
agent's outbound handler then uses the shared `ResponseDelivery` path:
`BeforeOutbound`, suppress-empty, optional coordination coalescing, and
`ChannelManager::respond()` with the original incoming metadata.

**Silent replies are not shutdowns (WI#550).** V1 `NO_REPLY` responses and V2
`Completed { response: None }` outcomes both return the same empty-string
suppression sentinel as `Stopped`. The agent loop therefore continues, reports
`NoReply` and sends nothing. V2 writes no assistant compatibility-history row;
V1 keeps the literal `NO_REPLY` response in its in-memory thread history and
database as before, since persistence precedes outbound suppression.
Model-driven paths and the legacy command adapter no longer produce a shutdown
sentinel. Legacy commands return text or the empty suppression sentinel, even
for message-less success, suppressed, interrupted or approval-pending results.
`Ok(None)` remains the explicit accepted-quit signal, with the DEC-1 client-channel
backstop. The separate `GatePaused` mapping is unchanged because its gate handler
owns status delivery.

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

**Join and restart recovery (WI#532/WI#534).** A thread id names a reusable
conversation surface, not a unique execution. `Thread.created_at` is therefore
the generation key for each incarnation: it changes when a new turn adopts the
id and stays stable when the same logical turn resumes from a gate.
`join_thread` selects the current generation, while foreground and expired-gate
waiters use `join_thread_for_run` with the generation captured at spawn/resume.
Their polling, timeout stops, and abandoned-waiter cancellation use that same
key, so a stale watcher cannot consume, stop, or finalize a newer turn.

Running completions are cloneable shared futures and completed outcomes are
retained per generation, so joining is multi-consumer rather than take-once.
Joining does not unregister live work, and dropping one joiner cannot consume or
cancel the shared completion. The per-thread lifecycle lock covers registry
snapshots, durable creation through running registration, and the store
fallback, but is never held across a live await. Terminal unregistration and
final persistence check the generation; re-adopting an id over another live
generation is refused. This closes the re-adoption window where a stale join
could see a new `Created` record without its task and fail that live task.
Task-level errors enter the same claimable completed slot, and task errors or
panics unregister their generation even when no joiner remains.

Completed entries expire after 22 minutes and are pruned on insertion and read,
covering the longest foreground/late-waiter budget without retaining every turn
for the daemon's lifetime. Every joiner can read an outcome, but an atomic
per-generation completion claim permits only one channel/history/gate
completion pass. Foreground, late, and expired-gate waiters all use that claim;
later waiters return the empty sentinel instead of duplicating a reply, history
entry, or pending gate. After retention expires, store recovery creates a fresh
unclaimed slot: exactly-once completion covers the current waiters inside that
window, not an arbitrary joiner arriving later.

When neither registry has the requested generation, `store.load_thread` is the
restart-safe fallback. `Done`/`Completed` reconstructs `Completed` from the last
visible assistant message, which the orchestrator persists before the final
save; a terminal record with no assistant message recovers as the empty
response sentinel, not as an agent shutdown. `Failed` returns `Stopped` for a
recorded host-stop reason and otherwise returns generic failure text rather
than replaying internal store, I/O, or engine reasons. `Waiting` returns
`ThreadError::Waiting` without modifying the record; foreground and late
completion paths suppress an immediate reply and leave resume to gate recovery.
`Suspended` reports an interrupted turn without touching its checkpoint. Only a
`Created`/`Running` record for the requested generation with no live task is
finalized as `Failed("engine task absent when its outcome was joined")`, with
checkpoint cleanup, event persistence, lease revocation, and broadcast. That
reason is deliberately distinct from a host stop, so a later recovery still
classifies the orphan as a failure. An id with no matching durable record still
returns `ThreadNotFound`.

Recovered outcomes populate the same bounded per-generation slot before return,
so later joiners see the same representation; they are not a durable outbox.
Join/stop/thread-lookup failures delivered by the agent message loop or late
watcher pass through `Error::user_facing_message`, which keeps engine context
and thread UUIDs in logs while sending a stable retry message to channel users.
A live re-adoption collision instead asks the user to resend after the previous
turn finishes stopping; its raw `AlreadyRunning` reason and UUID remain log-side.
Actionable gate errors such as stale or expired approvals keep their original
messages. Terminal failures classified as `Unknown` also use generic text;
classified model and orchestration messages remain unchanged.

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
decision, see the [steer contract](../ops/history/v2.1.1.0_GOAL18_WI105_STEER_PREEMPTION_CONTRACT.md)).
A note queued while a tool future is pending does not abort that call: the
inject signal only surfaces at the next `__check_signals__` poll, after the
in-flight tool's result. Only `/interrupt` (`ThreadSignal::Stop` / the run
token) cancels an in-flight tool. To stop a long-running tool, interrupt; to
redirect the turn afterward, steer — an interrupt followed by a steer note
lands the note on the next turn.

Legacy V1 has a separate pre-turn delivery seam (WI#584). While constructing the
next turn it drains Idle/Interrupted notes and inserts them immediately before
the new request with explicit "queued before this turn started" framing. The
first provider request therefore contains the note in the correct order without
claiming it arrived mid-work. Live mid-turn and approval-resume steering still
uses V1's iteration-boundary path. When a soft-timed-out turn still completes
with a response, suppressed-response draining clears both pending messages and
steer notes before considering follow-ups, including the post-drain race
cleanup, so stale steering cannot leak into a later turn. A turn that hangs
past the hard-kill grace is aborted instead: that path resets the thread and
keeps both queues, so the next turn receives the surviving messages and notes.

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
| `Store` | Persistence, scoped enumeration, leases, missions, and atomic insight validation | Workspace-backed `HybridStore` |
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

The conversation surface is the one bounded record. Every new foreground
thread is seeded with the conversation's prior user/agent entries
(`build_history_from_entries`), so `ConversationSurface::add_entry` evicts the
oldest entries FIFO past `MAX_CONVERSATION_ENTRIES` (400) or
`MAX_CONVERSATION_HISTORY_BYTES` (96,000 bytes, roughly 24k tokens), always
keeping the newest entry. An oversized pre-upgrade surface is trimmed by the
first `add_entry` after the upgrade — which runs before the next seed — so no
migration is needed. Thread messages, steps, and events are unaffected; the
relational dual-write keeps the full V1-compatible history.

## Event Sourcing

Threads persist significant lifecycle and execution events via `ThreadEvent`.
The 22 `EventKind` variants cover state, model-recovery, step, action, lease,
message, response- and reasoning-delta, child, approval, self-improvement,
skill-activation, and orchestrator-rollback events.
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
                                        # Default: 1 when unset (mt-admin tenants are
                                        # provisioned with 2); clamped to 0-3; 0 disables
                                        # engine recovery for every failure kind.
                                        # When >= 1 it does not clamp context_overflow
                                        # (WI#298): its shrink ladder changes the inputs
                                        # instead of blindly retrying, so it uses the
                                        # policy cap (2); the total-attempts cap below
                                        # still bounds it.
ENGINE_V2_MODEL_RECOVERY_TOTAL_ATTEMPTS=4 # Total recovery attempts per thread lifetime.
                                        # Default: 4 when unset (mt-admin tenants: 2);
                                        # clamped to 16; 0 disables all engine recovery
```

**Billed-empty retry budget (host daemon knob, WI#578/WI#595).**
`LLM_EMPTY_STREAM_RETRY_INPUT_TOKEN_BUDGET` defaults to **100000** reported input
tokens per provider leg and call. With retries enabled, the first transient
billed-empty attempt always gets one retry, even when it reaches the threshold.
After that, once the discarded billed-empty attempts plus the current one reach
the threshold, `RetryProvider` returns the current typed `EmptyStream` error
with every attempt's usage folded in instead of opening another attempt; one
attempt can therefore overshoot the threshold. `0` restores retry-count-only
behavior. Usage-less empty responses and ordinary transient retries are
unchanged. Every failover leg and engine or agent recovery re-issue starts a
fresh budget, so this is not a per-turn bound.

**Pending-gate expiry (host daemon knob, WI#111).** How long a pending
approval/auth gate waits before the background sweep expires it and resumes
the paused turn (WI#108) is configured on the daemon, not in `ThreadConfig`:
`AGENT_PENDING_GATE_EXPIRY_SECS` env (read via `AgentConfig::resolve`) takes
precedence over the DB-backed setting `agent.pending_gate_expiry_secs`;
default **1800s (30 minutes)**. Values outside the inclusive range **30s to
2592000s (30 days)** fall back to the default. The lower bound prevents windows
shorter than the 60-second gate sweep can observe; the upper bound prevents
unsigned-to-signed wraparound and Chrono duration overflow (WI#115). Expiry
replies carry the pending gate's own thread id. A successful gateway broadcast
emits the thread-scoped SSE response directly; the explicit SSE notification
remains a fallback if channel delivery fails.

**Prompt-overlay audit and operator lifecycle (WI#268).** The full contract —
overlay storage identity and bridge path, the three gated writers, consumption
semantics, the existing-thread retention caveat, safe enable/disable, and a
read-only tenant inspection runbook (`mt-admin daemon-pid` / `mt-admin psql`)
— is published in
`docs/ops/history/v2.1.4.0_WORK_ITEM_268_PROMPT_OVERLAY_AUDIT.md`. Key operator facts:
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

Run from `ic/`, using the vendored lockfile. These examples use the 12-thread
cap that `AGENTS.md` sets for cargo on the development host.
Use tmux for long runs and `cargo check` for compile verification.

```bash
taskset -c 0-11 cargo check --locked --offline -j12 -p lunarwing_engine
taskset -c 0-11 cargo test --locked --offline -j12 -p lunarwing_engine --lib -- --test-threads=12
```

Host bridge/HTTP gate tests live in the daemon crate and are separate from the
engine suite. A compile check alone does not verify approval ownership,
protocol-channel delivery, or recovery across a real tenant restart.

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
