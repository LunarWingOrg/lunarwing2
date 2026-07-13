# Engine V2 Interrupt-Aware Dispatch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make exact `/interrupt` and `/stop` controls reach the existing scoped Engine V2 cancellation route while an ordinary message handler is active, without making ordinary messages concurrent.

**Architecture:** Add a small dispatcher helper for exact interrupt classification and a bounded FIFO, then make `Agent::run()` select over its active `JoinHandle`, pinned soft-timeout `Sleep`, shutdown signals, and inbound channel messages. Priority interrupts run through the existing `handle_message()` and outbound-delivery path; every other message remains serialized in arrival order.

**Tech Stack:** Rust 2024, Tokio 1.49 (`select!`, cancel-safe `&mut JoinHandle`, pinned `Sleep`, `AbortHandle`), `VecDeque`, existing `SubmissionParser`, Engine V2 bridge cancellation, libSQL integration test harness.

---

## Fixed Decisions

- Keep exactly one ordinary `handle_message()` task active.
- Only `Submission::Interrupt` is priority. This includes exact `/interrupt` and `/stop`; it excludes text that merely contains those words.
- Cap deferred ordinary input at 256 messages. Overflow receives exactly:
  `Agent is busy and its deferred message queue is full. Try again after the active turn finishes.`
- Run priority controls through `handle_message()`, `BeforeOutbound`, empty suppression, and `ChannelManager::respond()`; do not call the bridge directly.
- Preserve the current soft timeout by dropping the `JoinHandle` (which detaches the task) while retaining its independent `AbortHandle` for the 30-second hard-kill grace.
- Abort the active task on process shutdown. Do not leave an unobserved handler running while channels are being shut down.
- Work in the user-designated current branch. Do not create a second worktree.

## File Map

- Create `ic/src/agent/dispatch.rs`: exact priority classification, bounded FIFO, overflow text, and focused unit tests.
- Modify `ic/src/agent/mod.rs`: register the private dispatcher module.
- Modify `ic/src/agent/agent_loop.rs`: prepare messages, centralize outbound result delivery, wait interrupt-aware for active handlers, and preserve timeout/shutdown behavior.
- Modify `ic/tests/support/test_rig.rs`: let integration tests name the test channel and inspect captured responses.
- Create `ic/tests/engine_v2_interrupt_ingress.rs`: real `Agent::run()` regression with a pending LLM acquisition future.
- Modify `ic/Cargo.toml`: register the isolated integration target behind `libsql,integration`.
- Modify `docs/proposals/ENGINE_LLM_STREAMING.md`: record local and live gate results only after verification.
- Modify `docs/superpowers/plans/2026-07-13-engine-llm-streaming-phase-4.md`: append the failed-gate diagnosis and completed corrective task.

### Task 1: Add The Real Failing Ingress Regression

**Files:**
- Modify: `ic/tests/support/test_rig.rs`
- Create: `ic/tests/engine_v2_interrupt_ingress.rs`
- Modify: `ic/Cargo.toml`

- [x] **Step 1: Extend `TestRig` without changing existing callers**

Add `channel_name: String` to `TestRigBuilder`, default it to `"test"`, add:

```rust
pub fn with_channel_name(mut self, channel_name: impl Into<String>) -> Self {
    self.channel_name = channel_name.into();
    self
}
```

Destructure `channel_name` in `build()` and replace the current conditional channel construction with:

```rust
let channel_name = if keep_bootstrap {
    "gateway".to_string()
} else {
    channel_name
};
let test_channel = Arc::new(TestChannel::new().with_name(channel_name));
```

Add this `TestRig` accessor:

```rust
pub fn captured_responses(&self) -> Vec<OutgoingResponse> {
    self.channel.captured_responses()
}
```

- [x] **Step 2: Register the isolated integration target**

Add after the existing `e2e_thread_scheduling` target in `ic/Cargo.toml`:

```toml
[[test]]
name = "engine_v2_interrupt_ingress"
required-features = ["libsql", "integration"]
```

- [x] **Step 3: Create a pending-provider regression test**

Create `ic/tests/engine_v2_interrupt_ingress.rs` with one serialized Tokio test. Use this provider shape so cancellation must drop an in-flight acquisition future:

