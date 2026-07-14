# Engine LLM Streaming Phase 4 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make an Engine V2 interrupt promptly cancel an in-flight provider
stream, return `ThreadOutcome::Stopped`, and commit no terminal response, usage
from the cancelled LLM call, cache entry, or recording response.

**Architecture:** Give every `RunningThread` its own
`tokio_util::sync::CancellationToken`. `ThreadManager::stop_thread()` cancels
that token and also sends the existing `ThreadSignal::Stop`, preserving the
current between-step signal contract. The token is passed through
`ExecutionLoop` to the orchestrator and wraps the complete stream operation, so
cancellation drops stream acquisition or the active HTTP body immediately and
returns a typed stopped result instead of an LLM error.

**Tech Stack:** Rust 2024, MSRV 1.92, Tokio, `tokio-util 0.7.18`
`CancellationToken`, `futures 0.3` streams, Engine V2 thread events, existing
gateway/channel response suppression.

**Status (2026-07-13): Complete.** The cancellation implementation, corrective
interrupt-aware dispatcher, local verification matrix, Brightdawn release
deployment, and live TensorZero `2026.3.2` gate all passed. Phase 5 remains
separate.

---

## Fixed Decisions And Boundaries

- Cancellation is owned by `lunarwing_engine`, where running thread lifetime is
  owned. Do not put provider-specific cancellation in Rig, TensorZero, reqwest,
  or the host bridge.
- Use one newly created token for each call to `ThreadManager::start_thread()`.
  A resumed thread receives a fresh token. Do not reuse a cancelled token.
- `stop_thread()` and `stop_thread_system()` call `cancel()` first, then send
  `ThreadSignal::Stop`. The signal remains necessary for stops observed between
  LLM calls and for non-streaming orchestrator steps.
- Cancellation wraps both `complete_stream().await` and stream collection. This
  covers a provider blocked before response headers as well as one blocked on a
  later SSE frame.
- `CancellationToken::run_until_cancelled()` is completion-biased when completion
  and cancellation become ready in the same poll. A stream that has already
  reached its terminal `Done` may therefore complete normally; cancellation
  tests use a pending stream so they prove the interrupt path deterministically.
- A cancelled LLM call is control flow, not `EngineError::Llm`. It returns
  `ThreadOutcome::Stopped`, transitions the thread through `Completed` to `Done`,
  and neither increments nor resets orchestrator failure/rollback accounting.
- Usage already committed by an earlier completed step remains accurate. The
  cancelled call contributes no `Done` usage and no `StepCompleted` accounting.
- Deltas broadcast before the stop request may already be visible or queued in
  the bridge receiver. Cancellation prevents any later provider poll from
  producing another delta, assistant message, synthetic `Done`, usage total,
  cache entry, or recording response. Phase 4 does not claim that an already
  queued SSE frame can be retracted or ordered ahead of the interrupt reply.
- The original message task returns `Some(String::new())` for `Stopped`; the
  interrupt submission itself may return the single acknowledgement
  `Interrupted.` through the normal channel response path.
- Until Phase 5 removes direct terminal SSE, ordinary gateway Engine V2 input
  retains the existing result-swallowing branch. Returning completed text to the
  outer response handler in Phase 4 would emit the terminal response twice.
- Phase 4 wires the gateway interrupt only. Phase 5 generalizes control routing
  to opt-in XMPP, DarkIRC, and WeeChat.
- Do not preserve partial output as a terminal assistant message. That is a
  separate product decision tracked by the existing partial-output parity item.
- Do not change the WIT interface, frontend protocol, database schema, provider
  SSE parser, timeout policy, or automatic WASM installation behavior.

## File Map

- Modify `ic/crates/lunarwing_engine/Cargo.toml`: enable `tokio-util`'s `rt`
  feature for `CancellationToken`.
- Modify `ic/Cargo.lock`: record the new direct engine dependency; the locked
  version remains `0.7.18`.
- Modify `ic/crates/lunarwing_engine/src/runtime/manager.rs`: own one token per
  running thread and make both stop entry points cancel it.
- Modify `ic/crates/lunarwing_engine/src/executor/loop_engine.rs`: carry the
  token into orchestrator execution.
- Modify `ic/crates/lunarwing_engine/src/executor/orchestrator.rs`: distinguish
  cancellation from success/error and stop without resuming Monty with a fake
  LLM result.
- Modify `ic/src/bridge/router.rs`: share scoped conversation-key construction,
  make gateway interrupt target the correct engine conversation, and suppress a
  stopped turn's terminal response/history entry.
