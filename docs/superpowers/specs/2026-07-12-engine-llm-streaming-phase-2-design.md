# Engine LLM Streaming Phase 2 Design

## Status

Approved direction, awaiting review of this written specification.

This design follows the completed provider-layer Phase 1 work in
`docs/proposals/ENGINE_LLM_STREAMING.md` and the approved channel-neutral
delivery design in
`docs/superpowers/specs/2026-07-12-engine-v2-channel-neutral-delivery-design.md`.

## Context

Phase 1 upgraded Rig to 0.40, added native plain and tool-capable streaming for
TensorZero Gateway 2026.3.2, and preserved stream chunks through the complete
production provider/decorator chain. The host now exposes provider-neutral
`LlmStreamChunk` values, but Engine V2 still calls the blocking
`LlmBackend::complete()` method from its primary orchestrator path.

The engine crate already defines its own provider-neutral `LlmStreamChunk` and
`LlmBackend::complete_stream()` fallback. The missing work is to connect the
host stream to that engine contract, consume it without changing the final
`LlmOutput` semantics, and publish engine-native text delta events.

Phase 2 is deliberately engine-internal. It does not deliver deltas through a
channel, change the gateway UI, or enable Engine V2 for WASM channels.

## Decision

Use a thin bridge adapter plus a reusable engine stream collector.

- `LlmBridgeAdapter` owns conversion between host and engine request/chunk
  types. It does not buffer a native stream or decide what its final response
  means.
- A focused engine executor module owns stream validation, accumulation, and
  reconstruction into the existing `LlmOutput` type.
- The primary orchestrator `__llm_complete__` handler consumes the stream and
  broadcasts transient `EventKind::ResponseDelta` events for non-empty text
  chunks.
- Compaction, scripting sub-calls, mission calls, and other engine LLM
  consumers remain blocking in this phase.

This keeps provider translation at the host boundary and execution semantics
inside the engine without expanding the Phase 0 stream contract.

## Goals

- Map the host's native plain and tool-capable streams into the engine stream
  type without buffering or losing chunk boundaries.
- Make the primary Engine V2 CodeAct/orchestrator call consume
  `complete_stream()`.
- Reconstruct the same `LlmOutput` variants and token usage produced by the
  existing blocking path.
- Emit provider-neutral text delta thread events in arrival order.
- Reject incomplete or ambiguous streams before the engine executes an action
  or commits token usage.
- Preserve blocking fallback compatibility for test and third-party
  `LlmBackend` implementations.
- Keep current channel behavior and the gateway-only Engine V2 default
  unchanged.

## Non-Goals

- Translating `ResponseDelta` into `StatusUpdate::StreamChunk`.
- Emitting gateway `AppEvent`, SSE, WebSocket, WIT, XMPP, IRC, or WeeChat
  protocol values.
- Changing terminal response delivery through `Channel::respond()`.
- Rendering or batching deltas in the frontend.
- Streaming tool argument fragments to users.
- Adding interrupt-aware stream cancellation; that remains Phase 4.
- Migrating compaction, scripting, mission, or other auxiliary engine LLM calls
  to streaming.
- Enabling Engine V2 for any additional channel.

## Architecture

### Bridge Adapter

`LlmBridgeAdapter::complete_stream()` uses the same conversion and routing
rules as `complete()`:

- `ThreadMessage` values become host `ChatMessage` values;
- `ActionDef` values become host `ToolDefinition` values unless `force_text`
  is set;
- depth zero uses the primary provider and depth greater than zero uses the
  configured cheap provider when present;
- default `max_tokens`, default temperature, tool choice, and opaque metadata
  remain identical to the blocking path.

Blocking and streaming methods share private request-building helpers so their
defaults cannot drift.

For a request without tools, the adapter opens `LlmProvider::complete_stream()`.
For a request with tools, it opens
`LlmProvider::complete_with_tools_stream()`. It maps each host chunk directly:

```text
host TextDelta       -> engine TextDelta
host ToolCallDelta   -> engine ToolCallDelta
host Done            -> engine Done
host LlmError        -> EngineError::Llm
```

Host `u32` usage counters are widened to engine `u64` counters. Cost remains
zero because the current host stream terminal value does not carry cost data.

### Engine Stream Collector

A new focused executor module at
`crates/lunarwing_engine/src/executor/llm_stream.rs` consumes an `LlmStream`
and returns an `LlmOutput`. It receives a small synchronous callback for each
non-empty text delta. This keeps accumulation independent of broadcast channel
details and makes the collector directly unit-testable.

The collector owns:

- ordered text concatenation;
- tool-call accumulation by numeric index;
- terminal and EOF validation;
- final tool argument JSON parsing;
- response classification;
- terminal usage selection.

The collector does not know about host providers, TensorZero, Rig, channels,
SSE, or Monty values.

### Orchestrator Integration

The primary `handle_llm_complete()` path opens `complete_stream()` and passes
the stream to the collector. Its text callback constructs a
`ThreadEvent::new(thread.id, EventKind::ResponseDelta { content })` and sends it
through the existing optional broadcast sender.