```rust
mod support;

use std::future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use rust_decimal::Decimal;
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;

use lunarwing::channels::IncomingMessage;
use lunarwing::error::LlmError;
use lunarwing::llm::{
    CompletionRequest, CompletionResponse, LlmProvider, ToolCompletionRequest,
    ToolCompletionResponse,
};

use support::test_rig::TestRigBuilder;

static ENGINE_V2_ENV_LOCK: Mutex<()> = Mutex::const_new(());

struct DropProbe(Arc<AtomicUsize>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct PendingLlm {
    started: Notify,
    dropped: Arc<AtomicUsize>,
}

impl PendingLlm {
    fn new(dropped: Arc<AtomicUsize>) -> Self {
        Self {
            started: Notify::new(),
            dropped,
        }
    }

    async fn wait_forever<T>(&self) -> T {
        let _probe = DropProbe(Arc::clone(&self.dropped));
        self.started.notify_one();
        future::pending::<T>().await
    }
}

#[async_trait]
impl LlmProvider for PendingLlm {
    fn model_name(&self) -> &str {
        "pending-test"
    }

    fn cost_per_token(&self) -> (Decimal, Decimal) {
        (Decimal::ZERO, Decimal::ZERO)
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, LlmError> {
        self.wait_forever().await
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        self.wait_forever().await
    }
}

struct EngineV2EnvGuard {
    original: Option<String>,
}

impl EngineV2EnvGuard {
    fn enable() -> Self {
        let original = std::env::var("ENGINE_V2").ok();
        // SAFETY: every test in this dedicated binary holds ENGINE_V2_ENV_LOCK
        // until its background agent is stopped.
        unsafe { std::env::set_var("ENGINE_V2", "true") };
        Self { original }
    }
}

impl Drop for EngineV2EnvGuard {
    fn drop(&mut self) {
        // SAFETY: the caller still holds ENGINE_V2_ENV_LOCK and has stopped its
        // background agent before this guard is dropped.
        unsafe {
            match &self.original {
                Some(value) => std::env::set_var("ENGINE_V2", value),
                None => std::env::remove_var("ENGINE_V2"),
            }
        }
    }
}

fn gateway_message(thread_id: Uuid, content: &str) -> IncomingMessage {
    IncomingMessage::new("gateway", "test-user", content)
        .with_thread(thread_id.to_string())
        .with_metadata(serde_json::json!({
            "thread_id": thread_id,
            "user_id": "test-user",
        }))
}

#[tokio::test]
async fn gateway_interrupt_is_dispatched_while_llm_acquisition_is_pending() {
    let _env_lock = ENGINE_V2_ENV_LOCK.lock().await;
    let _env = EngineV2EnvGuard::enable();
    lunarwing::bridge::reset_engine_state().await;

    let dropped = Arc::new(AtomicUsize::new(0));
    let pending = Arc::new(PendingLlm::new(Arc::clone(&dropped)));
    let llm: Arc<dyn LlmProvider> = pending.clone();
    let rig = TestRigBuilder::new()
        .with_channel_name("gateway")
        .with_llm(llm)
        .build()
        .await;
    let thread_id = Uuid::new_v4();

    rig.send_incoming(gateway_message(thread_id, "produce a long response"))
        .await;
    tokio::time::timeout(Duration::from_secs(2), pending.started.notified())
        .await
        .expect("LLM acquisition should start");

    rig.send_incoming(gateway_message(thread_id, "/interrupt"))
        .await;
    let responses = rig.wait_for_responses(1, Duration::from_secs(2)).await;

    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].content, "Interrupted.");
    tokio::time::timeout(Duration::from_secs(1), async {
        while dropped.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("interrupt should drop the pending LLM future");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.captured_responses().len(), 1);

    rig.shutdown_and_wait().await;
    lunarwing::bridge::reset_engine_state().await;
}
```

- [x] **Step 4: Run the regression and confirm RED**

From `ic/`:

```bash
taskset -c 0-5 cargo test -j6 --no-default-features \
  --features libsql,integration --test engine_v2_interrupt_ingress \
  -- --exact gateway_interrupt_is_dispatched_while_llm_acquisition_is_pending \
  --nocapture --test-threads=1
```

Expected: FAIL after two seconds because the old run loop does not dequeue the interrupt; `responses.len()` is `0` and the pending LLM future has not been dropped.

- [x] **Step 5: Commit the RED regression**

```bash
git add ic/Cargo.toml ic/tests/support/test_rig.rs \
  ic/tests/engine_v2_interrupt_ingress.rs
git commit -m "test(agent): reproduce blocked streaming interrupt"
```