- Modify `ic/src/agent/agent_loop.rs`: route gateway `Submission::Interrupt` to
  Engine V2 while preserving Phase 3 result suppression for ordinary gateway
  user input and leaving every other non-user control on its existing path.
- Modify `ic/src/llm/response_cache.rs`: add a consumer-drop regression test.
- Modify `ic/src/llm/recording.rs`: add a consumer-drop regression test.
- Modify `docs/proposals/ENGINE_LLM_STREAMING.md`: mark Phase 4 implemented only
  after all automated and live gates pass.
- Check `ic/FEATURE_PARITY.md`: keep partial-output-on-abort unsupported; update
  only the streaming note if it needs to mention interrupt-safe gateway streams.

### Task 1: Add Per-Running-Thread Cancellation Ownership

**Files:**
- Modify: `ic/crates/lunarwing_engine/Cargo.toml`
- Modify: `ic/Cargo.lock`
- Modify: `ic/crates/lunarwing_engine/src/runtime/manager.rs`

- [ ] **Step 1: Write the manager-level failing cancellation test**

In `runtime::manager::tests`, add a stream wrapper whose drop is observable and
an LLM backend that emits one delta and then remains pending:

```rust
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll};

use futures::{Stream, StreamExt};
use tokio::sync::Notify;

use crate::traits::llm::{LlmStream, LlmStreamChunk};

struct DropProbeStream {
    inner: LlmStream<'static>,
    dropped: Arc<AtomicBool>,
}

impl Stream for DropProbeStream {
    type Item = Result<LlmStreamChunk, EngineError>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

impl Drop for DropProbeStream {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

struct PendingStreamLlm {
    opened: Notify,
    dropped: Arc<AtomicBool>,
    stream_calls: AtomicUsize,
}

#[async_trait::async_trait]
impl LlmBackend for PendingStreamLlm {
    async fn complete(
        &self,
        _: &[ThreadMessage],
        _: &[ActionDef],
        _: &LlmCallConfig,
    ) -> Result<LlmOutput, EngineError> {
        Err(EngineError::Llm {
            reason: "blocking completion must not be called".into(),
        })
    }

    async fn complete_stream<'a>(
        &'a self,
        _: &[ThreadMessage],
        _: &[ActionDef],
        _: &LlmCallConfig,
    ) -> Result<LlmStream<'a>, EngineError> {
        self.stream_calls.fetch_add(1, Ordering::SeqCst);
        self.opened.notify_one();
        let inner = futures::stream::iter([Ok(LlmStreamChunk::TextDelta(
            "before-cancel".into(),
        ))])
        .chain(futures::stream::pending())
        .boxed();
        Ok(Box::pin(DropProbeStream {
            inner,
            dropped: Arc::clone(&self.dropped),
        }))
    }

    fn model_name(&self) -> &str {
        "pending-stream"
    }
}
```

Add `stop_during_llm_stream_is_prompt_and_drops_stream`. It must subscribe to
events before spawning, wait for `ResponseDelta("before-cancel")`, invoke
`stop_thread(thread_id, "user")`, and require all of these assertions:

```rust
let outcome = tokio::time::timeout(
    Duration::from_secs(1),
    manager.join_thread(thread_id),
)
.await
.expect("cancelled stream should stop promptly")
.expect("thread should join");

assert!(matches!(outcome, ThreadOutcome::Stopped));
assert!(dropped.load(Ordering::SeqCst));

let stored = store
    .load_thread(thread_id)
    .await
    .expect("thread lookup should work")
    .expect("thread should be persisted");
assert_eq!(stored.state, ThreadState::Done);
assert_eq!(stored.total_tokens_used, 0);
assert!(!stored.messages.iter().any(|message| {
    message.role == MessageRole::Assistant
        && message.content.contains("before-cancel")
}));
```

Add a second backend whose `complete_stream()` signals that acquisition started,
creates a local drop probe, and then awaits `futures::future::pending()`. The test
`stop_during_stream_acquisition_is_prompt_and_drops_future` stops after the
start signal and requires `ThreadOutcome::Stopped`, the acquisition probe to be
dropped, zero deltas, zero usage, and no assistant message. This separately
proves cancellation before HTTP response headers; do not treat the active-stream
test as coverage for both boundaries.

- [ ] **Step 2: Run the focused test and confirm RED**

