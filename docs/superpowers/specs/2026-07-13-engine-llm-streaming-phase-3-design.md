# Engine LLM Streaming Phase 3 Design

## Status

Implemented and live verified.

This design follows the completed Phase 2 work in
`docs/proposals/ENGINE_LLM_STREAMING.md` and the approved channel-neutral
delivery design in
`docs/superpowers/specs/2026-07-12-engine-v2-channel-neutral-delivery-design.md`.

## Context

Phase 2 made the primary Engine V2 orchestrator consume native provider streams
and broadcast transient `EventKind::ResponseDelta { content }` thread events
(`ic/crates/lunarwing_engine/src/executor/orchestrator.rs`). Those events were
deliberately not user-visible: no bridge match arm translated them into a
channel status or a gateway SSE event.

Phase 3 makes gateway streaming user-visible by translating `ResponseDelta` into
the existing `stream_chunk` SSE wire event. The wire types and the browser
frontend already exist from prior work:

- `AppEvent::StreamChunk { content, thread_id }`
  (`ic/crates/lunarwing_common/src/event.rs`) and its `SseEvent` mapping
  (`ic/src/channels/web/types.rs`).
- `GatewayChannel::send_status` maps `StatusUpdate::StreamChunk` to
  `AppEvent::StreamChunk` (`ic/src/channels/web/mod.rs`).
- `static/app.js` already handles the `stream_chunk` event: it buffers deltas
  into a per-thread streaming bubble (`getOrCreateStreamingMessage` /
  `flushStreamBuffer`) and finalizes on the terminal `response` event by
  replacing the bubble with the markdown-rendered authoritative text
  (`finalizeStreamingMessage`).

The only missing link was the bridge translation in
`ic/src/bridge/router.rs`.

## Decision

Translate `EventKind::ResponseDelta` at the bridge boundary in
`await_thread_outcome`'s two existing per-event dispatchers:

1. `thread_event_to_app_events` (direct gateway SSE path) gains a `ResponseDelta`
   arm returning `AppEvent::StreamChunk { content, thread_id: Some(<engine
   thread id>) }`. This is the user-visible gateway delivery.
2. `forward_event_to_channel` (channel-neutral `StatusUpdate` path) gains a
   `ResponseDelta` arm that sends `StatusUpdate::StreamChunk(content)` — but only
   for **non-gateway** channels.
3. Subscribe to the engine event broadcaster before each new-message or
   interactive gate-resolution operation whose result is delivered through
   `await_thread_outcome`, then pass that receiver into the outcome handler.
   When execution finishes, drain events already queued on that receiver before
   emitting the authoritative terminal response.

## Event capture ordering

Tokio broadcast receivers observe only events sent after subscription. Engine
threads start in a background task before `handle_user_message` returns, so
subscribing inside `await_thread_outcome` can miss the first provider deltas.
The same race exists when an approval or authentication resume continues into
`await_thread_outcome`.

The bridge therefore establishes the receiver before each spawn/resume
operation routed into `await_thread_outcome`. The outcome handler consumes that
existing receiver rather than creating a new one. Once the thread is no longer
running, all engine sends have completed, but some events can still be queued
locally; those matching the thread are drained and delivered before terminal
response reconciliation.

Receiver lag remains best-effort: it is logged, and the final response still
replaces the partial browser bubble. Phase 3 does not make transient deltas
durable or replayable.

## The double-emission constraint

For a gateway message, `await_thread_outcome` already runs **both** dispatchers,
and both reach the browser's SSE stream (`EngineState.sse` is the same
`SseManager` the browser subscribes to). `GatewayChannel::name()` is `"gateway"`
and gateway messages carry `channel: "gateway"`, so
`forward_event_to_channel` → `channels.send_status("gateway", …)` hits
`GatewayChannel::send_status` → SSE, while `thread_event_to_app_events` →
`sse.broadcast_for_user` also emits to SSE.

