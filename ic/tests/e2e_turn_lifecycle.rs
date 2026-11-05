//! E2E tests: message lifecycle reported by the agent loop (TUI design 8.2.2).
//!
//! Every inbound message reports `Started` when the agent dequeues it and exactly one
//! `Finished(outcome)` after any reply was handed to `respond()`. Priority controls
//! and messages that never run (cancelled by an interrupt) may finish without
//! `Started`. These tests drive the V1 agent loop (`ENGINE_V2` unset, as in CI)
//! through a `TestRig` and read `TestChannel::captured_lifecycle()`.

#[cfg(feature = "libsql")]
mod support;

#[cfg(feature = "libsql")]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use rust_decimal::Decimal;
    use tokio::sync::Notify;
    use uuid::Uuid;

    use lunarwing::channels::{IncomingMessage, MessageLifecycle, StatusUpdate, TurnOutcome};
    use lunarwing::error::LlmError;
    use lunarwing::hooks::{Hook, HookContext, HookError, HookEvent, HookOutcome, HookPoint};
    use lunarwing::llm::{
        CompletionRequest, CompletionResponse, FinishReason, LlmProvider, ToolCompletionRequest,
        ToolCompletionResponse,
    };

    use crate::support::test_channel::CapturedDelivery;
    use crate::support::test_rig::{TestRig, TestRigBuilder};
    use crate::support::trace_llm::{LlmTrace, TraceResponse, TraceStep, TraceToolCall};

    const TIMEOUT: Duration = Duration::from_secs(15);
    /// Time allowed for a duplicate `Finished` to show up before a test asserts there is none.
    const SETTLE: Duration = Duration::from_millis(300);
    /// Name and user of the rig's default capture channel.
    const CHANNEL: &str = "test";
    const USER: &str = "test-user";

    const STARTED: MessageLifecycle = MessageLifecycle::Started;

    fn finished(outcome: TurnOutcome) -> MessageLifecycle {
        MessageLifecycle::Finished(outcome)
    }

    fn fixture(name: &str) -> LlmTrace {
        let path = format!(
            "{}/tests/fixtures/llm_traces/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        LlmTrace::from_file(&path).unwrap_or_else(|e| panic!("failed to load {path}: {e}"))
    }

    fn text_step(content: &str) -> TraceStep {
        TraceStep {
            request_hint: None,
            response: TraceResponse::Text {
                content: content.to_string(),
                input_tokens: 10,
                output_tokens: 5,
            },
            expected_tool_results: Vec::new(),
        }
    }

    /// Injects `content` from the default test user and returns the message id.
    async fn send(rig: &TestRig, content: &str) -> Uuid {
        send_message(rig, IncomingMessage::new(CHANNEL, USER, content)).await
    }

    async fn send_message(rig: &TestRig, message: IncomingMessage) -> Uuid {
        let id = message.id;
        rig.send_incoming(message).await;
        id
    }

    /// Lifecycle events of one message, in arrival order.
    fn events_for(rig: &TestRig, id: Uuid) -> Vec<MessageLifecycle> {
        rig.captured_lifecycle()
            .into_iter()
            .filter(|(message_id, _)| *message_id == id)
            .map(|(_, event)| event)
            .collect()
    }

    /// Waits until message `id` reported `Finished`, then returns its events.
    async fn wait_for_finish(rig: &TestRig, id: Uuid) -> Vec<MessageLifecycle> {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            let events = events_for(rig, id);
            let done = events
                .iter()
                .any(|event| matches!(event, MessageLifecycle::Finished(_)));
            if done || tokio::time::Instant::now() >= deadline {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Position of message `id`'s first event matching `want` among all lifecycle records,
    /// with the number of deliveries the channel had seen at that moment.
    fn record_position(
        rig: &TestRig,
        id: Uuid,
        want: fn(&MessageLifecycle) -> bool,
    ) -> (usize, usize) {
        rig.captured_lifecycle_records()
            .iter()
            .enumerate()
            .find(|(_, record)| record.message_id == id && want(&record.event))
            .map(|(index, record)| (index, record.deliveries_before))
            .unwrap_or_else(|| panic!("no matching lifecycle event for {id}"))
    }

    fn is_started(event: &MessageLifecycle) -> bool {
        *event == MessageLifecycle::Started
    }

    fn is_finished(event: &MessageLifecycle) -> bool {
        matches!(event, MessageLifecycle::Finished(_))
    }

    /// Index among `captured_deliveries()` of the first reply to message `id`.
    fn reply_index(rig: &TestRig, id: Uuid) -> usize {
        rig.captured_deliveries()
            .iter()
            .position(|delivery| {
                matches!(delivery, CapturedDelivery::Response { message, .. } if message.id == id)
            })
            .unwrap_or_else(|| panic!("no reply delivered for {id}"))
    }

    #[tokio::test]
    async fn lifecycle_started_then_finished_replied_for_simple_turn() {
        let rig = TestRigBuilder::new()
            .with_trace(fixture("simple_text.json"))
            .build()
            .await;

        let id = send(&rig, "hello").await;
        let responses = rig.wait_for_responses(1, TIMEOUT).await;
        assert_eq!(responses.len(), 1, "expected one reply");
        assert_eq!(responses[0].content, "Hello from fixture file!");

        let events = wait_for_finish(&rig, id).await;
        assert_eq!(events, [STARTED, finished(TurnOutcome::Replied)]);

        // `Started` precedes every status and reply of the turn; the reply reached
        // `respond()` before `Finished`.
        let (_, delivered_before_start) = record_position(&rig, id, is_started);
        assert_eq!(delivered_before_start, 0, "a delivery overtook Started");
        let (_, delivered_before_finish) = record_position(&rig, id, is_finished);
        assert!(
            delivered_before_finish > reply_index(&rig, id),
            "Finished must follow the reply"
        );

        rig.shutdown();
    }

    #[tokio::test]
    async fn lifecycle_finished_exactly_once_per_message() {
        let trace = LlmTrace::single_turn(
            "lifecycle-two-messages",
            "first",
            vec![text_step("first reply"), text_step("second reply")],
        );
        let rig = TestRigBuilder::new().with_trace(trace).build().await;

        let first = send(&rig, "first").await;
        let second = send(&rig, "second").await;
        let responses = rig.wait_for_responses(2, TIMEOUT).await;
        let replies: Vec<&str> = responses.iter().map(|r| r.content.as_str()).collect();
        assert_eq!(replies, ["first reply", "second reply"]);

        assert_eq!(
            wait_for_finish(&rig, second).await,
            [STARTED, finished(TurnOutcome::Replied)]
        );
        tokio::time::sleep(SETTLE).await;
        assert_eq!(
            events_for(&rig, first),
            [STARTED, finished(TurnOutcome::Replied)]
        );
        assert_eq!(
            events_for(&rig, second),
            [STARTED, finished(TurnOutcome::Replied)]
        );
        assert_eq!(rig.captured_lifecycle().len(), 4, "no duplicate events");

        // Non-gateway messages run one at a time: the second starts only after the
        // first finished.
        let (first_finished, _) = record_position(&rig, first, is_finished);
        let (second_started, _) = record_position(&rig, second, is_started);
        assert!(first_finished < second_started);

        rig.shutdown();
    }

    #[tokio::test]
    async fn lifecycle_finished_no_reply_for_bare_keyword_without_pending() {
        let rig = TestRigBuilder::new().build().await;

        // A bare approval keyword with nothing pending is consumed silently.
        let id = send(&rig, "yes").await;
        let events = wait_for_finish(&rig, id).await;
        assert_eq!(events, [STARTED, finished(TurnOutcome::NoReply)]);
        tokio::time::sleep(SETTLE).await;
        assert!(rig.captured_responses().is_empty(), "no reply expected");
        assert_eq!(rig.llm_call_count(), 0, "no LLM call expected");

        rig.shutdown();
    }

    /// Stops every reply at `BeforeOutbound`: rejects it, or rewrites it to nothing.
    struct SuppressOutbound {
        reject: bool,
    }

    #[async_trait]
    impl Hook for SuppressOutbound {
        fn name(&self) -> &str {
            "lifecycle-suppress-outbound"
        }

        fn hook_points(&self) -> &[HookPoint] {
            &[HookPoint::BeforeOutbound]
        }

        async fn execute(
            &self,
            _event: &HookEvent,
            _ctx: &HookContext,
        ) -> Result<HookOutcome, HookError> {
            Ok(if self.reject {
                HookOutcome::reject("blocked by the lifecycle test")
            } else {
                HookOutcome::modify(String::new())
            })
        }
    }

    #[tokio::test]
    async fn lifecycle_finished_no_reply_when_outbound_hook_suppresses_reply() {
        for reject in [true, false] {
            let rig = TestRigBuilder::new()
                .with_trace(fixture("simple_text.json"))
                .with_hook(Arc::new(SuppressOutbound { reject }))
                .build()
                .await;

            // The model answers, but the reply never reaches `respond()`: the turn
            // finishes `no_reply`, not `replied`.
            let id = send(&rig, "hello").await;
            let events = wait_for_finish(&rig, id).await;
            assert_eq!(
                events,
                [STARTED, finished(TurnOutcome::NoReply)],
                "reject: {reject}"
            );
            assert!(rig.llm_call_count() > 0, "the model was asked");
            tokio::time::sleep(SETTLE).await;
            assert!(rig.captured_responses().is_empty(), "reject: {reject}");

            rig.shutdown();
        }
    }

    #[tokio::test]
    async fn lifecycle_finished_paused_when_tool_needs_approval() {
        // `read_file` needs approval unless auto-approved; the probe path does not exist,
        // so the approved call fails harmlessly and the model answers.
        let tool_step = TraceStep {
            request_hint: None,
            response: TraceResponse::ToolCalls {
                tool_calls: vec![TraceToolCall {
                    id: "call_read_file_lifecycle".to_string(),
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({
                        "path": "lifecycle-approval-probe-does-not-exist.txt"
                    }),
                }],
                input_tokens: 40,
                output_tokens: 10,
            },
            expected_tool_results: Vec::new(),
        };
        let trace = LlmTrace::single_turn(
            "lifecycle-paused",
            "read the probe file",
            vec![tool_step, text_step("The probe file does not exist.")],
        );
        let rig = TestRigBuilder::new()
            .with_trace(trace)
            .with_auto_approve_tools(false)
            .build()
            .await;

        let id = send(&rig, "read the probe file").await;
        let events = wait_for_finish(&rig, id).await;
        assert_eq!(events, [STARTED, finished(TurnOutcome::Paused)]);
        assert!(
            rig.captured_responses().is_empty(),
            "a paused turn has no reply"
        );

        // The approval prompt reached the channel before the turn finished.
        let prompt_index = rig
            .captured_deliveries()
            .iter()
            .position(|delivery| {
                matches!(
                    delivery,
                    CapturedDelivery::Status(StatusUpdate::ApprovalNeeded { .. })
                )
            })
            .expect("approval prompt was not delivered");
        let (_, delivered_before_finish) = record_position(&rig, id, is_finished);
        assert!(delivered_before_finish > prompt_index);

        // Approving resumes the work under the approval message, which has its own
        // lifecycle; the paused message never finishes a second time.
        let approval = send(&rig, "yes").await;
        let responses = rig.wait_for_responses(1, TIMEOUT).await;
        assert_eq!(responses.len(), 1, "expected the resumed turn's reply");
        assert_eq!(responses[0].content, "The probe file does not exist.");
        assert_eq!(
            wait_for_finish(&rig, approval).await,
            [STARTED, finished(TurnOutcome::Replied)]
        );
        tokio::time::sleep(SETTLE).await;
        assert_eq!(
            events_for(&rig, id),
            [STARTED, finished(TurnOutcome::Paused)]
        );

        rig.shutdown();
    }

    #[tokio::test]
    async fn lifecycle_finished_failed_for_error_reply() {
        let rig = TestRigBuilder::new()
            .with_trace(fixture("error_path.json"))
            .build()
            .await;

        // A tool error the model recovers from is still a replied turn.
        let recovered = send(&rig, "Read a file for me").await;
        let responses = rig.wait_for_responses(1, TIMEOUT).await;
        assert_eq!(responses.len(), 1);
        assert!(
            rig.tool_calls_completed()
                .iter()
                .any(|(name, success)| name == "read_file" && !success),
            "the fixture's read_file call should fail"
        );
        assert_eq!(
            wait_for_finish(&rig, recovered).await,
            [STARTED, finished(TurnOutcome::Replied)]
        );

        // The trace has no step left, so the next model call fails: the turn is
        // answered `Error: ...` and finishes `failed`.
        let failed = send(&rig, "Try once more").await;
        let responses = rig.wait_for_responses(2, TIMEOUT).await;
        assert_eq!(responses.len(), 2);
        assert!(
            responses[1].content.starts_with("Error:"),
            "expected an error reply, got {:?}",
            responses[1].content
        );
        assert_eq!(
            wait_for_finish(&rig, failed).await,
            [STARTED, finished(TurnOutcome::Failed)]
        );

        rig.shutdown();
    }

    // -----------------------------------------------------------------------
    // The remaining supervisor arms and the inline paths (beyond design 11.1).
    // -----------------------------------------------------------------------

    /// Holds the first model call until `release` is notified; later calls answer at once.
    struct GatedLlm {
        calls: AtomicUsize,
        started: Notify,
        release: Notify,
    }

    impl GatedLlm {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                started: Notify::new(),
                release: Notify::new(),
            }
        }

        async fn next_content(&self) -> String {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                self.started.notify_one();
                self.release.notified().await;
                "released reply".to_string()
            } else {
                format!("reply {call}")
            }
        }
    }

    #[async_trait]
    impl LlmProvider for GatedLlm {
        fn model_name(&self) -> &str {
            "lifecycle-gated"
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
                reasoning: None,
                input_tokens: 1,
                output_tokens: 1,
                reasoning_tokens: 0,
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
                reasoning: None,
                input_tokens: 1,
                output_tokens: 1,
                reasoning_tokens: 0,
                finish_reason: FinishReason::Stop,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            })
        }
    }

    /// Panics on every model call, inside the supervised handler task.
    struct PanickingLlm;

    #[async_trait]
    impl LlmProvider for PanickingLlm {
        fn model_name(&self) -> &str {
            "lifecycle-panicking"
        }

        fn cost_per_token(&self) -> (Decimal, Decimal) {
            (Decimal::ZERO, Decimal::ZERO)
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, LlmError> {
            panic!("lifecycle test: deliberate model panic");
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, LlmError> {
            panic!("lifecycle test: deliberate model panic");
        }
    }

    #[tokio::test]
    async fn lifecycle_cancelled_for_queued_message_removed_by_interrupt() {
        let llm = Arc::new(GatedLlm::new());
        let rig = TestRigBuilder::new()
            .with_llm(Arc::clone(&llm) as Arc<dyn LlmProvider>)
            .build()
            .await;

        // The running turn holds the (exclusive) scheduler slot; a message for another
        // conversation lane waits in the queue behind it.
        let running = send(&rig, "hold the model").await;
        tokio::time::timeout(TIMEOUT, llm.started.notified())
            .await
            .expect("the first model call should start");
        let queued = send_message(
            &rig,
            IncomingMessage::new(CHANNEL, USER, "queued prompt").with_thread("lane-b"),
        )
        .await;
        let interrupt = send_message(
            &rig,
            IncomingMessage::new(CHANNEL, USER, "/interrupt").with_thread("lane-b"),
        )
        .await;

        // The interrupt removes the queued prompt, which never ran, and is answered itself.
        let responses = rig.wait_for_responses(1, TIMEOUT).await;
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].content, "Interrupted.");
        assert_eq!(
            wait_for_finish(&rig, interrupt).await,
            [STARTED, finished(TurnOutcome::Replied)]
        );
        assert_eq!(events_for(&rig, queued), [finished(TurnOutcome::Cancelled)]);
        let (queued_finished, _) = record_position(&rig, queued, is_finished);
        let (interrupt_started, _) = record_position(&rig, interrupt, is_started);
        assert!(queued_finished < interrupt_started);

        // The running turn in the other lane is unaffected and finishes normally.
        llm.release.notify_one();
        let responses = rig.wait_for_responses(2, TIMEOUT).await;
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[1].content, "released reply");
        assert_eq!(
            wait_for_finish(&rig, running).await,
            [STARTED, finished(TurnOutcome::Replied)]
        );
        tokio::time::sleep(SETTLE).await;
        assert_eq!(events_for(&rig, queued), [finished(TurnOutcome::Cancelled)]);
        assert_eq!(
            llm.calls.load(Ordering::SeqCst),
            1,
            "the queued prompt must never run"
        );

        rig.shutdown();
    }

    #[tokio::test]
    async fn lifecycle_finished_timed_out_after_soft_timeout() {
        let llm = Arc::new(GatedLlm::new());
        let rig = TestRigBuilder::new()
            .with_llm(Arc::clone(&llm) as Arc<dyn LlmProvider>)
            .with_handle_message_timeout(Duration::from_millis(500))
            .build()
            .await;

        let id = send(&rig, "never answered in time").await;
        let responses = rig.wait_for_responses(1, TIMEOUT).await;
        assert_eq!(responses.len(), 1);
        assert!(
            responses[0].content.contains("timed out"),
            "expected the timeout notice, got {:?}",
            responses[0].content
        );
        assert_eq!(
            wait_for_finish(&rig, id).await,
            [STARTED, finished(TurnOutcome::TimedOut)]
        );
        let (_, delivered_before_finish) = record_position(&rig, id, is_finished);
        assert!(delivered_before_finish > reply_index(&rig, id));

        rig.shutdown();
    }

    #[tokio::test]
    async fn lifecycle_finished_failed_when_handler_panics() {
        let rig = TestRigBuilder::new()
            .with_llm(Arc::new(PanickingLlm) as Arc<dyn LlmProvider>)
            .build()
            .await;

        let id = send(&rig, "trigger a panic").await;
        let responses = rig.wait_for_responses(1, TIMEOUT).await;
        assert_eq!(responses.len(), 1);
        assert_eq!(
            responses[0].content,
            "Sorry, something went wrong. Please try again."
        );
        assert_eq!(
            wait_for_finish(&rig, id).await,
            [STARTED, finished(TurnOutcome::Failed)]
        );

        rig.shutdown();
    }

    #[tokio::test]
    async fn lifecycle_reported_for_inline_priority_control() {
        let rig = TestRigBuilder::new().build().await;

        // `/interrupt` with nothing running is handled inline on the main loop.
        let id = send(&rig, "/interrupt").await;
        let responses = rig.wait_for_responses(1, TIMEOUT).await;
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].content, "Nothing to interrupt.");
        assert_eq!(
            wait_for_finish(&rig, id).await,
            [STARTED, finished(TurnOutcome::Replied)]
        );
        assert_eq!(rig.llm_call_count(), 0);

        rig.shutdown();
    }
}