From `ic/` run:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  runtime::manager::tests::stop_during_llm_stream_is_prompt_and_drops_stream \
  -- --exact --nocapture
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  runtime::manager::tests::stop_during_stream_acquisition_is_prompt_and_drops_future \
  -- --exact --nocapture
```

Expected: both time out or return a non-`Stopped` outcome because
`ThreadSignal::Stop` is not polled while stream acquisition/collection is
pending.

- [ ] **Step 3: Add the dependency and token to `RunningThread`**

Add to `ic/crates/lunarwing_engine/Cargo.toml`:

```toml
tokio-util = { version = "0.7", features = ["rt"] }
```

In `manager.rs` import and store the token:

```rust
use tokio_util::sync::CancellationToken;

struct RunningThread {
    signal_tx: SignalSender,
    cancellation: CancellationToken,
    handle: tokio::task::JoinHandle<Result<ThreadOutcome, EngineError>>,
}
```

In `start_thread()`, create a new token for every execution and pass a clone to
the execution loop:

```rust
let cancellation = CancellationToken::new();

let mut exec_loop = ExecutionLoop::new(thread, llm, effects, leases, policy, rx, user_id)
    .with_cancellation_token(cancellation.clone())
    .with_capabilities(Arc::clone(&self.capabilities))
    .with_event_tx(self.event_tx.clone())
    .with_retrieval(retrieval)
    .with_store(Arc::clone(&self.store));
```

Store the original token in `RunningThread`.

- [ ] **Step 4: Centralize stop requests without holding the map lock across await**

Add this private helper and call it from both public stop methods:

```rust
async fn request_stop(&self, thread_id: ThreadId) -> Result<(), EngineError> {
    let (signal_tx, cancellation) = {
        let running = self.running.read().await;
        let running_thread = running
            .get(&thread_id)
            .ok_or(EngineError::ThreadNotFound(thread_id))?;
        (
            running_thread.signal_tx.clone(),
            running_thread.cancellation.clone(),
        )
    };

    cancellation.cancel();
    let _ = signal_tx.send(ThreadSignal::Stop).await;
    Ok(())
}
```

`stop_thread()` must retain its ownership check and then call
`self.request_stop(thread_id).await`. `stop_thread_system()` calls the same
helper without ownership validation.

- [ ] **Step 5: Keep the end-to-end cancellation tests RED for Task 2**

Run both manager tests again and confirm token ownership alone has not made them
pass. Do not commit a knowingly failing intermediate state. Task 2 completes the
cancellation path and commits the dependency, manager ownership, propagation,
and now-green tests together.

### Task 2: Propagate Cancellation Through The Execution Loop

**Files:**
- Modify: `ic/crates/lunarwing_engine/src/executor/loop_engine.rs`
- Modify: `ic/crates/lunarwing_engine/src/executor/orchestrator.rs`

- [ ] **Step 1: Add a cancellation result type and failing orchestrator test**

In `orchestrator.rs`, define the private result used only by the LLM host call:

```rust
enum LlmHostCallOutcome {
    Finished(ExtFunctionResult),
    Cancelled,
}
```

Add a test using a pending stream and a token. Spawn `execute_orchestrator`, wait
until the stream opens, cancel the token, and assert within one second:

```rust
assert!(matches!(result.outcome, ThreadOutcome::Stopped));
assert_eq!(result.tokens_used, TokenUsage::default());
assert_eq!(thread.state, ThreadState::Completed);
assert!(!thread.messages.iter().any(|message| {
    message.role == MessageRole::Assistant
}));
```

- [ ] **Step 2: Run the orchestrator test and confirm RED**

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  executor::orchestrator::tests::cancelled_llm_stream_returns_stopped_without_usage \
  -- --exact --nocapture
```

Expected: compilation fails because `execute_orchestrator` has no cancellation
argument and `handle_llm_complete` cannot return `Cancelled`.

- [ ] **Step 3: Carry a token in `ExecutionLoop`**

Add a non-optional field initialized to a fresh token so direct unit-test
construction remains valid:

```rust
use tokio_util::sync::CancellationToken;

pub struct ExecutionLoop {
    // existing fields
    cancellation: CancellationToken,
}

// In ExecutionLoop::new:
cancellation: CancellationToken::new(),

pub fn with_cancellation_token(mut self, cancellation: CancellationToken) -> Self {
    self.cancellation = cancellation;
    self
}
```

Pass `&self.cancellation` immediately after `&mut self.signal_rx` in the call to
`execute_orchestrator`.

- [ ] **Step 4: Wrap stream acquisition and collection as one cancellable future**

