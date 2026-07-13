# Engine LLM Streaming

Add true token-level streaming to the LLM stack so Engine V2 responses can
eventually render incrementally in the gateway, support cancellation during
generation, and expose progress during long agentic runs.

## Status

- **Phase 0 is complete** in commit `1c844d8`: the host and engine trait layers
  have provider-neutral stream types and blocking fallbacks.
- **Host Phase 1 is complete on the implementation branch**: Rig 0.40 streams
  plain and tool-capable requests, and the complete production decorator chain
  preserves those streams.
- **Phase 2 is complete on the implementation branch**: the host bridge maps
  native streams, the primary Engine V2 orchestrator consumes them strictly,
  and engine text deltas are broadcast as transient provider-neutral events.
- **Phase 3 is complete on the implementation branch and is user-visible.** The
  bridge translates engine `ResponseDelta` events into the gateway SSE
  `stream_chunk` event (via the direct SSE path in `await_thread_outcome`), and
  the existing web frontend renders deltas incrementally, finalizing on the
  terminal `response`. The channel-neutral `StatusUpdate::StreamChunk` path is
  additionally wired for non-gateway channels (inert today; WASM channels ignore
  `StreamChunk`) to plumb the Phase 5 rollout.
- **Phase 4 engine cancellation and interrupt-aware ingress are implemented on
  the integration branch; the corrected Brightdawn live gate is pending.** Each
  running thread owns a `tokio_util` `CancellationToken`; a stop
  cancels it (in addition to the existing between-step `ThreadSignal::Stop`) so
  an in-flight provider stream — whether still acquiring or mid-collection — is
  dropped promptly. A cancelled LLM call is control flow, not an error: the
  thread returns `ThreadOutcome::Stopped` and commits no terminal assistant
  reply, no usage from the cancelled call, no cache entry, and no recording
  trace. Cancellation is isolated by owner and conversation scope, resumed and
  subsequent turns get fresh tokens, and the gateway `Submission::Interrupt` is
  routed to Engine V2 (returning a single `Interrupted.` acknowledgement). The
  host dispatcher now continues polling channel input while one ordinary handler
  is active: exact `/interrupt` and `/stop` controls use the normal scoped route,
  while every other message remains FIFO in a 256-entry bounded queue with an
  explicit busy response on overflow. The original agent-level regression first
  failed with zero responses after two seconds, then passed with the dispatcher;
  five ingress lifecycle tests now cover acquisition cancellation, FIFO priority,
  overflow, legacy fallback, and soft-timeout preservation. Phase 4 remains open
  until Brightdawn repeats the TensorZero `2026.3.2` live gate successfully.
- **WASM channel delivery is still not covered.** That remains Phase 5.
- **TensorZero Gateway `2026.3.2` is the compatibility target.** LunarWing uses
  its OpenAI-compatible `/openai/v1/chat/completions` endpoint.

The next work is repeating the corrected Phase 4 Brightdawn live gate, then
finishing opt-in WASM channel delivery (Phase 5). Provider, engine, and host
dispatcher plumbing are no longer the blocker.

## Why this is foundational

- **Interrupt mode:** a blocking request is opaque until completion. A stream
  can be dropped between chunks so the in-flight HTTP body is cancelled.
- **Long agentic runs:** multi-step CodeAct can expose meaningful progress
  instead of waiting for a complete generation at every step.
- **Perceived latency:** provider chunks make time-to-first-token observable
  and deliverable instead of hiding it inside `complete()`.

## Scope

Phase 1 owns the provider boundary and decorators:

```text
RigAdapter
  -> RetryProvider
  -> SmartRoutingProvider
  -> FailoverProvider
  -> CircuitBreakerProvider
  -> CachedProvider
  -> TimeoutProvider
  -> RecordingLlm
```

It does not change engine execution or channel behavior. Engine V2 remains
gateway-only by default. Any future WASM channel opt-in must follow the
channel-neutral delivery design in
[`2026-07-12-engine-v2-channel-neutral-delivery-design.md`](../superpowers/specs/2026-07-12-engine-v2-channel-neutral-delivery-design.md).

## Implemented host contract

`LlmProvider` exposes both plain and tool-capable streams:

```rust
pub enum LlmStreamChunk {
    TextDelta(String),
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        args_delta: String,
    },
    Done {
        usage: Option<TokenUsage>,
        finish_reason: String,
    },
}
```

- `complete_stream()` has a blocking fallback that emits one text chunk and
  one `Done` chunk.
- `complete_with_tools_stream()` has a blocking fallback that emits complete
  text/tool chunks and `Done`.
- Tool-capable streaming is required in Phase 1 because Engine V2 normally
  supplies actions. Supporting only plain streaming would leave CodeAct turns
  blocking.
- User-visible rendering of partial tool arguments is still deferred. The host
  contract preserves them now so the engine can make that decision later.

