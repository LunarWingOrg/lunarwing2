mod support;

use std::future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use rust_decimal::Decimal;
use tokio::sync::Notify;
use uuid::Uuid;

use lunarwing::channels::IncomingMessage;
use lunarwing::error::LlmError;
use lunarwing::llm::{
    CompletionRequest, CompletionResponse, LlmProvider, ToolCompletionRequest,
    ToolCompletionResponse,
};

use support::test_rig::TestRigBuilder;

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
        // SAFETY: this dedicated integration binary contains one test, so no
        // concurrent code reads or mutates ENGINE_V2.
        unsafe { std::env::set_var("ENGINE_V2", "true") };
        Self { original }
    }
}

impl Drop for EngineV2EnvGuard {
    fn drop(&mut self) {
        // SAFETY: this dedicated integration binary contains one test.
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

    rig.shutdown();
    lunarwing::bridge::reset_engine_state().await;
}