### Task 2: Add Exact Classification And Bounded FIFO

**Files:**
- Create: `ic/src/agent/dispatch.rs`
- Modify: `ic/src/agent/mod.rs`

- [x] **Step 1: Write pure dispatcher tests**

Create tests in `dispatch.rs` first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn message(content: &str) -> IncomingMessage {
        IncomingMessage::new("gateway", "user", content)
    }

    #[test]
    fn only_exact_interrupt_submissions_are_priority() {
        assert!(is_priority_interrupt(&message("/interrupt")));
        assert!(is_priority_interrupt(&message("/stop")));
        assert!(!is_priority_interrupt(&message("please interrupt this")));
        assert!(!is_priority_interrupt(&message("/interrupt later")));
        assert!(!is_priority_interrupt(&message("/clear")));
    }

    #[test]
    fn deferred_messages_are_fifo_and_bounded() {
        let mut queue = DeferredMessages::with_capacity_for_test(2);
        assert!(queue.defer(message("first")).is_none());
        assert!(queue.defer(message("second")).is_none());
        let rejected = queue.defer(message("third")).expect("queue is full");
        assert_eq!(rejected.content, "third");
        assert_eq!(queue.pop_front().map(|m| m.content), Some("first".into()));
        assert_eq!(queue.pop_front().map(|m| m.content), Some("second".into()));
    }
}
```

- [x] **Step 2: Run the unit tests and confirm RED**

```bash
taskset -c 0-5 cargo test -j6 --lib agent::dispatch::tests -- --nocapture
```

Expected: compilation fails because the module and helpers do not exist.

- [x] **Step 3: Implement the minimal dispatcher helper**

Create `ic/src/agent/dispatch.rs`:

```rust
use std::collections::VecDeque;

use crate::agent::submission::{Submission, SubmissionParser};
use crate::channels::IncomingMessage;

pub(super) const DEFERRED_MESSAGE_LIMIT: usize = 256;
pub(super) const DEFERRED_QUEUE_FULL_RESPONSE: &str =
    "Agent is busy and its deferred message queue is full. Try again after the active turn finishes.";

pub(super) fn is_priority_interrupt(message: &IncomingMessage) -> bool {
    matches!(SubmissionParser::parse(&message.content), Submission::Interrupt)
}

pub(super) struct DeferredMessages {
    queue: VecDeque<IncomingMessage>,
    capacity: usize,
}

impl DeferredMessages {
    pub(super) fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            capacity: DEFERRED_MESSAGE_LIMIT,
        }
    }

    #[cfg(test)]
    fn with_capacity_for_test(capacity: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            capacity,
        }
    }

    pub(super) fn defer(&mut self, message: IncomingMessage) -> Option<IncomingMessage> {
        if self.queue.len() >= self.capacity {
            return Some(message);
        }
        self.queue.push_back(message);
        None
    }

    pub(super) fn pop_front(&mut self) -> Option<IncomingMessage> {
        self.queue.pop_front()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}
```

Add `mod dispatch;` to `ic/src/agent/mod.rs`.

- [x] **Step 4: Run the unit tests and commit**

```bash
taskset -c 0-5 cargo test -j6 --lib agent::dispatch::tests -- --nocapture
git add ic/src/agent/dispatch.rs ic/src/agent/mod.rs
git commit -m "feat(agent): classify priority interrupts"
```

Expected: both dispatcher tests pass.

### Task 3: Make The Active-Message Wait Interrupt-Aware

**Files:**
- Modify: `ic/src/agent/agent_loop.rs`

- [x] **Step 1: Extract common outbound result delivery**

Move the existing normal-completion branches into:

```rust
enum MessageLoopControl {
    Continue,
    Shutdown,
}