## Rig and TensorZero behavior

`RigAdapter` uses Rig 0.40's typed streaming API for plain and tool-capable
requests. Compatibility tests use captured local SSE frames and do not require
a live gateway.

The TensorZero `2026.3.2` wire contract relevant to LunarWing is:

- JSON SSE `data:` frames end with `data: [DONE]`.
- Rig 0.40 sends `stream_options.include_usage=true`; usage arrives in the
  terminal response rather than ordinary deltas.
- Tool-call IDs can appear only in the first chunk while names and arguments
  arrive incrementally. TensorZero's numeric tool index remains stable.
- OpenAI-shaped mid-stream `{"error": ...}` events terminate the stream. Rig
  0.40 surfaces them; the previous Rig 0.30 parser could silently skip them.
- `delta.tensorzero_extra_content` can contain thought or unknown blocks. The
  generic Rig OpenAI parser does not expose those blocks, and Phase 1 does not
  add a reasoning chunk type, so this content remains intentionally deferred.

LunarWing maps Rig tool deltas by `internal_call_id`, assigning numeric indexes
in first-seen order. It does not key by the provider tool ID because that ID can
be absent on early deltas.

Rig's portable final event exposes usage but not a portable finish reason.
LunarWing therefore reports `tool_calls` after any tool event and `stop`
otherwise. A stream must contain Rig's `Final` event; EOF before `Final` is an
`InvalidResponse`, not a successful partial completion.

## Decorator semantics

Streaming changes when a decorator can safely commit state. Each production
decorator now has an explicit policy.

### Retry and failover

Retry and failover may switch attempts after setup failure or an error in the
first stream item. The first successful chunk is the commit point. After that
chunk, later errors pass through unchanged and no second provider is opened,
preventing duplicate visible text.

Failover binds the selected provider to the current Tokio task before returning
the stream so model attribution remains request-scoped under concurrency.

### Circuit breaker

- Setup and mid-stream transient errors count as failures.
- `Done` is the success commit point.
- Opening a stream or receiving an ordinary delta is not success.
- Dropping a stream before `Done` leaves breaker state unchanged.

### Timeout

`TimeoutProvider` retains its existing absolute total turn budget. One deadline
covers acquisition, retries/failover inside the wrapper, and every stream poll.
A visible chunk can therefore be followed by one retryable timeout error, but
never a synthetic `Done`.

This is deliberately not a time-to-first-chunk-only timeout. Weakening the
existing watchdog would again allow stacked retries to exceed the agent turn
budget and trigger the hard-kill path that clears queued follow-up messages. A
separate idle or TTFC timeout can be designed later without replacing the hard
total budget.

### Cache

- A cache hit emits the cached text as one `TextDelta` plus `Done`.
- A miss forwards native chunks unchanged and inserts only when `Done` is
  consumed.
- Errors, premature EOF, and consumer drop never cache a partial response.
- Blocking and streaming plain requests share cache keys and entries.
- Tool-capable requests bypass the cache because replay could repeat side
  effects.

### Smart routing

Simple, complex, and tool routes delegate directly to native provider streams.
Tool-capable requests always use the primary provider.

The moderate cascade is the one buffered path: SmartRouting consumes the cheap
candidate through `Done`, reconstructs its response, and checks uncertainty.
It then either replays the exact cheap chunks or discards them and opens the
primary stream. No cheap delta escapes before the escalation decision.

### Recording

Recording forwards chunks immediately but appends a trace response only at
`Done`. Text is concatenated, tool fragments are accumulated by numeric index,
and complete arguments are parsed as JSON. Malformed arguments are preserved as
strings. Errors, premature EOF, and consumer drop do not record a partial
assistant response.

## Remaining delivery path

The bridge and primary Engine V2 executor now consume native provider streams,
strictly reconstruct the existing text/code/action result, and broadcast
transient provider-neutral `ResponseDelta` thread events. Provider streaming is
still not user-visible because the bridge/router does not translate those
events into channel statuses yet.

1. The bridge/router must convert those events to channel-neutral application
   events.
2. Gateway SSE can map those events to the existing `stream_chunk` wire shape;
   the terminal response remains the finalization signal.
3. Interrupt handling must drop the active stream without emitting a spurious
   successful terminal response.

The gateway UI remains the first supported consumer. WASM channels stay on the
current legacy-compatible path unless explicitly enabled after terminal
delivery, approval, interrupt, and scope-isolation tests pass. If that contract
cannot be made reliable for a WASM channel, Engine V2 remains gated to the
gateway rather than exposing partial support.

## Rollout

- **Phase 0 - complete (`1c844d8`):** shared stream types and blocking
  fallbacks, no behavior change.
- **Phase 1 - implemented:** Rig 0.40 native plain/tool streams, TensorZero
  `2026.3.2` fixtures, strict terminal handling, and decorator preservation.
