# Engine LLM Streaming Phase 3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Engine V2 gateway responses stream incrementally in the web UI by
translating the engine's transient `EventKind::ResponseDelta` events into the
existing `stream_chunk` SSE wire event, finalizing on the existing terminal
`response` event.

**Architecture:** Add two match arms at the bridge boundary in
`ic/src/bridge/router.rs`. The gateway is served by the direct SSE path
(`thread_event_to_app_events`), which carries the real engine thread id and
scopes by `message.user_id`. The channel-neutral `StatusUpdate::StreamChunk`
path (`forward_event_to_channel`) is wired for non-gateway channels only, to
avoid double-emitting to the gateway and to plumb the Phase 5 rollout. No engine,
SSE, gateway-channel, or frontend changes are required — those layers already
support `StreamChunk`.

**Tech Stack:** Rust 2024, existing LunarWing bridge/channel types,
`lunarwing_engine` event types, vanilla-JS gateway frontend (unchanged).

---

### Task 1: Gateway SSE delivery of deltas

**Files:**
- Modify: `ic/src/bridge/router.rs` (`thread_event_to_app_events`)

- [x] **Step 1: Add the `ResponseDelta` arm**

Before the terminal `_ => vec![]` arm, map
`EventKind::ResponseDelta { content }` to a single
`AppEvent::StreamChunk { content: content.clone(), thread_id: Some(thread_id.into()) }`.

- [x] **Step 2: Confirm no other layer needs edits**

`AppEvent::StreamChunk` → `SseEvent::StreamChunk` → wire `stream_chunk` and the
`static/app.js` `stream_chunk` handler already exist.

### Task 2: Channel-neutral status path (non-gateway)

**Files:**
- Modify: `ic/src/bridge/router.rs` (`forward_event_to_channel`)

- [x] **Step 1: Add the guarded `ResponseDelta` arm**

Add `EventKind::ResponseDelta { content } if channel_name != "gateway" =>` that
sends `StatusUpdate::StreamChunk(content.clone())` via
`channels.send_status(channel_name, …, metadata)`. The guard prevents duplicating
the gateway's streamed text (the gateway is served by the direct SSE path). WASM
channels ignore `StreamChunk` today; this plumbs Phase 5.

### Task 3: Tests

**Files:**
- Modify: `ic/src/bridge/router.rs` (`mod tests`)

- [x] **Step 1: Map test**

`response_delta_maps_to_stream_chunk_app_event`: build a `ThreadEvent` with
`EventKind::ResponseDelta { content: "hi" }`, call
`thread_event_to_app_events(&evt, "tid-1")`, assert exactly one
`AppEvent::StreamChunk { content: "hi", thread_id: Some("tid-1") }`.

- [x] **Step 2: Gateway-skip test**

`response_delta_skips_gateway_but_forwards_to_other_channels`: register two
`StubChannel`s (`"gateway"`, `"xmpp"`) in a `ChannelManager`; assert
`forward_event_to_channel` sends no status to `"gateway"` and one
`StatusUpdate::StreamChunk("hi")` to `"xmpp"`.

- [x] **Step 3: Run focused tests**

```bash
cargo test -j6 --lib bridge::router::tests::response_delta -- --nocapture
```

Expected: both tests pass.

### Task 4: Docs and verification

**Files:**
- Modify: `docs/proposals/ENGINE_LLM_STREAMING.md`
- Modify: `ic/FEATURE_PARITY.md`

- [x] **Step 1: Update Phase 3 status** in the proposal (implemented;
  gateway-visible via direct SSE path; non-gateway `StatusUpdate` plumbed).

- [ ] **Step 2: Update `FEATURE_PARITY.md`** to reflect gateway incremental
  streaming as user-visible.

- [x] **Step 3: Static checks**

```bash
cargo clippy -j6 --lib -- -D warnings
cargo fmt --all -- --check   # my regions clean; pre-existing skill-patch fmt noise is unrelated
git diff --check
```

- [ ] **Step 4: End-to-end UI check** — run the gateway, send a prompt, confirm
  incremental text then single markdown-rendered final replacement, including on
  a new thread's first message.

## Notes / boundaries

- Interrupt-aware cancellation is Phase 4; WASM channel enablement is Phase 5.
- Pre-existing dual-path emission for non-delta events (tool cards, thinking) is
  out of scope; the channel-neutral delivery migration owns collapsing it.