Because the frontend appends deltas additively (`_streamBuffer += content`),
emitting a delta through both paths would duplicate streamed text. Therefore the
gateway is served by exactly one path (the direct SSE path), and the
`StatusUpdate` arm is guarded to skip `"gateway"`.

## The thread-id / scoping constraint

The direct SSE path always uses the real **engine** thread id (`tid_str`) and
scopes by `message.user_id`. `GatewayChannel::send_status` instead derives
`thread_id` from `metadata` (present only if the request already had a thread)
and scopes by `metadata["user_id"]`, falling back to a global broadcast when
absent. On a new thread's first message the `send_status` path would carry
`thread_id: None`, mis-key the streaming bubble, and leave an orphaned partial
bubble plus a duplicate final message. Routing the gateway delta through the
direct SSE path avoids this entirely.

## Delta / reconciliation policy

"Stream all, replace at end." Every step's `ResponseDelta` (including tool-preface
text and fenced CodeAct output) accumulates into one per-thread bubble; the
terminal `response` event replaces the bubble with the authoritative final text.
No per-step reset is added. This is existing frontend behavior and requires no
JS change.

## Non-Goals

- Interrupt-aware stream cancellation (Phase 4).
- Enabling Engine V2 for WASM channels (Phase 5). The non-gateway
  `StatusUpdate::StreamChunk` wiring is inert today (WASM channels ignore
  `StreamChunk`) but plumbs that rollout.
- Adding event and terminal-response delivery to the standalone OAuth callback
  resume. That callback has no `Agent` / `IncomingMessage` delivery context and
  requires its own lifecycle design.
- Collapsing the two gateway dispatch paths. Any pre-existing duplication for
  non-delta events is out of scope; the channel-neutral delivery migration owns
  it.
- Batching/coalescing adjacent deltas or streaming tool argument fragments.

## Files Changed

- `ic/src/bridge/router.rs`: `ResponseDelta` arms in `thread_event_to_app_events`
  and `forward_event_to_channel`; pre-execution event subscription and
  completion draining; unit tests.
- `docs/proposals/ENGINE_LLM_STREAMING.md`: Phase 3 status.
- `ic/FEATURE_PARITY.md`: gateway incremental streaming is now user-visible.

## Test Matrix

- `response_delta_maps_to_stream_chunk_app_event`: a `ResponseDelta` event maps
  to exactly one `AppEvent::StreamChunk` carrying the content and engine thread
  id.
- `response_delta_skips_gateway_but_forwards_to_other_channels`:
  `forward_event_to_channel` sends no status to a `"gateway"` channel for a
  `ResponseDelta` but sends one `StatusUpdate::StreamChunk` to a non-gateway
  channel.
- `message_execution_subscribes_before_immediate_deltas`: an immediate engine
  backend cannot emit its first response delta before the bridge receiver is
  established.
- `pending_thread_events_are_drained_in_order`: deltas already queued when a
  thread completes are retained in order while events for other threads are
  ignored.

## Verification

From `ic/` (six-thread constraint; prefix with the environment proxy for
network fetches, and `taskset -c 0-5` on Linux hosts):

```bash
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests:: -- --nocapture
taskset -c 0-5 cargo test -j6 -p lunarwing_engine -- --nocapture
taskset -c 0-5 cargo clippy -j6 --lib -- -D warnings
taskset -c 0-5 cargo fmt --all -- --check
git diff --check
```

End-to-end: run the gateway, send a prompt in the web UI, confirm assistant text
appears incrementally then is replaced once by the final markdown-rendered
answer — including on a brand-new thread's first message. Capture the SSE event
sequence to verify at least two ordered `stream_chunk` events precede exactly
one terminal `response` event for the same thread.

Live protocol verification against the `brightdawn` tenant and TensorZero
Gateway `2026.3.2` passed for both a new-thread first message (3 chunks) and an
existing-thread continuation (93 chunks). Each case produced exactly one later
terminal response on the same thread, showed no paired duplicate emission, and
matched DB-backed persisted history. No receiver-lag warning or engine delivery
error was logged. The user also confirmed incremental text rendering in the
live Gateway UI.