async fn deliver_message_result(
    &self,
    message: &IncomingMessage,
    result: Result<Option<String>, Error>,
) -> MessageLoopControl
```

The helper must preserve the existing four outcomes exactly:

```rust
match result {
    Ok(Some(response)) if !response.is_empty() => {
        self.deliver_outbound_with_hooks(message, response).await;
        MessageLoopControl::Continue
    }
    Ok(Some(empty)) => {
        tracing::debug!(
            channel = %message.channel,
            user = %message.user_id,
            empty_len = empty.len(),
            "Suppressed empty response (not sent to channel)"
        );
        MessageLoopControl::Continue
    }
    Ok(None) => MessageLoopControl::Shutdown,
    Err(error) => {
        tracing::error!("Error handling message: {error}");
        if let Err(send_error) = self
            .channels
            .respond(message, OutgoingResponse::text(format!("Error: {error}")))
            .await
        {
            tracing::error!(
                channel = %message.channel,
                error = %send_error,
                "Failed to send error response to channel"
            );
        }
        MessageLoopControl::Continue
    }
}
```

Extract the current `BeforeOutbound` block without changing its behavior:

```rust
async fn deliver_outbound_with_hooks(&self, message: &IncomingMessage, response: String)
```

- [x] **Step 2: Add message preparation and priority execution helpers**

Extract the current transcription/document/indexing block into:

```rust
async fn prepare_message(&self, mut message: IncomingMessage) -> IncomingMessage {
    if let Some(ref transcription) = self.deps.transcription {
        transcription.process(&mut message).await;
    }
    if let Some(ref document_extraction) = self.deps.document_extraction {
        document_extraction.process(&mut message).await;
    }
    self.store_extracted_documents(&message).await;
    message
}
```

Add:

```rust
async fn handle_priority_interrupt(&self, message: IncomingMessage) {
    let suppressed = AtomicBool::new(false);
    let result = self.handle_message(&message, &suppressed).await;
    let _ = self.deliver_message_result(&message, result).await;
}
```

- [x] **Step 3: Replace the blocking timeout with an interrupt-aware select loop**

At the start of the main message loop, create:

```rust
let mut deferred_messages = crate::agent::dispatch::DeferredMessages::new();
let mut channels_open = true;
```

Acquire the next ordinary message from `deferred_messages` first, otherwise from
`message_stream`. After spawning its `JoinHandle`, pin one deadline:

```rust
let deadline = tokio::time::sleep(self.config.handle_message_timeout);
tokio::pin!(deadline);
let abort_handle = handle.abort_handle();
```

Then select repeatedly:

```rust
enum ActiveMessageResult {
    Completed(Result<Result<Option<String>, Error>, tokio::task::JoinError>),
    TimedOut,
    Shutdown(ShutdownSignal),
}

enum ShutdownSignal {
    CtrlC,
    Sigterm,
}

let active_result = loop {
    tokio::select! {
        result = &mut handle => {
            break ActiveMessageResult::Completed(result);
        }
        _ = &mut deadline => {
            break ActiveMessageResult::TimedOut;
        }
        incoming = message_stream.next(), if channels_open => {
            match incoming {
                Some(incoming) if crate::agent::dispatch::is_priority_interrupt(&incoming) => {
                    self.handle_priority_interrupt(incoming).await;
                }
                Some(incoming) => {
                    if let Some(rejected) = deferred_messages.defer(incoming) {
                        let result = Ok(Some(
                            crate::agent::dispatch::DEFERRED_QUEUE_FULL_RESPONSE.to_string(),
                        ));
                        let _ = self.deliver_message_result(&rejected, result).await;
                    }
                }
                None => channels_open = false,
            }
        }
        _ = tokio::signal::ctrl_c() => {
            break ActiveMessageResult::Shutdown(ShutdownSignal::CtrlC);
        }
        _ = recv_sigterm(&mut sigterm) => {
            break ActiveMessageResult::Shutdown(ShutdownSignal::Sigterm);
        }
    }
};
```

Use a `'message_loop` label. Keep the old soft-timeout branch unchanged except
that it now handles `ActiveMessageResult::TimedOut` and retains `abort_handle` for
the delayed hard kill. For `Completed`, keep the existing panic reset, then pass
the inner `Result<Option<String>, Error>` to `deliver_message_result()`. For
`Shutdown`, abort and await the active task before leaving `'message_loop`:

```rust
ActiveMessageResult::Shutdown(signal) => {
    handle.abort();
    let _ = (&mut handle).await;
    match signal {
        ShutdownSignal::CtrlC => tracing::info!("Ctrl+C received, shutting down..."),
        ShutdownSignal::Sigterm => {
            tracing::warn!("SIGTERM received, shutting down gracefully...")
        }
    }
    break 'message_loop;
}
```

Add a small `recv_sigterm` helper under `#[cfg(unix)]` plus a non-Unix pending
implementation so Ctrl+C/SIGTERM remain selectable during active work.