Sending a delta is best effort. A missing receiver or broadcast send failure
does not fail the LLM call. Delta events are not appended to `thread.events`:
provider chunk boundaries are transient delivery data, and persisting every
chunk would inflate thread history and serialized state.

After successful reconstruction, the handler updates `total_tokens` and
returns the same Monty dictionary shape used today. If collection fails, it
returns the existing LLM runtime error shape and does not commit usage.

Only this primary handler changes to streaming. Existing auxiliary calls to
`LlmBackend::complete()` stay unchanged.

## Data Flow

```text
Engine V2 __llm_complete__
    -> LlmBridgeAdapter::complete_stream
       -> primary or cheap host provider
       -> plain or tool-capable host stream
       -> one-for-one host-to-engine chunk mapping
    -> engine stream collector
       -> TextDelta: append + transient ResponseDelta broadcast
       -> ToolCallDelta: accumulate privately by numeric index
       -> Done: capture terminal usage and validate terminal position
       -> EOF: reconstruct LlmOutput
    -> existing Monty response dictionary
    -> existing orchestrator behavior
```

No `StatusUpdate`, `AppEvent`, or channel protocol type crosses this path.

## Reconstruction Semantics

### Text

Every non-empty `TextDelta` is appended in arrival order and immediately
offered to the callback. Empty text chunks are ignored for event emission but
do not affect stream validity.

Phase 2 emits provider truth. Text preceding a later tool call and fenced
CodeAct output also produce `ResponseDelta` events. The events remain internal
until Phase 3 defines presentation and terminal-response reconciliation.

When a stream has no tool calls, the accumulated text is classified through a
shared `LlmResponse::from_text` constructor. The existing fenced-code parser
moves behind that constructor so blocking and streaming paths classify
`Text` versus `Code` identically without duplicate parsing logic.

### Tool Calls

Tool fragments are keyed by their numeric index and emitted as final action
calls in ascending index order. For each index, the collector stores:

- the first non-empty ID;
- the first non-empty action name;
- concatenated argument fragments.

Repeating the same ID or name is accepted. A later different non-empty ID or
name for the same index is an error. At successful termination, every tool
call must have a non-empty ID and name, and its accumulated arguments must
parse as JSON.

If at least one complete tool call exists, the final response is
`LlmResponse::ActionCalls`. Accumulated text becomes its optional `content`;
an empty string becomes `None`. Tool calls take precedence over text or fenced
code classification.

Tool fragments never generate `ResponseDelta` events themselves.

### Terminal Usage

Exactly one `Done` chunk is required. Its optional usage maps to engine
`TokenUsage`; absent usage becomes `TokenUsage::default()`. The finish reason
is transport metadata and does not override the reconstructed response shape.
The presence of complete tool calls determines `ActionCalls`.

The collector verifies that `Done` is the last chunk and that the stream then
ends. This retains Phase 1's strict terminal semantics across the engine
boundary.

## Error Handling And Invariants

The Phase 2 LLM path returns `EngineError::Llm` for:

- stream acquisition failure mapped by the bridge;
- any streamed provider error;
- EOF before `Done`;
- more than one `Done` chunk;
- any chunk after `Done`;
- conflicting IDs or action names for one tool index;
- a tool call missing its final ID or action name;
- malformed final tool argument JSON.

An error after one or more text chunks may occur after transient deltas have
already been broadcast. The engine does not retry or fail over at that point
and does not create a synthetic `Done` or terminal result. Phase 1 provider
decorators already enforce the no-retry-after-first-chunk boundary.

Token usage is added to the orchestrator total only after successful
collection. No ambiguous tool call reaches action execution.

## Event Contract

Add this provider-neutral engine variant:

```rust
EventKind::ResponseDelta {
    content: String,
}
```

It derives the same debug, clone, serialization, and deserialization behavior
as other `EventKind` variants. It carries no gateway thread string, SSE event
name, channel metadata, provider identifier, or tool arguments. The enclosing
`ThreadEvent` already supplies the typed engine thread ID and timestamp.

The event is live and transient in Phase 2. Phase 3 may translate it at the
bridge/router boundary, but Phase 2 does not add that match arm.

## Channel Compatibility

This phase supports the larger existing-channel goal by keeping the engine
contract transport-neutral. It does not claim channel delivery support on its
own.

- The gateway can later map `ResponseDelta` through
  `StatusUpdate::StreamChunk` to its existing SSE event.
- WASM channels may continue ignoring stream status while receiving one final
  response through their existing `Channel::respond()`/`on_respond` path.
- Engine V2 remains gateway-only by default.
- XMPP, DarkIRC, and WeeChat remain ineligible for Engine V2 opt-in until their
  terminal delivery, approval, authentication, interrupt, and scope-isolation
  tests pass.

If a channel cannot satisfy those gates, it remains on the legacy engine path
without requiring an engine/provider fork.

## Alternatives Considered

### Collect Inside `handle_llm_complete()`

This uses fewer modules initially, but embeds terminal validation, tool
assembly, JSON parsing, and response classification in an already substantial
orchestrator function. It is harder to test directly and cannot be reused by a
future engine consumer. Rejected in favor of a focused collector.

