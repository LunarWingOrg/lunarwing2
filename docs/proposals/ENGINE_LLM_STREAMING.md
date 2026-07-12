# Engine LLM Streaming

Add true token-level streaming to the LLM stack so engine responses render
incrementally in the gateway (and so interrupt-mid-generation and long agentic
runs become tractable). Today every LLM call — legacy and engine — is a
blocking `complete()`: the full response is generated before anything reaches
the user. For a ~14k-token context that is ~9s of dead air per turn.

## Why this is foundational (not polish)

- **Interrupt mode** is near-meaningless without streaming. A blocking call is
  an opaque 9s block; you can cancel before or after, but not cut into the
  model mid-thought. Real interrupt needs a token stream that is cancellable
  in-flight.
- **Self-improvement / long agentic runs** — multi-step CodeAct with large
  generations currently shows the user nothing until each step finishes.
  Streaming gives live visibility (and a better signal for progress/stuck
  detection: you can *see* tokens flowing, not just infer from step boundaries).
- **Perceived latency** — the current "feels slow" is largely masked
  time-to-first-token. Streaming fixes the feel on any endpoint.

## Current state (verified in-tree)

The blocking path, bottom to top:

1. **`LlmProvider` trait** (`ic/src/llm/provider.rs:363`) — the legacy provider
   contract. Methods: `complete()`, `complete_with_tools()`, `model_name()`,
   cost/model helpers. **No streaming method.** Many implementations, several of
   them **decorators** that wrap another provider: `CircuitBreakerProvider`,
   `FailoverProvider`, `TimeoutProvider`, `RetryProvider`, `SmartRoutingProvider`,
   `CachedProvider`, `RecordingLlm`, plus concrete `LunarWingCloudChatProvider`
   and the OpenAI-compatible registry path.
2. **`LlmBackend` trait** (`ic/crates/lunarwing_engine/src/traits/llm.rs`) — the
   engine's contract. Exactly `complete(messages, actions, config) -> LlmOutput`
   and `model_name()`. **No streaming method** — the engine structurally cannot
   stream today.
3. **`LlmBridgeAdapter`** (`ic/src/bridge/llm_adapter.rs`) — wraps an
   `Arc<dyn LlmProvider>` as an `LlmBackend`, converting `ThreadMessage` ↔
   `ChatMessage` and calling `complete` / `complete_with_tools`.
4. **Engine executor** — the Python orchestrator's `__llm_complete__` host
   function awaits the full `LlmBackend::complete()` (blocking) per step.
5. **`await_thread_outcome`** (`ic/src/bridge/router.rs`) — streams *coarse*
   thread events to SSE (Thinking / ToolStarted / ToolCompleted) but the final
   answer is one `AppEvent::Response` after the whole thread completes.
6. **SSE layer** (`ic/src/channels/web/sse.rs`) — `SseEvent` enum already has a
   **`StreamChunk` variant** (wire name `"stream_chunk"`), alongside `Response`,
   `Thinking`, etc.
7. **Gateway frontend** (`app.js`) — ALREADY has streaming scaffolding:
   `_streamingThreadId`, `getOrCreateStreamingMessage(threadId)`,
   `finalizeStreamingMessage(threadId, content)`, and a `resetStreamingState()`.
   The `response` listener calls `finalizeStreamingMessage` (finalize a
   streamed bubble) with a fallback to `addMessage`.

**Two prior-author breadcrumbs confirm the intended design:**
- `ic/src/channels/web/openai_compat.rs:590-596` (LunarWing's *own*
  OpenAI-compatible server endpoint) already fakes streaming by word-splitting a
  completed response, with the comment: *"The current LlmProvider returns
  complete responses (no streaming method)… True token streaming can be added
  later by extending LlmProvider with a `complete_stream()` method."*
- `SseEvent::StreamChunk` already exists — the chunk-delivery wire event is in
  place; nothing consumes it from the engine path yet.