- **Phase 2 - implemented:** the host bridge maps native streams, the primary
  Engine V2 orchestrator consumes them strictly, and engine text deltas are
  broadcast as transient provider-neutral events.
- **Phase 3 - implemented:** the bridge maps engine `ResponseDelta` to the
  gateway SSE `stream_chunk` event via the direct SSE path; the existing web
  frontend appends deltas and finalizes on the terminal `response`. The
  non-gateway `StatusUpdate::StreamChunk` path is wired (inert today) for the
  Phase 5 rollout. Gateway delta and status are emitted through a single path to
  avoid duplicating streamed text.
- **Phase 4 - implementation locally verified; corrected live gate pending:** per-thread
  `CancellationToken` ownership in `ThreadManager`, cancellation propagated
  through `ExecutionLoop` into the orchestrator's LLM host call (wrapping stream
  acquisition and collection in `run_until_cancelled`), a typed
  `ThreadOutcome::Stopped` that resumes neither Monty nor failure/rollback
  accounting, terminal-only decorator state preserved on stream drop, and a
  scoped gateway interrupt route. The between-step `ThreadSignal::Stop` contract
  is retained. The 2026-07-13 Brightdawn gate proved that the old serialized
  agent loop did not dequeue `/interrupt` while an ordinary handler was active.
  The corrective dispatcher is now implemented and locally verified: exact
  interrupts overtake a bounded FIFO of ordinary messages without introducing
  ordinary-message concurrency, and active shutdown aborts and awaits its task.
  Brightdawn must still repeat the live TensorZero `2026.3.2` cancellation gate.
- **Phase 5:** opt-in channel-neutral delivery for eligible WASM channels after
  the approved safety gates pass.

## Validation

Phase 1 coverage includes:

- blocking fallback parity for plain and tool-capable requests;
- native Rig text, usage, tool-index, duplicate-full-call, strict EOF, and
  mid-stream error mapping;
- TensorZero `2026.3.2` text, usage, fragmented tool-call, `[DONE]`, and error
  fixtures;
- pre-first-chunk retry/failover boundaries and post-first-chunk error pass
  through;
- breaker terminal accounting, absolute stream deadlines, terminal-only cache
  insertion, buffered moderate routing, and terminal-only trace recording;
- full-chain plain and tool composition tests proving exact chunk order without
  duplicate first items.

Phase 1 verification uses scoped tests plus `cargo check`, Clippy with warnings
denied, formatting checks, and `git diff --check`, all under the repository's
six-thread build constraint.

Phase 2 coverage adds host-to-engine plain/tool chunk mapping, request/default
parity, depth routing, setup and item error mapping, strict engine terminal and
tool reconstruction, complete-only fallback compatibility, response-delta
Serde coverage, and real orchestrator text/code/tool/error/no-receiver streams.
The integration tests also prove ordered live delta broadcast, non-persistence
of provider chunks, and terminal-only usage commitment.

Phase 4 ingress coverage adds an agent-level pending-acquisition regression. It
was observed RED against the old loop (`0` responses after the two-second
deadline) and GREEN after the dispatcher. The final isolated target passes five
tests covering one acknowledgement with no cancelled terminal response, FIFO
priority across scopes, the real 256-message overflow response, unmatched-scope
legacy fallback without cross-thread cancellation, and soft-timeout detachment.
The full Engine crate passes 322 tests; focused agent-loop, dispatcher, and bridge
suites pass 17, 2, and 30 tests respectively. Default, PostgreSQL-only, and
libSQL-only checks, formatting, and zero-warning Clippy also pass under the
six-thread constraint. These are local gates, not a substitute for Brightdawn.

`FEATURE_PARITY.md` is intentionally unchanged in Phase 2 because no
user-facing channel consumes native provider deltas yet.

## Non-goals and risks

- No change to CodeAct's prompt or "always use tools" behavior.
- No user-visible streaming claim until the engine and delivery phases land.
- No reasoning stream is promised while `tensorzero_extra_content` is omitted.
- No automatic WASM channel migration; gateway-first remains the safe default.
- Provider deltas can split at arbitrary UTF-8-safe string boundaries, so
  consumers must append chunks rather than treating them as words or tokens.

The primary remaining risk has moved from provider parsing to lifecycle
delivery: ensuring terminal response, interrupt, approval, thread scope, and
channel scope remain coherent while deltas are in flight.

## Open questions

- Whether Engine V2 streaming should use a dedicated gateway-scoped flag or be
  enabled automatically for gateway Engine V2 after Phase 2 validation.
- Whether gateway SSE should forward provider chunks directly or batch small
  adjacent text deltas after measuring frame overhead.
- Which WASM channels, if any, can satisfy the channel-neutral delivery gates
  without weakening the gateway-only default.
