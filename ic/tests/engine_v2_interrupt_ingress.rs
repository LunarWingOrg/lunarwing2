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
    CompletionRequest, CompletionResponse, FinishReason, LlmProvider, ToolCompletionRequest,
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

    async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
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

    async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
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

#[tokio::test]
async fn unmatched_scoped_interrupt_falls_back_without_stopping_the_active_engine_thread() {
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
    let active_thread_id = Uuid::new_v4();
    let unmatched_thread_id = Uuid::new_v4();

    rig.send_incoming(gateway_message(active_thread_id, "active"))
        .await;
    tokio::time::timeout(Duration::from_secs(2), pending.started.notified())
        .await
        .expect("active provider call should start");
    rig.send_incoming(gateway_message(unmatched_thread_id, "/interrupt"))
        .await;

    let responses = rig.wait_for_responses(1, Duration::from_secs(2)).await;
    assert_eq!(
        responses[0].content,
        "Error: Invalid or unauthorized thread ID."
    );
    assert_eq!(dropped.load(Ordering::SeqCst), 0);

    rig.send_incoming(gateway_message(active_thread_id, "/interrupt"))
        .await;
    let responses = rig.wait_for_responses(2, Duration::from_secs(2)).await;
    assert_eq!(responses[1].content, "Interrupted.");
    assert_eq!(dropped.load(Ordering::SeqCst), 1);

    rig.shutdown_and_wait().await;
    lunarwing::bridge::reset_engine_state().await;
}

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

    rig.send_incoming(gateway_message(thread_id, "active"))
        .await;
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

    rig.send_incoming(gateway_message(Uuid::new_v4(), "first"))
        .await;
    tokio::time::timeout(Duration::from_secs(2), provider.first_started.notified())
        .await
        .expect("first provider call should start");
    rig.send_incoming(gateway_message(Uuid::new_v4(), "second"))
        .await;

    let responses = rig.wait_for_responses(2, Duration::from_secs(6)).await;
    assert_eq!(
        responses[0].content,
        "Sorry, your request timed out. Please try again."
    );
    assert_eq!(responses[1].content, "response-1");
    assert_eq!(dropped.load(Ordering::SeqCst), 0);

    provider.release_first.notify_one();
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(rig.captured_responses().len(), 2);

    rig.shutdown_and_wait().await;
    lunarwing::bridge::reset_engine_state().await;
}