So the missing link is specifically the **provider→backend→engine streaming
path**; the SSE event type and frontend rendering are largely present.

## Design

### The streaming primitive
Add a streaming completion to both trait layers, returning an async stream of
typed chunks rather than a single result. Proposed chunk type (shared, in
`ic/src/llm/`):

```rust
pub enum LlmStreamChunk {
    /// Incremental assistant text (the common case).
    TextDelta(String),
    /// Incremental tool/action call (name + partial args), for CodeAct/tool use.
    ToolCallDelta { index: usize, name: Option<String>, args_delta: String },
    /// Terminal: usage/stop reason; stream ends after this.
    Done { usage: Option<TokenUsage>, finish_reason: String },
}
```

- **`LlmProvider`** gains `async fn complete_stream(&self, request) ->
  Result<BoxStream<'_, Result<LlmStreamChunk, LlmError>>, LlmError>` with a
  **default impl that falls back to `complete()` and yields the whole response
  as one `TextDelta` + `Done`.** This is the key to a safe rollout: every
  existing provider/decorator compiles and works unchanged; only providers that
  *natively* stream override it.
- **`LlmBackend`** (engine) gains the analogous `complete_stream`, also with a
  `complete()`-backed default.

### Layer-by-layer changes

1. **OpenAI-compatible provider** (the one that hits `LLM_BASE_URL` /
   TensorZero): implement real `complete_stream` — send `"stream": true`, parse
   the SSE `data:` lines into `TextDelta`/`ToolCallDelta`, terminate on
   `[DONE]`. Reference the existing SSE-parse shapes already in
   `openai_compat.rs` (`OpenAiDelta`, the streaming response types at
   `:123-138`).
2. **Decorators** (`Retry`, `Failover`, `CircuitBreaker`, `Timeout`,
   `SmartRouting`, `Cached`, `Recording`): forward `complete_stream` to the
   inner provider, preserving each decorator's semantics. These need care —
   documented per-decorator below (see "Decorator semantics").
3. **`LlmBridgeAdapter`**: implement `LlmBackend::complete_stream` by calling the
   provider's `complete_stream` and mapping `LlmStreamChunk` → engine chunk type.
4. **Engine executor / `__llm_complete__`**: consume the stream. Emit an engine
   `ThreadEvent` per text delta (a new `EventKind::ResponseDelta { thread_id,
   text }`) as tokens arrive, accumulating the full text for the step's final
   result (so tool-call parsing and step state are unchanged downstream).
5. **`await_thread_outcome` / bridge**: map `ResponseDelta` thread-events →
   `SseEvent::StreamChunk { thread_id, delta }` (the variant already exists), and
   keep the terminal `AppEvent::Response` as the finalize signal (frontend
   already calls `finalizeStreamingMessage` on `response`).
6. **Frontend**: wire a `stream_chunk` SSE listener → `getOrCreateStreamingMessage`
   + append delta (scaffolding already present). `response` stays the finalizer.

### Interrupt integration
The engine already has a signal/interrupt channel (`SignalReceiver` in the
execution loop). Streaming makes interrupt real: while consuming the chunk
stream, check the interrupt signal between chunks and **drop the stream**
(cancel the in-flight HTTP body) on interrupt, then transition the thread. This
is the concrete payoff — design the stream-consume loop to be cancellation-aware
from day one.

## Decorator semantics (the delicate part)
A new trait method means every decorator must forward it correctly, and
streaming changes some decorators' contracts:

- **Retry / Failover / CircuitBreaker**: with a blocking call they retry on a
  returned `Err`. With streaming, an error can occur **mid-stream** (after
  chunks already emitted to the user). Decision: retry/failover only apply to
  **pre-first-chunk** failures (connect/handshake). Once the first chunk is
  delivered, a mid-stream error surfaces to the caller (no silent re-stream —
  that would duplicate visible text). Document this explicitly.