Add `cancellation: &CancellationToken` to `execute_orchestrator` and
`handle_llm_complete`. Replace the current open/collect sequence with:

```rust
let stream_result = cancellation
    .run_until_cancelled(async {
        let stream = llm.complete_stream(&messages, &actions, &config).await?;
        collect_llm_stream(stream, |content| {
            let Some(tx) = event_tx else {
                return;
            };
            let event = ThreadEvent::new(
                thread_id,
                EventKind::ResponseDelta {
                    content: content.to_string(),
                },
            );
            let _ = tx.send(event);
        })
        .await
    })
    .await;

match stream_result {
    Some(Ok(output)) => {
        LlmHostCallOutcome::Finished(llm_output_result(output, total_tokens))
    }
    Some(Err(error)) => {
        LlmHostCallOutcome::Finished(llm_error_result(error))
    }
    None => LlmHostCallOutcome::Cancelled,
}
```

This exact boundary matters: returning `None` drops the acquisition/collector
future, which owns and drops the provider stream. Do not fabricate an
`LlmStreamChunk::Done`.

- [ ] **Step 5: Return a stopped orchestrator result without resuming Monty**

At the `"__llm_complete__"` dispatch arm, handle cancellation before producing
an `ExtFunctionResult`:

```rust
match handle_llm_complete(
    args,
    thread,
    llm,
    effects,
    leases,
    &mut total_tokens,
    event_tx,
    cancellation,
)
.await
{
    LlmHostCallOutcome::Finished(result) => result,
    LlmHostCallOutcome::Cancelled => {
        if thread.state == ThreadState::Running {
            thread.transition_to(
                ThreadState::Completed,
                Some("stopped during LLM stream".into()),
            )?;
        }
        return Ok(OrchestratorResult {
            outcome: ThreadOutcome::Stopped,
            // Earlier completed steps remain accounted for. The cancelled call
            // never reached Done and therefore added nothing to this value.
            tokens_used: total_tokens,
        });
    }
}
```

Do not call `record_orchestrator_failure`, `sync_visible_outcome`, or
`llm_error_result` in the cancelled branch.

- [ ] **Step 6: Preserve orchestrator failure history on user cancellation**

In `ExecutionLoop`'s successful orchestrator branch, reset the failure tracker
only for outcomes other than `Stopped`:

```rust
if !matches!(&orch_result.outcome, ThreadOutcome::Stopped)
    && let Some(store) = self.store.as_ref()
{
    crate::executor::orchestrator::reset_orchestrator_failures(
        store,
        self.thread.project_id,
    )
    .await;
}
```

Add `executor::loop_engine::tests::stopped_outcome_does_not_reset_orchestrator_failures`.
Seed a nonzero failure tracker in the loop's test store, cancel its pending LLM
stream, and assert the stored count is unchanged. This distinguishes user
control flow from both a successful run and an orchestrator failure.

- [ ] **Step 7: Run the RED tests and existing stream suite**

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  executor::orchestrator::tests::cancelled_llm_stream_returns_stopped_without_usage \
  -- --exact --nocapture
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  runtime::manager::tests::stop_during_llm_stream_is_prompt_and_drops_stream \
  -- --exact --nocapture
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  runtime::manager::tests::stop_during_stream_acquisition_is_prompt_and_drops_future \
  -- --exact --nocapture
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  executor::loop_engine::tests::stopped_outcome_does_not_reset_orchestrator_failures \
  -- --exact --nocapture
taskset -c 0-5 cargo test -j6 -p lunarwing_engine executor::llm_stream::tests \
  -- --nocapture
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  executor::loop_engine::tests::orchestrator_uses_native_text_stream_and_broadcasts_deltas \
  -- --exact --nocapture
```

Expected: all pass; existing strict `Done`, usage, text, code, and tool-stream
behavior remains unchanged on non-cancelled calls.

- [ ] **Step 8: Commit engine propagation**

```bash
git add ic/crates/lunarwing_engine/Cargo.toml ic/Cargo.lock \
  ic/crates/lunarwing_engine/src/runtime/manager.rs \
  ic/crates/lunarwing_engine/src/executor/loop_engine.rs \
  ic/crates/lunarwing_engine/src/executor/orchestrator.rs
