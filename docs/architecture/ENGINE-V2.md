# Engine V2 Architecture

The V2 engine is a unified thread-capability-CodeAct execution model that lives in `ic/crates/lunarwing_engine/`. It replaces approximately 10 separate v1 abstractions (Session, Job, Routine, Channel, Tool, Skill, Hook, Observer, Extension, LoopDelegate) with 5 core primitives.

Enabled at runtime via the `ENGINE_V2=true` environment variable. The gateway
uses Engine V2 by default when enabled. The bridge router (`src/bridge/router.rs`)
keeps other channels on the legacy path unless an eligible channel name is
present in `ENGINE_V2_CHANNELS` (case-insensitive exact matching).

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
    error.rs              EngineError, ThreadError, StepError, CapabilityError
  traits/                 External dependency abstractions (host implements these)
    llm.rs                LlmBackend trait
    store.rs              Store trait (31 CRUD methods)
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
    messaging.rs          ThreadSignal, ThreadOutcome, signal channels
    skill_feedback.rs     Skill usage feedback events (feature-gated)
  executor/               Step execution
    loop_engine.rs        ExecutionLoop (core loop replacing run_agentic_loop)
    structured.rs         Tier 0: structured tool call execution
    scripting.rs          Tier 1: embedded Python via Monty (CodeAct/RLM)
    context.rs            Context builder (messages + actions from leases + memory docs)
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
                   --> Completed --> Done
                   --> Failed
```

All transitions are validated by `ThreadState::can_transition_to()`. Terminal states: `Done`, `Failed`. Invalid transitions return `ThreadError::InvalidTransition`.

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
3. Call LLM via LlmBackend::complete()
4. If Text: check tool intent nudge, return if final response
5. If ActionCalls (Tier 0): find lease -> check policy -> consume use -> execute -> record
6. If Code (Tier 1): execute Python via Monty with context-as-variables
7. Record Step, emit ThreadEvents
8. Repeat until: text response, stop signal, max iterations, or approval needed
```

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
- **Compact output metadata**: between code steps, only a summary is added to chat context (e.g. `"[code output] stdout (4532 chars): The results show..."`) to prevent context bloat
- **Explicit termination**: `FINAL(answer)` or `FINAL_VAR(name)` for output
- **Resource limits**: user CodeAct blocks default to a 30s timeout, 64MB memory,
  and 1M allocations. The orchestrator VM has a separate 300s/128MB/5M budget.
  Monty execution is wrapped in `catch_unwind` for panic safety.

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
- Can be patched by the self-improvement mission and versioned in the Store
- Falls back to compiled-in default (v0) if disabled or patches fail (3+ consecutive failures triggers rollback)
- Async dispatch of tool calls with `asyncio.gather()` for parallel execution
- Default orchestrator source: `ic/crates/lunarwing_engine/orchestrator/default.py`

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
3. **Consume**: `LeaseManager::consume()` decrements the use counter
4. **Revoke**: `LeaseManager::revoke()` permanently disables the lease
5. **Expire**: `LeaseManager::expire_stale()` cleans up time-expired leases

### Policy Engine

The `PolicyEngine` evaluates actions against leases deterministically:

- **Priority**: `Deny > RequireApproval > Allow`
- **Effect types**: `ReadLocal`, `ReadExternal`, `WriteLocal`, `WriteExternal`, `CredentialedNetwork`, `Compute`, `Financial`
- Every action declares its side effects via `EffectType`. The policy engine uses these for allow/deny decisions
- **Provenance types**: origin labels exist for user, system, tool, LLM, and
  memory-retrieval data. `PolicyEngine::evaluate_with_provenance()` implements
  targeted approval rules for LLM-generated or tool-sourced data, but the
  production execution paths still call `evaluate()`, so provenance-aware
  enforcement is not wired into runtime action dispatch yet.

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
boundaries.

`MissionManager::ensure_learning_missions()` provisions three baseline missions
and, when `SKILL_SELF_IMPROVEMENT=true`, two additional skill-improvement
missions. They are event-driven; not every mission runs after thread completion.

### Error Diagnosis (`self-improvement`)

- **Trigger**: fires when a thread completes with trace issues
- **Action**: diagnoses root cause and applies prompt overlays or orchestrator patches

### Skill Extraction (`skill-extraction`)

- **Trigger**: fires when a thread succeeds with 5+ steps and 3+ tool actions
- **Action**: extracts reusable skills with activation metadata, CodeAct code snippets, and domain tags
- **Output**: stored as `DocType::Skill` MemoryDoc

### Conversation Insights (`conversation-insights`)

- **Trigger**: fires every 5 completed threads in a project
- **Action**: extracts user preferences, domain knowledge, and workflow patterns

### Expected Behavior (`expected-behavior`)

- **Trigger**: explicit `user_feedback/expected_behavior` events
- **Action**: investigates user-reported expectation gaps and proposes or applies
  an appropriate correction

### Skill Maintenance (`skill-maintenance`, feature-gated)

- **Trigger**: `thread_completed_with_issues` events when
  `SKILL_SELF_IMPROVEMENT=true`
- **Action**: identifies confidently dead or quarantined skills and stages
  user-approved prune/archive proposals

`self-improvement` is also feature-gated. With the flag disabled, skill usage
tracking, automatic demotion, patch proposals, and prune proposals are inert.

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

The daemon integrates the engine via three adapters in `src/bridge/`:

### LlmBridgeAdapter (`src/bridge/llm_adapter.rs`)