- **Timeout**: apply to time-to-first-chunk, not total duration (a long
  legitimate generation must not be killed).
- **Cached**: a cache hit yields the whole cached text as one `TextDelta` +
  `Done` (trivially correct via the default).
- **Recording**: record the reassembled full text (accumulate deltas), so
  recordings stay comparable to the blocking path.
- All others: forward via the default `complete()`-backed impl until/unless
  native streaming is worth it.

## Phased rollout

- **Phase 0 — traits + default fallback (no behavior change).** Add
  `LlmStreamChunk`, `complete_stream` to both traits with `complete()`-backed
  defaults; decorators forward. Everything compiles; nothing streams yet.
  Verifies the abstraction with zero risk. `cargo check`.
- **Phase 1 — native provider streaming.** Implement `complete_stream` on the
  OpenAI-compatible provider (`stream:true` + delta parse). Add decorator
  forwarding with the semantics above. Unit-test the SSE parse + fallback.
- **Phase 2 — engine consumes the stream.** Engine `__llm_complete__` consumes
  chunks, emits `EventKind::ResponseDelta`; accumulate full text for step
  result. Gate behind a flag (`ENGINE_STREAMING=true`) initially.
- **Phase 3 — SSE + frontend.** Map `ResponseDelta` → `SseEvent::StreamChunk`;
  wire the frontend `stream_chunk` listener to the existing streaming-message
  scaffolding. This is where the user *sees* incremental output.
- **Phase 4 — interrupt-aware consume loop.** Make the chunk loop check the
  interrupt signal and cancel the in-flight stream. Unlocks true interrupt mode.
- **Phase 5 (later) — legacy path + tool-call streaming.** Optionally stream the
  legacy agentic loop too, and stream partial tool-call args for CodeAct.

## Testing
- Trait default: a non-streaming provider yields exactly one `TextDelta` + `Done`
  equal to its `complete()` output.
- OpenAI provider: parse a recorded `stream:true` SSE transcript into the right
  chunk sequence; malformed/`[DONE]` handling; mid-stream error surfaces.
- Decorators: retry/failover fire pre-first-chunk, NOT mid-stream; timeout on
  TTFC; cached/recording reassemble correctly.
- Engine: streamed step accumulates the same final text as the blocking path
  (parity test — streamed vs `complete()` produce identical thread outcome).
- Interrupt: a mid-stream interrupt cancels the HTTP body and transitions the
  thread without emitting a spurious final Response.
- Frontend: `stream_chunk` appends to the streaming bubble; `response`
  finalizes; out-of-thread chunks ignored (isCurrentThread).

## Build constraints
`taskset -c 0-5 cargo check -j6` / scoped `cargo test`. No full debug builds.
Static assets are `include_bytes!`-compiled — the frontend change needs a
release rebuild / tenant upgrade to take effect (same as prior gateway work).

## Non-goals / risks
- Not changing the CodeAct "always use tools" behavior (separate concern; see
  the over-working discussion — that's a prompt-design decision).
- Not a rewrite of the provider stack — the default-fallback design keeps every
  existing provider working; native streaming is opt-in per provider.
- Main risk is the decorator semantics (retry/failover mid-stream) — hence they
  get explicit rules and tests, and Phase 0/1 land them before anything user-
  visible depends on them.
- TensorZero must actually stream on the configured function/endpoint; if a
  given function doesn't, the default fallback degrades gracefully to blocking.

## Open questions
- Chunk granularity for SSE: per-token (chattiest, smoothest) vs. small batches
  (fewer SSE frames). Start per-provider-chunk, batch if frame overhead shows.
- Should `ENGINE_STREAMING` be its own flag or implied by `ENGINE_V2`? Proposed:
  separate flag through Phase 2-3, fold into default once proven.
- Tool-call streaming (partial args) — defer to Phase 5; text streaming first.