- [x] **Step 4: Run the real regression and confirm GREEN**

```bash
taskset -c 0-5 cargo test -j6 --no-default-features \
  --features libsql,integration --test engine_v2_interrupt_ingress \
  -- --exact gateway_interrupt_is_dispatched_while_llm_acquisition_is_pending \
  --nocapture --test-threads=1
```

Expected: PASS; one `Interrupted.` response arrives, the acquisition future is
dropped, and no cancelled terminal response follows.

- [x] **Step 5: Run existing agent/bridge cancellation tests**

```bash
taskset -c 0-5 cargo test -j6 --lib agent::agent_loop::tests -- --nocapture
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests -- --nocapture
taskset -c 0-5 cargo test -j6 -p lunarwing_engine runtime::manager::tests \
  -- --nocapture
```

Expected: all pass with no duplicate response, scope, or cancellation regression.

- [x] **Step 6: Commit the dispatcher integration**

```bash
git add ic/src/agent/agent_loop.rs
git commit -m "fix(agent): dispatch interrupts during active turns"
```

### Task 4: Lock Down Queue, Timeout, And Shutdown Semantics

**Files:**
- Modify: `ic/src/agent/dispatch.rs`
- Modify: `ic/src/agent/agent_loop.rs`
- Modify: `ic/tests/support/test_rig.rs`
- Modify: `ic/tests/engine_v2_interrupt_ingress.rs`

- [x] **Step 1: Complete exact command-classification coverage**

Add these assertions to `only_exact_interrupt_submissions_are_priority` before
changing production code:

```rust
assert!(is_priority_interrupt(&message(" /STOP ")));
assert!(!is_priority_interrupt(&message("please /stop after this")));
assert!(!is_priority_interrupt(&message("/stop now")));
```

This locks classification to `SubmissionParser` rather than a substring or
prefix check.

- [x] **Step 2: Add a controllable provider for lifecycle tests**

In `ic/tests/engine_v2_interrupt_ingress.rs`, import `FinishReason` and add this
provider below `PendingLlm`:

```rust
struct FirstPendingThenReplyLlm {
    calls: AtomicUsize,
    first_started: Notify,
    later_started: Notify,
    release_first: Notify,
    first_dropped: Arc<AtomicUsize>,
}

impl FirstPendingThenReplyLlm {
    fn new(first_dropped: Arc<AtomicUsize>) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            first_started: Notify::new(),
            later_started: Notify::new(),
            release_first: Notify::new(),
            first_dropped,
        }
    }

    async fn next_content(&self) -> String {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            let _probe = DropProbe(Arc::clone(&self.first_dropped));
            self.first_started.notify_one();
            self.release_first.notified().await;
            "late-first-response".to_string()
        } else {
            self.later_started.notify_one();
            format!("response-{call}")
        }
    }
}

#[async_trait]
impl LlmProvider for FirstPendingThenReplyLlm {
    fn model_name(&self) -> &str {
        "first-pending-then-reply"
    }

    fn cost_per_token(&self) -> (Decimal, Decimal) {
        (Decimal::ZERO, Decimal::ZERO)
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, LlmError> {
        Ok(CompletionResponse {
            content: self.next_content().await,
            input_tokens: 1,
            output_tokens: 1,
            finish_reason: FinishReason::Stop,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        })
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        Ok(ToolCompletionResponse {
            content: Some(self.next_content().await),
            tool_calls: Vec::new(),
            input_tokens: 1,
            output_tokens: 1,
            finish_reason: FinishReason::Stop,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        })
    }
}
```

- [x] **Step 3: Prove an interrupt overtakes FIFO without making ordinary input concurrent**

Add this integration test. It observes the response snapshot at the instant the
second provider call starts, so a scheduler that starts `second` before sending
the acknowledgement fails deterministically:

```rust
#[tokio::test]
async fn queued_ordinary_message_starts_after_interrupt_acknowledgement() {
    let _env_lock = ENGINE_V2_ENV_LOCK.lock().await;
    let _env = EngineV2EnvGuard::enable();
    lunarwing::bridge::reset_engine_state().await;

    let dropped = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(FirstPendingThenReplyLlm::new(Arc::clone(&dropped)));
    let llm: Arc<dyn LlmProvider> = provider.clone();
    let rig = TestRigBuilder::new()
        .with_channel_name("gateway")
        .with_llm(llm)
        .build()
        .await;
    let active_thread_id = Uuid::new_v4();
    let deferred_thread_id = Uuid::new_v4();

    rig.send_incoming(gateway_message(active_thread_id, "first"))
        .await;
    tokio::time::timeout(Duration::from_secs(2), provider.first_started.notified())
        .await
        .expect("first provider call should start");
    rig.send_incoming(gateway_message(deferred_thread_id, "second"))
        .await;
    rig.send_incoming(gateway_message(active_thread_id, "/interrupt"))
        .await;

    tokio::time::timeout(Duration::from_secs(2), provider.later_started.notified())
        .await
        .expect("deferred ordinary message should start after cancellation");
    let at_second_start = rig.captured_responses();
    assert_eq!(
        at_second_start
            .iter()
            .map(|response| response.content.as_str())
            .collect::<Vec<_>>(),
        vec!["Interrupted."]
    );

    let responses = rig.wait_for_responses(2, Duration::from_secs(2)).await;
    assert_eq!(responses[1].content, "response-1");
    assert_eq!(dropped.load(Ordering::SeqCst), 1);

    rig.shutdown_and_wait().await;
    lunarwing::bridge::reset_engine_state().await;
}
```

- [x] **Step 4: Exercise overflow at the real 256-message limit**

Do not add a test-only production capacity. Add this integration test using the
real configured bound:

```rust
#[tokio::test]
async fn deferred_queue_overflow_gets_one_explicit_busy_response() {
    let _env_lock = ENGINE_V2_ENV_LOCK.lock().await;
    let _env = EngineV2EnvGuard::enable();
    lunarwing::bridge::reset_engine_state().await;

    let dropped = Arc::new(AtomicUsize::new(0));
    let pending = Arc::new(PendingLlm::new(Arc::clone(&dropped)));
    let llm: Arc<dyn LlmProvider> = pending.clone();
    let rig = TestRigBuilder::new()
        .with_channel_name("gateway")
        .with_llm(llm)
        .build()
        .await;
    let thread_id = Uuid::new_v4();

    rig.send_incoming(gateway_message(thread_id, "active")).await;
    tokio::time::timeout(Duration::from_secs(2), pending.started.notified())
        .await
        .expect("active provider call should start");
    tokio::time::timeout(Duration::from_secs(2), async {
        for _ in 0..=256 {
            rig.send_incoming(gateway_message(thread_id, "/clear"))
                .await;
        }
    })
    .await
    .expect("dispatcher should keep draining channel input while the turn is active");

    let responses = rig.wait_for_responses(1, Duration::from_secs(2)).await;
    assert_eq!(responses.len(), 1);
    assert_eq!(
        responses[0].content,
        "Agent is busy and its deferred message queue is full. Try again after the active turn finishes."
    );

    rig.send_incoming(gateway_message(thread_id, "/interrupt"))
        .await;
    let responses = rig.wait_for_responses(2, Duration::from_secs(2)).await;
    assert_eq!(responses[1].content, "Interrupted.");

    let responses = rig.wait_for_responses(258, Duration::from_secs(10)).await;
    assert_eq!(responses.len(), 258);

    rig.shutdown_and_wait().await;
    lunarwing::bridge::reset_engine_state().await;
}
```

- [x] **Step 5: Preserve the soft-timeout detach contract**

Add `handle_message_timeout: Option<Duration>` to `TestRigBuilder`, default it
to `None`, destructure it in `build()`, and add:

```rust
pub fn with_handle_message_timeout(mut self, timeout: Duration) -> Self {
    self.handle_message_timeout = Some(timeout);
    self
}
```

Add an awaited teardown alongside the existing synchronous test helper:

```rust
pub async fn shutdown_and_wait(mut self) {
    self.channel.signal_shutdown();
    if let Some(handle) = self.agent_handle.take() {
        handle.abort();
        let _ = handle.await;
    }
}
```

After `AppBuilder::build_all()`, force the test override alongside the existing
deterministic agent flags:

```rust
if let Some(timeout) = handle_message_timeout {
    components.config.agent.handle_message_timeout = timeout;
}
```

Then add this characterization test. Distinct scopes ensure the second Engine V2
turn can run while the timed-out first handler remains detached:

```rust
#[tokio::test]
async fn soft_timeout_detaches_and_suppresses_the_original_handler() {
    let _env_lock = ENGINE_V2_ENV_LOCK.lock().await;
    let _env = EngineV2EnvGuard::enable();
    lunarwing::bridge::reset_engine_state().await;

    let dropped = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(FirstPendingThenReplyLlm::new(Arc::clone(&dropped)));
    let llm: Arc<dyn LlmProvider> = provider.clone();
    let rig = TestRigBuilder::new()
        .with_channel_name("gateway")
        .with_handle_message_timeout(Duration::from_secs(2))
        .with_llm(llm)
        .build()
        .await;

    rig.send_incoming(gateway_message(Uuid::new_v4(), "first")).await;
    tokio::time::timeout(Duration::from_secs(2), provider.first_started.notified())
        .await
        .expect("first provider call should start");
    rig.send_incoming(gateway_message(Uuid::new_v4(), "second"))
        .await;

    let responses = rig.wait_for_responses(2, Duration::from_secs(6)).await;
    assert_eq!(responses[0].content, "Sorry, your request timed out. Please try again.");
    assert_eq!(responses[1].content, "response-1");
    assert_eq!(dropped.load(Ordering::SeqCst), 0);

    provider.release_first.notify_one();
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(rig.captured_responses().len(), 2);

    rig.shutdown_and_wait().await;
    lunarwing::bridge::reset_engine_state().await;
}
```

- [x] **Step 6: Prove active shutdown waits for task cancellation**

Extract the shutdown operation used by both signal branches:

```rust
async fn abort_and_wait<T>(handle: &mut tokio::task::JoinHandle<T>) {
    handle.abort();
    let _ = handle.await;
}
```

Add this unit test in `agent_loop.rs` before using the helper in production:

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

#[tokio::test]
async fn abort_and_wait_drops_the_active_handler_before_returning() {
    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    let dropped = Arc::new(AtomicBool::new(false));
    let task_dropped = Arc::clone(&dropped);
    let started = Arc::new(Notify::new());
    let task_started = Arc::clone(&started);
    let mut handle = tokio::spawn(async move {
        let _flag = DropFlag(task_dropped);
        task_started.notify_one();
        std::future::pending::<()>().await;
    });
    started.notified().await;

    abort_and_wait(&mut handle).await;

    assert!(handle.is_finished());
    assert!(dropped.load(Ordering::SeqCst));
}
```

- [x] **Step 7: Run the new cases and confirm the intended RED/characterization split**

```bash
taskset -c 0-5 cargo test -j6 --lib agent::dispatch::tests -- --nocapture
taskset -c 0-5 cargo test -j6 --no-default-features \
  --features libsql,integration --test engine_v2_interrupt_ingress \
  -- --test-threads=1 --nocapture
```

Expected before the lifecycle implementation is complete: the classification
test passes, the current run loop fails the interrupt/FIFO and overflow cases,
and the existing soft-timeout behavior passes. The active-shutdown unit test
fails to compile until `abort_and_wait` is introduced.

- [x] **Step 8: Make only the minimal lifecycle adjustments required by RED**

Keep queue capacity, FIFO, timeout detachment, hard-kill abort, and process
shutdown in their owning helpers. Do not add general message concurrency or
priority for any other submission.

- [x] **Step 9: Run, format, and commit lifecycle coverage**

```bash
taskset -c 0-5 cargo test -j6 --lib agent::dispatch::tests -- --nocapture
taskset -c 0-5 cargo test -j6 --no-default-features \
  --features libsql,integration --test engine_v2_interrupt_ingress \
  -- --test-threads=1 --nocapture
taskset -c 0-5 cargo fmt --all -- --check
git add ic/src/agent/dispatch.rs ic/src/agent/agent_loop.rs \
  ic/tests/support/test_rig.rs ic/tests/engine_v2_interrupt_ingress.rs