Wraps `LlmProvider` as `LlmBackend`:
- Converts between `ThreadMessage` and `ChatMessage`
- Converts `ActionDef` to `ToolDefinition`
- Supports cheaper provider for sub-calls (depth > 0)
- Returns provider text to the engine; `LlmResponse::from_text()` in
  `types/step.rs` classifies fenced Python as CodeAct code

### EffectBridgeAdapter (`src/bridge/effect_adapter.rs`)

Wraps tool execution and safety layer as `EffectExecutor`:
- Maps engine action calls to tool registry execution
- Applies safety checks, timeout, serialization

### HybridStore (`src/bridge/store_adapter.rs`)

Wraps the `Workspace` as the engine's `Store` trait:
- 31 CRUD methods for threads, steps, events, projects, docs, leases, missions, conversations
- In-memory `HashMap` caches backed by workspace files under `engine/`
- Human-readable knowledge/orchestrator files plus JSON runtime state under
  `engine/.runtime/`
- Fallback to workspace files on cache miss; terminal cleanup evicts memory but
  preserves persisted output

The router dual-writes compatibility conversation history to the relational
`Database` separately. That database is not the backing store for `HybridStore`.

### Channel Routing And Delivery

`should_route_to_engine_v2()` owns the rollout policy. `ENGINE_V2=false`
disables every Engine V2 route. With Engine V2 enabled, the gateway is always
eligible; `ENGINE_V2_CHANNELS` can additionally select the exact names `xmpp`,
`darkirc`, and `weechat`. Values are comma-separated, trimmed, and matched
case-insensitively by exact channel name. Empty and unknown values are ignored.

Engine `ResponseDelta` events flow through
`ChannelManager::send_status(StatusUpdate::StreamChunk)`. The gateway maps each
chunk to SSE. WASM channels deliberately treat chunks as a no-op because the
current WIT has no message-editing contract. Completed terminal text returns to
the agent's outer outbound handler, which applies `BeforeOutbound`, suppresses
empty responses, persists compatibility history, and calls
`ChannelManager::respond()` once with the original incoming metadata.

Approval, authentication, interrupt, clear, and new-thread controls use the
same user and engine-conversation scope as ordinary input. This includes
non-UUID DarkIRC and WeeChat scope keys. Approval and auth prompts are statuses;
gate pauses, auth pauses, and stopped turns return an empty no-reply sentinel so
the outer handler does not send or persist a duplicate terminal response.

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
| `LlmBackend` | `complete(messages, actions, config) -> LlmOutput` | `LlmProvider` |
| `Store` | 31 CRUD methods for all engine types | Workspace-backed `HybridStore` |
| `EffectExecutor` | `execute_action(name, params, lease, ctx) -> ActionResult` | `ToolRegistry` + `SafetyLayer` |

## Data Retention Policy

Thread messages, steps, and events are **never deleted** from the engine
workspace store. Runtime JSON lives under `engine/.runtime/`; knowledge and
orchestrator artifacts live in human-readable paths under `engine/`. Raw
attachment bytes are the deliberate exception: only sanitized attachment text
is durable, while provider-bound image parts remain transient. The router also
dual-writes V1-compatible conversation history to the relational database.

"Cleanup" of terminal threads means writing a compact archive and evicting
in-memory cache entries to bound RAM; workspace files remain. `load_thread()`,
`load_steps()`, and `load_events()` reload persisted files on a cache miss.

## Event Sourcing

Every thread records a complete event log via `ThreadEvent`. The `EventKind` enum has 20 variants covering the full lifecycle: thread creation, state transitions, message receipt, LLM calls, action execution, gate decisions, mission triggers, and more.

This enables:
- Full replay of any thread's execution history
- Retrospective analysis via `executor/trace.rs`
- Learning mission triggers based on event patterns
- Debugging and auditing

## Configuration

```bash
ENGINE_V2=true                 # Enable Engine V2 (default: false)
ENGINE_V2_CHANNELS=            # Gateway only (default)
ENGINE_V2_CHANNELS=xmpp        # Add XMPP
ENGINE_V2_CHANNELS=xmpp,weechat # Add exact eligible channels
SKILL_SELF_IMPROVEMENT=false   # Gate skill feedback/patch/prune lifecycle
```

The engine inherits most configuration from the host daemon (LLM provider settings, database config, tool registry, safety settings). Engine-specific behavior is controlled through `ThreadConfig` at thread spawn time.

## Build & Test

```bash
taskset -c 0-5 cargo check -j6 -p lunarwing_engine
taskset -c 0-5 cargo clippy -j6 -p lunarwing_engine --all-targets -- -D warnings
taskset -c 0-5 cargo test -j6 -p lunarwing_engine -- --test-threads=6
```

## Key Design Decisions

1. **No dependency on main crate** -- clean separation, testable in isolation
2. **No safety logic in engine** -- sanitization/leak detection applied at the adapter boundary (`EffectExecutor` impl)
3. **Event sourcing from day one** -- every thread records a complete event log
4. **Tier 0 + Tier 1** -- structured tool calls and embedded Python via Monty coexist
5. **Engine owns its message type** -- `ThreadMessage` is simpler than `ChatMessage`; bridge adapters handle conversion
6. **RLM pattern** -- context as variable (not attention input), recursive `llm_query()`, compact output metadata between steps
7. **Fail-closed by construction** -- `GateDecision` has no `None` variant, `ResumeKind` is a closed enum
8. **Never delete LLM output** -- engine workspace files are preserved; only
   in-memory caches are evicted (V1 compatibility history is dual-written to the
   relational database)