git commit -m "feat(engine): cancel active LLM streams on stop"
```

### Task 3: Prove Isolation, Recovery, And Resume Semantics

**Files:**
- Modify: `ic/crates/lunarwing_engine/src/runtime/manager.rs`

- [ ] **Step 1: Add strict manager regression tests**

Add these tests using the pending/drop-probe backend from Task 1:

1. `stop_only_cancels_the_target_thread`: start two pending threads; stop the
   first; require first outcome `Stopped`, second still running, and only one
   stream dropped. Then stop and join the second to leave no task behind.
2. `new_thread_after_cancel_gets_fresh_token`: cancel and join one pending
   thread, then use a scripted finite stream for a second thread and require
   `Completed { response: Some("recovered") }`.
3. `resumed_thread_gets_fresh_token`: persist a `Waiting` thread, call
   `resume_thread`, wait for its new stream, stop it, and require `Stopped`.
4. `wrong_owner_cannot_cancel_stream`: call `stop_thread` with another owner,
   require `EngineError::AccessDenied`, and require the target remains running
   until the owning user stops it.

Use a one-second timeout for every join. Do not accept `Completed` or
`MaxIterations` as the old `stop_thread_works` test currently does.

- [ ] **Step 2: Tighten the existing permissive stop test**

Replace the broad assertion:

```rust
ThreadOutcome::Stopped | ThreadOutcome::Completed { .. } | ThreadOutcome::MaxIterations
```

with a deterministic pending-stream test that requires only:

```rust
assert!(matches!(outcome, ThreadOutcome::Stopped));
```

- [ ] **Step 3: Run all lifecycle tests**

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine runtime::manager::tests \
  -- --nocapture
```

Expected: target isolation, ownership, prompt stop, subsequent execution, and
resume-token freshness all pass.

- [ ] **Step 4: Commit lifecycle tests**

```bash
git add ic/crates/lunarwing_engine/src/runtime/manager.rs
git commit -m "test(engine): cover streaming cancellation lifecycle"
```

### Task 4: Wire Scoped Gateway Interrupt Delivery

**Files:**
- Modify: `ic/src/bridge/router.rs`
- Modify: `ic/src/agent/agent_loop.rs`

- [ ] **Step 1: Add pure scoped-key and stopped-result tests**

Extract and test:

```rust
fn engine_conversation_key(message: &IncomingMessage) -> String {
    match message.conversation_scope() {
        Some(scope) => format!("{}:{scope}", message.channel),
        None => message.channel.clone(),
    }
}

fn thread_outcome_response(outcome: &ThreadOutcome) -> Option<String> {
    match outcome {
        ThreadOutcome::Completed { response } => response.clone(),
        ThreadOutcome::Stopped => Some(String::new()),
        ThreadOutcome::MaxIterations => {
            Some("Reached maximum iterations without completing.".into())
        }
        ThreadOutcome::Failed { error } => Some(format!("Error: {error}")),
        ThreadOutcome::GatePaused { .. } => None,
    }
}
```

Required tests:

```rust
#[test]
fn engine_conversation_key_includes_non_gateway_scope() {
    let msg = IncomingMessage::new("xmpp", "alice", "hi")
        .with_conversation_scope("room@example.org");
    assert_eq!(engine_conversation_key(&msg), "xmpp:room@example.org");
}

#[test]
fn stopped_outcome_uses_empty_response_sentinel() {
    assert_eq!(
        thread_outcome_response(&ThreadOutcome::Stopped),
        Some(String::new())
    );
}
```

- [ ] **Step 2: Run the tests and confirm RED**

```bash
taskset -c 0-5 cargo test -j6 --lib \
  bridge::router::tests::engine_conversation_key_includes_non_gateway_scope \
  -- --exact --nocapture
taskset -c 0-5 cargo test -j6 --lib \
  bridge::router::tests::stopped_outcome_uses_empty_response_sentinel \
  -- --exact --nocapture
```

Expected: compilation fails because the helpers do not exist.

- [ ] **Step 3: Use one scoped key everywhere touched by Phase 4**

Use `engine_conversation_key(message)` in `handle_with_engine_inner`,
`handle_interrupt`, `handle_expected`, and `clear_engine_conversation`. This
fixes the current mismatch where ordinary messages use `channel:scope` but
interrupt and clear look up only `channel`.

- [ ] **Step 4: Suppress the original stopped turn**

Use `thread_outcome_response()` for completed/stopped/max-iteration/failed
outcomes while retaining the existing side-effectful `GatePaused` branch.
`ThreadOutcome::Stopped` returns `Some(String::new())`. Add `!text.is_empty()` to
the v1 compatibility-history write guard so no empty assistant row is stored:

```rust
if let Ok(Some(ref text)) = result
    && !text.is_empty()
    && let Some(ref db) = state.db
{
    write_v1_response(db, text).await;
}
```

- [ ] **Step 5: Route only gateway interrupts to the engine in Phase 4**

In the existing gateway-only Engine V2 branch in `handle_message`, add the
control case before legacy session/thread resolution. Keep ordinary user input's
Phase 3 result suppression until Phase 5 removes direct terminal SSE:

```rust
if crate::bridge::is_engine_v2_enabled() && message.channel == "gateway" {
    match &submission {
        Submission::UserInput { content } => {
            match crate::bridge::handle_with_engine(self, message, content).await {
                Ok(_) => {}
                Err(error) => {
                    tracing::error!(
                        message_id = %message.id,
                        error = %error,
                        "engine v2 message handling failed"
                    );
                }
            }
            return Ok(Some(String::new()));
        }
        Submission::Interrupt => {
            return crate::bridge::handle_interrupt(self, message).await;
        }
        _ => {}
    }
}
```

Update the old comment to describe both branches. Phase 5 will replace the
literal gateway check, remove result swallowing, and delete direct terminal SSE
as one atomic delivery change. Do not route approval, auth, clear, new-thread, or
other controls in this task.

- [ ] **Step 6: Add a bridge scope-isolation interrupt test**

Create two engine conversations for the same user with scopes `thread-a` and
`thread-b`, each running a pending stream. Call `handle_interrupt` with an
`IncomingMessage` scoped to `thread-a`. Require only A returns `Stopped`, B is
still running, and the result is `Some("Interrupted.")`. Cleanly stop B.

- [ ] **Step 7: Run bridge and agent parsing tests**

```bash
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests -- --nocapture
taskset -c 0-5 cargo test -j6 --lib agent::agent_loop::tests -- --nocapture
taskset -c 0-5 cargo test -j6 --lib agent::submission::tests -- --nocapture
```

- [ ] **Step 8: Commit the gateway interrupt route**

```bash
git add ic/src/bridge/router.rs ic/src/agent/agent_loop.rs
git commit -m "feat(engine): route scoped gateway interrupts"
```

### Task 5: Lock Down Terminal-Only Decorator State

**Files:**
- Modify: `ic/src/llm/response_cache.rs`
- Modify: `ic/src/llm/recording.rs`

- [ ] **Step 1: Add a cache consumer-drop test**

Open a scripted stream containing `TextDelta("partial")` followed by `Done`,
poll only the delta, drop the stream, and assert `cached.is_empty()`:

```rust
let mut stream = cached
    .complete_stream(simple_request())
    .await
    .expect("stream should open");
assert!(matches!(
    stream.next().await,
    Some(Ok(LlmStreamChunk::TextDelta(text))) if text == "partial"
));
drop(stream);
assert!(cached.is_empty());
```

- [ ] **Step 2: Add a recording consumer-drop test**

Use the same poll-one-then-drop pattern around `RecordingLlm`. Assert its trace
contains only the existing `TraceResponse::UserInput` marker and no text/tool
response.

- [ ] **Step 3: Run cache, recording, breaker, and full-chain tests**

```bash
taskset -c 0-5 cargo test -j6 --lib \
  llm::response_cache::tests::stream_drop_before_done_does_not_populate_cache \
  -- --exact --nocapture
taskset -c 0-5 cargo test -j6 --lib \
  llm::recording::tests::stream_drop_before_done_does_not_record_response \
  -- --exact --nocapture
taskset -c 0-5 cargo test -j6 --lib \
  llm::circuit_breaker::tests::stream_drop_before_done_does_not_mark_success \
  -- --exact --nocapture
taskset -c 0-5 cargo test -j6 --lib llm::tests::native_stream_survives_full_decorator_chain \
  -- --exact --nocapture
taskset -c 0-5 cargo test -j6 --lib llm::tests::native_tool_stream_survives_full_decorator_chain \
  -- --exact --nocapture
```

- [ ] **Step 4: Commit terminal-only state regressions**

```bash
git add ic/src/llm/response_cache.rs ic/src/llm/recording.rs
git commit -m "test(llm): preserve terminal-only state on stream drop"
```

### Task 6: Documentation And Local Verification

**Files:**
- Modify: `docs/proposals/ENGINE_LLM_STREAMING.md`
- Check/possibly modify: `ic/FEATURE_PARITY.md`
- Check: `docs/architecture/ENGINE-V2.md`