git commit -m "test(agent): cover interrupt dispatcher lifecycle"
```

### Task 5: Local Verification And Documentation

**Files:**
- Modify: `docs/proposals/ENGINE_LLM_STREAMING.md`
- Modify: `docs/superpowers/plans/2026-07-13-engine-llm-streaming-phase-4.md`
- Check: `ic/FEATURE_PARITY.md`
- Check: `docs/architecture/ENGINE-V2.md`
- Check: `CHANGELOG.md`

- [x] **Step 1: Run the scoped verification matrix**

From `ic/`, one Cargo process at a time:

```bash
taskset -c 0-5 cargo fmt --all -- --check
taskset -c 0-5 cargo check -j6
taskset -c 0-5 cargo check -j6 --no-default-features --features postgres
taskset -c 0-5 cargo check -j6 --no-default-features --features libsql
taskset -c 0-5 cargo test -j6 -p lunarwing_engine -- --test-threads=6
taskset -c 0-5 cargo test -j6 --lib agent::agent_loop::tests -- --test-threads=6
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests -- --test-threads=6
taskset -c 0-5 cargo test -j6 --no-default-features \
  --features libsql,integration --test engine_v2_interrupt_ingress \
  -- --test-threads=1
taskset -c 0-5 cargo clippy -j6 --all --benches --tests --examples -- -D warnings
```

From the repository root:

```bash
git diff --check
rg -n 'systemctl is-active|systemctl status|rc-service' \
  lunarwing_mt_onboard lunarwing_mt_onboard_web
```

- [x] **Step 2: Update status with verified facts only**

Record the RED/green integration test, exact local commands, and the fact that
Phase 4 remains live-pending until Brightdawn passes. Append a corrective task to
the Phase 4 plan rather than rewriting its historical completed tasks. Update
`FEATURE_PARITY.md`, architecture docs, or changelog only if their current text
would otherwise make a false behavior claim.

- [x] **Step 3: Commit local verification documentation**

```bash
git add docs/proposals/ENGINE_LLM_STREAMING.md \
  docs/superpowers/plans/2026-07-13-engine-llm-streaming-phase-4.md \
  ic/FEATURE_PARITY.md docs/architecture/ENGINE-V2.md CHANGELOG.md
git commit -m "docs: record interrupt dispatcher verification"
```

Stage only files actually changed; do not create an empty commit.

### Task 6: Rebuild Brightdawn And Repeat The Live Gate

**Files:**
- Preserve: `/home/brightdawn/lunarwing/env/`
- Preserve: `/home/brightdawn/lunarwing/state/`
- Preserve installed WASM artifacts.

- [ ] **Step 1: Push and update only Brightdawn source**

Require local/origin HEAD equality, then fetch and fast-forward the current
integration branch through the Brightdawn account. Preserve its pre-existing
modified nested lockfiles and untracked `env/`/`state/`; abort if the new commit
overlaps those changes.

- [ ] **Step 2: Build the release binary in one tmux session**

```bash
tmux new-session -d -s phase4-dispatch-brightdawn-build \
  "sudo -n -u brightdawn env HOME=/home/brightdawn bash -o pipefail -c \
  'cd /home/brightdawn/lunarwing/ic && taskset -c 0-5 \
  cargo build --release -j6 --bin lunarwing 2>&1 | \
  tee /tmp/phase4-dispatch-brightdawn-build.log'"
```

Require pane exit status `0`. Do not build or reinstall WASM.

- [ ] **Step 3: Restart through the lifecycle owner**

```bash
sudo -n ic/scripts/lunarwing-mt-admin.sh restart-tenant brightdawn
sudo -n ic/scripts/lunarwing-mt-admin.sh status brightdawn
curl -fsS http://127.0.0.1:10010/api/health
curl -fsS http://127.0.0.1:10011/agent/status
```

- [ ] **Step 4: Run the sanitized live cancellation harness**

Use `/tmp/brightdawn_phase4_live_gate.sh` after updating it to retain sanitized
failure diagnostics and pipeline exit status. Require:

- two or more real TensorZero chunks before interrupt;
- one `Interrupted.` acknowledgement in at most two seconds;
- stable chunk count for one second after queued frames drain;
- no cancelled terminal response or persisted assistant response;
- same-thread recovery streams and completes;
- a concurrent second thread completes after the first is interrupted;
- sanitized journals contain no panic, event lag, rollback/failure increment, or
  duplicate response.

- [ ] **Step 5: Record the passing live evidence and commit**

Update the proposal and Phase 4 plan with sanitized timestamps, thread IDs,
event counts, acknowledgement latency, history count, recovery/isolation result,
and log scan. Mark Phase 4 complete only if every gate passes.

```bash
git add docs/proposals/ENGINE_LLM_STREAMING.md \
  docs/superpowers/plans/2026-07-13-engine-llm-streaming-phase-4.md
git commit -m "docs: complete engine streaming phase 4"
```