### Put `LlmOutput` In `Done`

This makes the adapter reconstruct the response and gives the executor a final
object directly. It also expands the Phase 0 stream contract, duplicates
streamed content in a terminal chunk, and moves engine semantics into the host
bridge. Rejected because of its larger compatibility surface.

### Include Channel Delivery In Phase 2

Mapping the new event to `StatusUpdate` would make this phase user-visible and
mix engine correctness with routing/SSE behavior. Rejected to preserve the
approved Phase 2/Phase 3 boundary.

## Expected File Ownership

- `ic/src/bridge/llm_adapter.rs`: shared request construction and native host
  stream mapping.
- `ic/crates/lunarwing_engine/src/types/step.rs`: shared text/code response
  classification.
- `ic/crates/lunarwing_engine/src/types/event.rs`: `ResponseDelta` event type.
- `ic/crates/lunarwing_engine/src/executor/llm_stream.rs`: strict collector and
  focused unit tests.
- `ic/crates/lunarwing_engine/src/executor/mod.rs`: collector module wiring.
- `ic/crates/lunarwing_engine/src/executor/orchestrator.rs`: primary streaming
  call and transient event broadcast.
- `docs/proposals/ENGINE_LLM_STREAMING.md`: Phase 2 implementation status after
  verification.

No channel, web, frontend, WIT, database, configuration, or feature-parity file
is expected to change in Phase 2.

## Required Test Matrix

All categories below are required for Phase 2 completion.

### Bridge Adapter Tests

- Plain host text and terminal usage map to the engine stream exactly.
- Fragmented tool-call chunks preserve index, optional identity fields, and
  argument fragments.
- Primary versus cheap provider selection remains depth-dependent.
- `force_text`, token limit, temperature, metadata, and tool-choice behavior
  match blocking request construction.
- Stream acquisition errors and item errors become `EngineError::Llm`.

### Collector Tests

- Multiple text chunks preserve order in callbacks and final text.
- Empty text chunks do not emit events.
- Fenced CodeAct text reconstructs `LlmResponse::Code` identically to blocking
  completion.
- Fragmented, interleaved tool calls reconstruct in numeric index order.
- Repeated identical identity fragments are accepted.
- Tool text becomes optional action-call content.
- Terminal usage is preserved; missing usage becomes zero/default usage.
- EOF before `Done` fails.
- Multiple `Done` chunks fail.
- A text, tool, or terminal chunk after `Done` fails.
- Conflicting tool IDs or names fail.
- Missing final tool IDs or names fail.
- Malformed tool argument JSON fails.
- A mid-stream provider error propagates after earlier callbacks without a
  final output.

### Orchestrator Integration Tests

- The primary `__llm_complete__` path calls `complete_stream()` rather than
  `complete()`.
- Delta events are broadcast in provider order.
- Delta events are not appended to `thread.events`.
- The returned Monty dictionary remains identical for text, code, and action
  responses.
- Token totals update once after success and remain unchanged after failure.
- Running without an event receiver still completes normally.

### Regression Tests

- An `LlmBackend` that implements only `complete()` still works through the
  Phase 0 blocking stream fallback.
- Existing compaction, scripting, mission, and auxiliary blocking-call tests
  continue to pass.
- Existing engine event serialization remains compatible with the new variant.
- No gateway, channel, or frontend event is emitted by Phase 2 code.

## Verification

Run targeted tests first, then the affected crate and host checks from `ic/`.
Every Cargo command must use the repository's six-thread constraint.

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine <targeted_test> -- --exact --nocapture
taskset -c 0-5 cargo test -j6 --lib bridge::llm_adapter::tests:: -- --nocapture
taskset -c 0-5 cargo test -j6 -p lunarwing_engine -- --nocapture
taskset -c 0-5 cargo check -j6 -p lunarwing_engine
taskset -c 0-5 cargo check -j6 --lib
taskset -c 0-5 cargo clippy -j6 -p lunarwing_engine --all-targets -- -D warnings
taskset -c 0-5 cargo fmt --all -- --check
git diff --check
```

No debug `cargo build` is part of verification.

## Success Criteria

Phase 2 is complete when:

1. The real `LlmBridgeAdapter` opens native plain and tool-capable host streams.
2. The primary Engine V2 orchestrator path consumes those streams.
3. Valid streams reconstruct existing `LlmOutput` behavior for text, code,
   tools, and usage.
4. Non-empty text chunks produce ordered, transient `ResponseDelta` events.
5. Every required strictness and integration test passes.
6. No user-visible delivery, channel routing, or Engine V2 enablement behavior
   changes.

## Deferred To Later Phases

- Phase 3 translates engine deltas to channel-neutral statuses and implements
  gateway SSE/frontend reconciliation, including batching or replacement
  policy for intermediate CodeAct/tool-preface text.
- Phase 4 makes active stream consumption interrupt-aware and verifies that a
  cancelled stream cannot produce a successful terminal response.
- Phase 5 enables eligible WASM channels only after the approved delivery and
  scope safety gates pass.

`FEATURE_PARITY.md` remains unchanged because Phase 2 is not user-visible.
