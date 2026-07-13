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

### Task 0: Close the engine event subscription race

**Files:**
- Modify: `ic/src/bridge/router.rs` (`handle_with_engine_inner`, gate resume
  paths, `await_thread_outcome`)

- [ ] **Step 1: Add failing lifecycle tests**

Add these tests to `bridge::router::tests`. The first exercises the real
conversation/thread managers with the existing immediate `NoopLlm`; the second
defines the required completion-drain behavior:

```rust
#[tokio::test]
async fn message_execution_subscribes_before_immediate_deltas() {
    let store = Arc::new(TestStore::new());
    let state = make_expected_test_state(store);
    let conversation_id = state
        .conversation_manager
        .get_or_create_conversation("gateway", "alice")
        .await
        .expect("conversation should be created");

    let (thread_id, mut event_rx) = handle_user_message_with_event_receiver(
        &state,
        conversation_id,
        "reply immediately",
        state.default_project_id,
        "alice",
        lunarwing_engine::ThreadConfig::default(),
        None,
    )
    .await
    .expect("thread should start");

    let first_delta = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let event = event_rx.recv().await.expect("event sender should remain open");
            if event.thread_id == thread_id
                && let lunarwing_engine::EventKind::ResponseDelta { content } = event.kind
            {
                break content;
            }
        }
    })
    .await
    .expect("first delta should not be missed");

    assert_eq!(first_delta, "done");
    let _ = state.thread_manager.join_thread(thread_id).await;
}

#[test]
fn pending_thread_events_are_drained_in_order() {
    let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(8);
    let thread_id = lunarwing_engine::ThreadId::new();
    let other_thread_id = lunarwing_engine::ThreadId::new();

    for (id, content) in [
        (thread_id, "one"),
        (other_thread_id, "ignore"),
        (thread_id, "two"),
    ] {
        event_tx
            .send(lunarwing_engine::ThreadEvent::new(
                id,
                lunarwing_engine::EventKind::ResponseDelta {
                    content: content.to_string(),
                },
            ))
            .expect("receiver should be active");
    }

    let drained = drain_pending_thread_events(&mut event_rx, thread_id);
    let contents: Vec<_> = drained
        .into_iter()
        .filter_map(|event| match event.kind {
            lunarwing_engine::EventKind::ResponseDelta { content } => Some(content),
            _ => None,
        })
        .collect();

    assert_eq!(contents, ["one", "two"]);
}
```

Run:

```bash
taskset -c 0-5 cargo test -j6 --lib \
  bridge::router::tests::message_execution_subscribes_before_immediate_deltas \
  -- --exact --nocapture
taskset -c 0-5 cargo test -j6 --lib \
  bridge::router::tests::pending_thread_events_are_drained_in_order \
  -- --exact --nocapture
```

Expected: compilation fails because
`handle_user_message_with_event_receiver` and `drain_pending_thread_events` do
not exist.

- [ ] **Step 2: Subscribe before spawn/resume**

Create the broadcast receiver before every operation that can start engine
execution. Pass it into `await_thread_outcome`; do not create a replacement
receiver there.

Add this focused helper and use it from `handle_with_engine_inner`:

```rust
async fn handle_user_message_with_event_receiver(
    state: &EngineState,
    conversation_id: lunarwing_engine::ConversationId,
    content: &str,
    project_id: lunarwing_engine::ProjectId,
    user_id: &str,
    thread_config: ThreadConfig,
    preferred_thread_id: Option<lunarwing_engine::ThreadId>,
) -> Result<(
    lunarwing_engine::ThreadId,
    tokio::sync::broadcast::Receiver<lunarwing_engine::ThreadEvent>,
), Error> {
    let event_rx = state.thread_manager.subscribe_events();
    let thread_id = state
        .conversation_manager
        .handle_user_message(
            conversation_id,
            content,
            project_id,
            user_id,
            thread_config,
            preferred_thread_id,
        )
        .await
        .map_err(|error| engine_err("thread error", error))?;
    Ok((thread_id, event_rx))
}
```

For approval/authentication resumes, call `subscribe_events()` immediately
before `resume_thread()`. Extend `await_thread_outcome` with an `event_rx`
argument and remove its internal subscription.

- [ ] **Step 3: Drain completion-time events**

When the thread is no longer running, drain matching events already queued on
the receiver and deliver them before joining and emitting the terminal response.
Log receiver lag and retain final-response reconciliation as the recovery path.

```rust
fn drain_pending_thread_events(
    event_rx: &mut tokio::sync::broadcast::Receiver<lunarwing_engine::ThreadEvent>,
    thread_id: lunarwing_engine::ThreadId,
) -> Vec<lunarwing_engine::ThreadEvent> {
    let mut events = Vec::new();
    loop {
        match event_rx.try_recv() {
            Ok(event) if event.thread_id == thread_id => events.push(event),
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => {
                tracing::warn!(
                    thread_id = %thread_id,
                    skipped,
                    "engine event receiver lagged while draining"
                );
            }
            Err(
                tokio::sync::broadcast::error::TryRecvError::Empty
                | tokio::sync::broadcast::error::TryRecvError::Closed,
            ) => break,
        }
    }
    events
}
```

Extract the existing per-event forwarding into a small
`deliver_thread_event` helper so normal receives and completion draining use
the same gateway/non-gateway mapping. Re-run both tests; expected: PASS.

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

- [x] **Step 2: Update `FEATURE_PARITY.md`** to reflect gateway incremental
  streaming as user-visible.

- [x] **Step 3: Static checks**

```bash
cargo clippy -j6 --lib -- -D warnings
cargo fmt --all -- --check   # my regions clean; pre-existing skill-patch fmt noise is unrelated
git diff --check
```

- [ ] **Step 4: End-to-end UI check** — run the gateway, send a prompt, confirm
  at least two ordered `stream_chunk` events followed by one terminal response
  and a single markdown-rendered final replacement, including on a new thread's
  first message.

## Notes / boundaries

- Interrupt-aware cancellation is Phase 4; WASM channel enablement is Phase 5.
- Pre-existing dual-path emission for non-delta events (tool cards, thinking) is
  out of scope; the channel-neutral delivery migration owns collapsing it.