- [ ] **Step 1: Update status only after tests pass**

Mark Phase 4 implemented. Record these exact semantics: per-thread cancellation,
prompt stream drop, retained between-step stop signal, no terminal assistant
reply/usage/cache/record commit, gateway-only interrupt route, and Phase 5 still
pending.

- [ ] **Step 2: Run formatting and the complete focused verification matrix**

From `ic/`:

```bash
taskset -c 0-5 cargo fmt --all -- --check
taskset -c 0-5 cargo check -j6 -p lunarwing_engine
taskset -c 0-5 cargo check -j6
taskset -c 0-5 cargo test -j6 -p lunarwing_engine -- --test-threads=6
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests -- --test-threads=6
taskset -c 0-5 cargo clippy -j6 -p lunarwing_engine --all-targets -- -D warnings
taskset -c 0-5 cargo clippy -j6 --lib -- -D warnings
```

From the repository root:

```bash
git diff --check
rg -n "ThreadOutcome::Stopped.*Thread was stopped|message.channel == \"gateway\"" \
  ic/src/bridge/router.rs ic/src/agent/agent_loop.rs
```

Expected: checks and tests pass; the stopped terminal text is absent. The
gateway literal remains only in the explicit Phase 4 routing policy and is
removed by Phase 5.

- [ ] **Step 3: Commit documentation**

```bash
git add docs/proposals/ENGINE_LLM_STREAMING.md ic/FEATURE_PARITY.md
git commit -m "docs: record interrupt-safe engine streaming"
```

Stage `ic/FEATURE_PARITY.md` only if its wording changed.

### Task 7: Brightdawn Live Cancellation Gate

**Files:**
- No source-file edits during deployment.
- Preserve: `/home/brightdawn/lunarwing/env/`
- Preserve: `/home/brightdawn/lunarwing/state/`

- [ ] **Step 1: Push the implementation branch before touching the tenant**

```bash
git status --short --branch
git push origin feat/wire-engine-p345
```

Require a clean worktree and confirm local HEAD equals
`origin/feat/wire-engine-p345`.

- [ ] **Step 2: Update only the tenant source checkout**

Do not run an upgrade/import script, `git clean`, state copy, env regeneration,
`install-wasm`, or `build-tenant --with-wasm`.

```bash
sudo -n -u brightdawn git -C /home/brightdawn/lunarwing fetch origin \
  feat/wire-engine-p345
sudo -n -u brightdawn git -C /home/brightdawn/lunarwing switch \
  feat/wire-engine-p345
sudo -n -u brightdawn git -C /home/brightdawn/lunarwing merge --ff-only \
  origin/feat/wire-engine-p345
```

- [ ] **Step 3: Build the release binary manually in tmux**

```bash
tmux new-session -d -s phase4-brightdawn-build \
  "cd /home/brightdawn/lunarwing/ic && taskset -c 0-5 cargo build --release -j6 --bin lunarwing 2>&1 | tee /tmp/phase4-brightdawn-build.log"
```

Wait for completion and require exit status 0. This is the only build in the
plan; do not start a second Cargo process while it runs.

- [ ] **Step 4: Restart through the multi-tenant lifecycle owner**

```bash
sudo -n ic/scripts/lunarwing-mt-admin.sh restart-tenant brightdawn
sudo -n ic/scripts/lunarwing-mt-admin.sh status brightdawn
```

- [ ] **Step 5: Exercise real TensorZero 2026.3.2 cancellation**

In the gateway UI, start a prompt that produces a long response, wait for at
least two visible chunks, then submit the normal interrupt command. Require:

- the interrupt acknowledgement arrives within two seconds;
- any `stream_chunk` already broadcast before the stop request is recorded as a
  delivery race, then the chunk count remains stable for at least one second;
- the provider stream drop and `ThreadOutcome::Stopped` occur before that
  quiescence window begins;
- no terminal response finalizes the cancelled generation;
- no assistant history row contains the partial generation;
- a new prompt in the same gateway thread streams and completes normally;
- a prompt in a second gateway thread is not interrupted by the first thread's
  interrupt;
- service logs contain no orchestrator failure/rollback increment, receiver lag,
  panic, or duplicate response.

- [ ] **Step 6: Record evidence and rollback criteria**

Record timestamps, thread IDs, ordered event counts, acknowledgement latency,
history result, subsequent-turn result, and sanitized log findings in the Phase
4 status section of `docs/proposals/ENGINE_LLM_STREAMING.md`.

Rollback if cancellation exceeds two seconds, deltas continue after queued
frames drain, a terminal assistant response appears, the wrong thread stops, or
the next turn remains cancelled. Roll back the tenant binary/source to the last
known-good Phase 3 commit and restart with `lunarwing-mt-admin.sh`; do not alter
`env/` or `state/`.

### Task 8: Correct The Live Ingress Scheduling Blocker

The first Brightdawn live gate failed before cancellation routing was reached.
The target thread started at `20:24:08Z`, completed normally at `20:25:39Z`, and
the queued `/interrupt` was not parsed until `20:25:58Z`. `Agent::run()` awaited
the ordinary `handle_message()` task before polling channel input again.

- [x] Add an agent-level pending-acquisition regression and observe RED: the old
  loop returned zero responses at the two-second deadline.
- [x] Add parser-based priority classification plus a bounded 256-message FIFO.
- [x] Keep one ordinary handler active while exact `/interrupt` and `/stop`
  controls use the existing `handle_message()` and outbound-hook path.
- [x] Preserve soft-timeout detachment and the independent hard-kill
  `AbortHandle`; abort and await the active task for process shutdown.
- [x] Cover FIFO priority, real-capacity overflow, unmatched scoped fallback,
  timeout suppression, and single-acknowledgement cancellation in the isolated
  `engine_v2_interrupt_ingress` target (`5 passed; 0 failed`).
- [x] Run local gates: Engine `322/322`, agent loop `17/17`, dispatcher `2/2`,
  bridge/router `30/30`, default/PostgreSQL/libSQL checks, formatting, and
  zero-warning Clippy.
- [x] Push the corrected integration branch, rebuild Brightdawn without touching
  tenant `env/`, `state/`, or installed WASM artifacts, and repeat the measured
  TensorZero `2026.3.2` live gate.

### Task 9: Complete The Corrected Brightdawn Gate

- [x] Push `83af2e6` and confirm local/origin equality on
  `integration/enginep4p5-uifix-skillsb2b3/v2.0.0.0`.
- [x] Fast-forward only `/home/brightdawn/lunarwing` source after proving the
  incoming paths did not overlap its modified nested lockfiles or untracked
  `env/` and `state/` directories.
- [x] Build `lunarwing` in release mode with `taskset -c 0-5` and `-j6`. The
  single tmux build completed in 3m17s without rebuilding or reinstalling WASM.
- [x] Restart through `ic/scripts/lunarwing-mt-admin.sh`; tenant status, gateway
  health, and agent status passed.
- [x] Calibrate the live harness to the dispatcher contract: ordinary messages
  remain globally serialized, so the second thread must remain queued until the
  first thread is interrupted. Recovery and isolation assert scoped streaming,
  completion, persistence, and absence of errors rather than exact model wording.
- [x] Pass the live gate at `2026-07-13T18:52:49-04:00` with exit status `0`:
  primary thread `93d0d61d-6a2f-4593-972a-d1f8100724f7`, isolation thread
  `67142c06-b447-4c05-9a46-bb637a798dee`, four initial chunks, 11 ms first
  acknowledgement, stable count of four after quiescence, zero persisted
  cancelled assistant responses, 14 recovery chunks, one persisted recovery,
  zero isolation chunks while queued, 10 ms second acknowledgement, and one
  terminal/persisted isolation response.
- [x] Scan the scoped journal window. It contained no panic, receiver lag,
  rollback/failure signal, or duplicate response. Tenant `env/`, `state/`,
  installed WASM artifacts, and pre-existing nested lockfile changes remained
  intact.

## Phase 4 Completion Gate

Phase 4 is complete only when every item below is true:

- [x] Active stream acquisition or collection is dropped promptly.
- [x] The outcome is exactly `ThreadOutcome::Stopped`.
- [x] No provider delta is produced after cancellation; any delta already queued
  before the request is bounded and recorded. No synthetic `Done`, cancelled-call
  usage, cache entry, trace response, assistant message, or terminal channel
  response is committed.
- [x] Existing between-step `ThreadSignal::Stop` behavior still passes.
- [x] Cancellation is isolated by owner and conversation scope.
- [x] Resume and subsequent new turns use fresh, non-cancelled tokens.
- [x] Gateway interrupt reaches Engine V2 and returns one acknowledgement.
- [x] All local checks pass under the six-thread constraint.
- [x] Brightdawn passes the live TensorZero `2026.3.2` gate without changing
  tenant `env/`, tenant `state/`, or installed WASM channels.
