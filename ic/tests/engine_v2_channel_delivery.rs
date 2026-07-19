mod support;

use std::future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use rust_decimal::Decimal;
use tokio::sync::Notify;
use uuid::Uuid;

use lunarwing::channels::{AttachmentKind, IncomingAttachment, IncomingMessage, StatusUpdate};
use lunarwing::context::JobContext;
use lunarwing::error::LlmError;
use lunarwing::hooks::{Hook, HookContext, HookError, HookEvent, HookOutcome, HookPoint};
use lunarwing::llm::{
    ChatMessage, CompletionRequest, CompletionResponse, ContentPart, FinishReason, LlmProvider,
    LlmStream, LlmStreamChunk, Role, TokenUsage, ToolCall, ToolCompletionRequest,
    ToolCompletionResponse,
};
use lunarwing::tools::{ApprovalRequirement, Tool, ToolError, ToolOutput};

use support::engine_v2_env::EngineV2EnvGuard;
use support::test_channel::CapturedDelivery;
use support::test_rig::TestRigBuilder;

const TERMINAL_RESPONSE: &str = "channel-final";
const AUTH_TERMINAL_RESPONSE: &str = "authenticated-final";
const APPROVAL_CALL_ID: &str = "callgate1";
const APPROVAL_REPLAY_CALL_ID: &str = "callgate2";
const APPROVAL_CONTEXT_MARKER: &str = "The user explicitly approved this action";
const OUTBOUND_POINTS: [HookPoint; 1] = [HookPoint::BeforeOutbound];

struct DeterministicStreamingLlm;

#[derive(Default)]
struct CapturingMultimodalLlm {
    requests: Mutex<Vec<Vec<ChatMessage>>>,
}

impl CapturingMultimodalLlm {
    fn record(&self, messages: &[ChatMessage]) {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(messages.to_vec());
    }

    fn captured_requests(&self) -> Vec<Vec<ChatMessage>> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

struct FailingLlm;

struct DropProbe(Arc<AtomicBool>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct PendingLlm {
    started: Notify,
    dropped: Arc<AtomicBool>,
}

struct ApprovalLlm {
    calls: AtomicUsize,
}

struct AuthLlm {
    calls: AtomicUsize,
}

fn incoming_attachment(
    kind: AttachmentKind,
    mime_type: &str,
    filename: &str,
    data: Vec<u8>,
    extracted_text: Option<&str>,
) -> IncomingAttachment {
    IncomingAttachment {
        id: Uuid::new_v4().to_string(),
        kind,
        mime_type: mime_type.to_string(),
        filename: Some(filename.to_string()),
        size_bytes: Some(data.len() as u64),
        source_url: Some(format!("https://private.example/{filename}")),
        storage_key: Some(format!("/srv/lunarwing/private/{filename}")),
        extracted_text: extracted_text.map(ToString::to_string),
        data,
        duration_secs: None,
    }
}

impl ApprovalLlm {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }

    fn has_executed_result(request: &ToolCompletionRequest) -> bool {
        request.messages.iter().any(|message| {
            message.role == Role::Tool
                && message.tool_call_id.as_deref() == Some(APPROVAL_CALL_ID)
                && message.name.as_deref() == Some("phase5_gate")
                && message.content.contains("gate executed")
        })
    }

    fn has_approval_context(request: &ToolCompletionRequest) -> bool {
        request.messages.iter().any(|message| {
            message.role == Role::Tool
                && message.tool_call_id.as_deref() == Some(APPROVAL_CALL_ID)
                && message.content.contains(APPROVAL_CONTEXT_MARKER)
        })
    }

    fn next_stream(
        &self,
        has_executed_result: bool,
        has_approval_context: bool,
    ) -> Result<LlmStream<'static>, LlmError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = match call {
            0 => vec![
                Ok(LlmStreamChunk::ToolCallDelta {
                    index: 0,
                    id: Some(APPROVAL_CALL_ID.to_string()),
                    name: Some("phase5_gate".to_string()),
                    args_delta: "{}".to_string(),
                }),
                Ok(LlmStreamChunk::Done {
                    usage: Some(TokenUsage::default()),
                    finish_reason: "tool_calls".to_string(),
                }),
            ],
            1 if has_executed_result && has_approval_context => vec![
                Ok(LlmStreamChunk::TextDelta(
                    "```repl\nFINAL('approved-final')\n```".to_string(),
                )),
                Ok(LlmStreamChunk::Done {
                    usage: Some(TokenUsage::default()),
                    finish_reason: "stop".to_string(),
                }),
            ],
            1 if has_executed_result => vec![
                Ok(LlmStreamChunk::TextDelta(
                    "```repl\nFINAL('approval-context-missing')\n```".to_string(),
                )),
                Ok(LlmStreamChunk::Done {
                    usage: Some(TokenUsage::default()),
                    finish_reason: "stop".to_string(),
                }),
            ],
            1 => vec![
                Ok(LlmStreamChunk::ToolCallDelta {
                    index: 0,
                    id: Some(APPROVAL_REPLAY_CALL_ID.to_string()),
                    name: Some("phase5_gate".to_string()),
                    args_delta: "{}".to_string(),
                }),
                Ok(LlmStreamChunk::Done {
                    usage: Some(TokenUsage::default()),
                    finish_reason: "tool_calls".to_string(),
                }),
            ],
            _ => return Err(provider_failure()),
        };
        Ok(futures::stream::iter(chunks).boxed())
    }
}

impl AuthLlm {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }

    fn next_code_response(&self) -> Result<String, LlmError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match call {
            0 | 1 => Ok(
                "```repl\nFINAL('{\"error\":\"authentication_required\",\"credential_name\":\"phase5_credential\"}')\n```"
                    .to_string(),
            ),
            2 => Ok(format!(
                "```repl\nFINAL('{AUTH_TERMINAL_RESPONSE}')\n```"
            )),
            _ => Err(provider_failure()),
        }
    }

    fn next_stream(&self) -> Result<LlmStream<'static>, LlmError> {
        let content = self.next_code_response()?;
        Ok(futures::stream::iter([
            Ok(LlmStreamChunk::TextDelta(content)),
            Ok(LlmStreamChunk::Done {
                usage: Some(TokenUsage::default()),
                finish_reason: "stop".to_string(),
            }),
        ])
        .boxed())
    }
}

struct ApprovalTool {
    executions: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for ApprovalTool {
    fn name(&self) -> &str {
        "phase5_gate"
    }

    fn description(&self) -> &str {
        "Phase 5 approval routing fixture"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    async fn execute(
        &self,
        _params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("gate executed", Duration::ZERO))
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::Always
    }

    fn requires_sanitization(&self) -> bool {
        false
    }
}

impl PendingLlm {
    fn new(dropped: Arc<AtomicBool>) -> Self {
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

fn provider_failure() -> LlmError {
    LlmError::RequestFailed {
        provider: "phase5-test".to_string(),
        reason: "expected failure".to_string(),
    }
}

impl DeterministicStreamingLlm {
    fn code_response() -> String {
        format!("```repl\nFINAL('{TERMINAL_RESPONSE}')\n```")
    }

    fn stream() -> LlmStream<'static> {
        futures::stream::iter([
            Ok(LlmStreamChunk::TextDelta(
                "```repl\nFINAL('channel-".to_string(),
            )),
            Ok(LlmStreamChunk::TextDelta("final')\n```".to_string())),
            Ok(LlmStreamChunk::Done {
                usage: Some(TokenUsage {
                    input_tokens: 3,
                    output_tokens: 2,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                }),
                finish_reason: "stop".to_string(),
            }),
        ])
        .boxed()
    }
}

#[async_trait]
impl LlmProvider for DeterministicStreamingLlm {
    fn model_name(&self) -> &str {
        "phase5-delivery-test"
    }

    fn cost_per_token(&self) -> (Decimal, Decimal) {
        (Decimal::ZERO, Decimal::ZERO)
    }

    async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        Ok(CompletionResponse {
            content: Self::code_response(),
            input_tokens: 3,
            output_tokens: 2,
            finish_reason: FinishReason::Stop,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        })
    }

    async fn complete_stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        Ok(Self::stream())
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        Ok(ToolCompletionResponse {
            content: Some(Self::code_response()),
            tool_calls: Vec::new(),
            input_tokens: 3,
            output_tokens: 2,
            finish_reason: FinishReason::Stop,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        })
    }

    async fn complete_with_tools_stream(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        Ok(Self::stream())
    }
}

#[async_trait]
impl LlmProvider for CapturingMultimodalLlm {
    fn model_name(&self) -> &str {
        "phase6-multimodal-capture"
    }

    fn cost_per_token(&self) -> (Decimal, Decimal) {
        (Decimal::ZERO, Decimal::ZERO)
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        self.record(&request.messages);
        Ok(CompletionResponse {
            content: DeterministicStreamingLlm::code_response(),
            input_tokens: 3,
            output_tokens: 2,
            finish_reason: FinishReason::Stop,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        })
    }

    async fn complete_stream(&self, request: CompletionRequest) -> Result<LlmStream<'_>, LlmError> {
        self.record(&request.messages);
        Ok(DeterministicStreamingLlm::stream())
    }

    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        self.record(&request.messages);
        Ok(ToolCompletionResponse {
            content: Some(DeterministicStreamingLlm::code_response()),
            tool_calls: Vec::new(),
            input_tokens: 3,
            output_tokens: 2,
            finish_reason: FinishReason::Stop,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        })
    }

    async fn complete_with_tools_stream(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        self.record(&request.messages);
        Ok(DeterministicStreamingLlm::stream())
    }
}

#[async_trait]
impl LlmProvider for FailingLlm {
    fn model_name(&self) -> &str {
        "phase5-failing-test"
    }

    fn cost_per_token(&self) -> (Decimal, Decimal) {
        (Decimal::ZERO, Decimal::ZERO)
    }

    async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        Err(provider_failure())
    }

    async fn complete_stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        Err(provider_failure())
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        Err(provider_failure())
    }

    async fn complete_with_tools_stream(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        Err(provider_failure())
    }
}

#[async_trait]
impl LlmProvider for PendingLlm {
    fn model_name(&self) -> &str {
        "phase5-pending-test"
    }

    fn cost_per_token(&self) -> (Decimal, Decimal) {
        (Decimal::ZERO, Decimal::ZERO)
    }

    async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        self.wait_forever().await
    }

    async fn complete_stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        self.wait_forever().await
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        self.wait_forever().await
    }

    async fn complete_with_tools_stream(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        self.wait_forever().await
    }
}

#[async_trait]
impl LlmProvider for ApprovalLlm {
    fn model_name(&self) -> &str {
        "phase5-approval-test"
    }

    fn cost_per_token(&self) -> (Decimal, Decimal) {
        (Decimal::ZERO, Decimal::ZERO)
    }

    async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        Err(provider_failure())
    }

    async fn complete_stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        self.next_stream(false, false)
    }

    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        let has_executed_result = Self::has_executed_result(&request);
        let has_approval_context = Self::has_approval_context(&request);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match call {
            0 => Ok(ToolCompletionResponse {
                content: None,
                tool_calls: vec![ToolCall {
                    id: APPROVAL_CALL_ID.to_string(),
                    name: "phase5_gate".to_string(),
                    arguments: serde_json::json!({}),
                    reasoning: None,
                }],
                input_tokens: 1,
                output_tokens: 1,
                finish_reason: FinishReason::ToolUse,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            }),
            1 if has_executed_result && has_approval_context => Ok(ToolCompletionResponse {
                content: Some("```repl\nFINAL('approved-final')\n```".to_string()),
                tool_calls: Vec::new(),
                input_tokens: 1,
                output_tokens: 1,
                finish_reason: FinishReason::Stop,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            }),
            1 if has_executed_result => Ok(ToolCompletionResponse {
                content: Some("```repl\nFINAL('approval-context-missing')\n```".to_string()),
                tool_calls: Vec::new(),
                input_tokens: 1,
                output_tokens: 1,
                finish_reason: FinishReason::Stop,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            }),
            1 => Ok(ToolCompletionResponse {
                content: None,
                tool_calls: vec![ToolCall {
                    id: APPROVAL_REPLAY_CALL_ID.to_string(),
                    name: "phase5_gate".to_string(),
                    arguments: serde_json::json!({}),
                    reasoning: None,
                }],
                input_tokens: 1,
                output_tokens: 1,
                finish_reason: FinishReason::ToolUse,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            }),
            _ => Err(provider_failure()),
        }
    }

    async fn complete_with_tools_stream(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        self.next_stream(
            Self::has_executed_result(&request),
            Self::has_approval_context(&request),
        )
    }
}

#[async_trait]
impl LlmProvider for AuthLlm {
    fn model_name(&self) -> &str {
        "phase5-auth-test"
    }

    fn cost_per_token(&self) -> (Decimal, Decimal) {
        (Decimal::ZERO, Decimal::ZERO)
    }

    async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let content = self.next_code_response()?;
        Ok(CompletionResponse {
            content,
            input_tokens: 1,
            output_tokens: 1,
            finish_reason: FinishReason::Stop,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        })
    }

    async fn complete_stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        self.next_stream()
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        Ok(ToolCompletionResponse {
            content: Some(self.next_code_response()?),
            tool_calls: Vec::new(),
            input_tokens: 1,
            output_tokens: 1,
            finish_reason: FinishReason::Stop,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        })
    }

    async fn complete_with_tools_stream(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        self.next_stream()
    }
}

struct DeliveryCase {
    channel: &'static str,
    scope: String,
    metadata: serde_json::Value,
}

enum HookBehavior {
    Modify,
    Reject,
}

struct OutboundHook {
    behavior: HookBehavior,
}

#[async_trait]
impl Hook for OutboundHook {
    fn name(&self) -> &str {
        match self.behavior {
            HookBehavior::Modify => "phase5-modify",
            HookBehavior::Reject => "phase5-reject",
        }
    }

    fn hook_points(&self) -> &[HookPoint] {
        &OUTBOUND_POINTS
    }

    async fn execute(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext,
    ) -> Result<HookOutcome, HookError> {
        Ok(match self.behavior {
            HookBehavior::Modify => HookOutcome::modify("modified-by-hook".to_string()),
            HookBehavior::Reject => HookOutcome::reject("blocked"),
        })
    }
}

async fn assert_delivery_case(case: DeliveryCase) {
    lunarwing::bridge::reset_engine_state().await;

    let llm: Arc<dyn LlmProvider> = Arc::new(DeterministicStreamingLlm);
    let rig = TestRigBuilder::new()
        .with_channel_name(case.channel)
        .with_llm(llm)
        .build()
        .await;
    let message = IncomingMessage::new(case.channel, "test-user", "finish once")
        .with_conversation_scope(case.scope.clone())
        .with_metadata(case.metadata.clone());

    rig.send_incoming(message).await;
    let responses = rig.wait_for_responses(1, Duration::from_secs(10)).await;
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].content, TERMINAL_RESPONSE);

    let deliveries = rig.captured_deliveries();
    let chunk_index = deliveries
        .iter()
        .position(|delivery| {
            matches!(
                delivery,
                CapturedDelivery::Status(StatusUpdate::StreamChunk(_))
            )
        })
        .expect("stream chunk should be delivered");
    let response_index = deliveries
        .iter()
        .position(|delivery| matches!(delivery, CapturedDelivery::Response { .. }))
        .expect("terminal response should be delivered");
    assert!(chunk_index < response_index);

    let CapturedDelivery::Response {
        message: delivered_message,
        response: delivered_response,
    } = &deliveries[response_index]
    else {
        panic!("response index should contain a response delivery");
    };
    assert_eq!(delivered_message.metadata, case.metadata);
    assert_eq!(delivered_response.content, TERMINAL_RESPONSE);

    let conversation_id = rig
        .database()
        .get_or_create_scoped_conversation(case.channel, "test-user", &case.scope)
        .await
        .expect("scoped conversation should resolve");
    let messages = rig
        .database()
        .list_conversation_messages(conversation_id)
        .await
        .expect("history should load");
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.role == "assistant")
            .count(),
        1
    );

    rig.shutdown_and_wait().await;
    lunarwing::bridge::reset_engine_state().await;
}

async fn assert_legacy_case(channel: &'static str, scope: &str) {
    lunarwing::bridge::reset_engine_state().await;

    let llm: Arc<dyn LlmProvider> = Arc::new(DeterministicStreamingLlm);
    let rig = TestRigBuilder::new()
        .with_channel_name(channel)
        .with_llm(llm)
        .build()
        .await;
    let message = IncomingMessage::new(channel, "test-user", "stay legacy")
        .with_conversation_scope(scope)
        .with_metadata(serde_json::json!({"scope_marker": scope}));

    rig.send_incoming(message).await;
    let responses = rig.wait_for_responses(1, Duration::from_secs(10)).await;
    assert_eq!(responses.len(), 1);
    assert!(
        !rig.captured_status_events()
            .iter()
            .any(|status| matches!(status, StatusUpdate::StreamChunk(_)))
    );
    assert!(
        engine_threads_for_user().await.is_empty(),
        "legacy route must not create an engine thread"
    );

    rig.shutdown_and_wait().await;
    lunarwing::bridge::reset_engine_state().await;
}

async fn engine_threads_for_user() -> Vec<lunarwing::bridge::EngineThreadInfo> {
    let projects = lunarwing::bridge::list_engine_projects("test-user")
        .await
        .expect("engine project lookup should work");
    let mut threads = Vec::new();
    for project in projects {
        threads.extend(
            lunarwing::bridge::list_engine_threads(Some(&project.id), "test-user")
                .await
                .expect("engine thread lookup should work"),
        );
    }
    threads
}

async fn wait_for_engine_completion() {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let threads = engine_threads_for_user().await;
            if threads.iter().any(|thread| thread.state == "Done") {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("engine thread should complete");
}

fn captured_user_message(provider: &CapturingMultimodalLlm, marker: &str) -> ChatMessage {
    provider
        .captured_requests()
        .into_iter()
        .flatten()
        .find(|message| message.role == Role::User && message.content.contains(marker))
        .unwrap_or_else(|| panic!("provider request should contain user marker {marker:?}"))
}

async fn assert_xmpp_image_only_attachment_case() {
    lunarwing::bridge::reset_engine_state().await;

    let provider = Arc::new(CapturingMultimodalLlm::default());
    let llm: Arc<dyn LlmProvider> = provider.clone();
    let rig = TestRigBuilder::new()
        .with_channel_name("xmpp")
        .with_llm(llm)
        .build()
        .await;
    let scope = Uuid::new_v4().to_string();
    let mut message = IncomingMessage::new("xmpp", "test-user", "")
        .with_conversation_scope(scope.clone())
        .with_metadata(serde_json::json!({"xmpp_target": "alice@example.org"}));
    message.attachments.push(incoming_attachment(
        AttachmentKind::Image,
        "image/png",
        "image-only.png",
        vec![1, 2, 3],
        None,
    ));

    rig.send_incoming(message).await;
    let responses = rig.wait_for_responses(1, Duration::from_secs(10)).await;
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].content, TERMINAL_RESPONSE);

    let provider_message = captured_user_message(&provider, "image-only.png");
    assert!(!provider_message.content.trim().is_empty());
    assert!(!provider_message.content.contains("private.example"));
    assert!(!provider_message.content.contains("/srv/lunarwing/private/"));
    assert_eq!(provider_message.content_parts.len(), 1);
    match &provider_message.content_parts[0] {
        ContentPart::ImageUrl { image_url } => {
            assert_eq!(image_url.url, "data:image/png;base64,AQID");
        }
        other => panic!("expected provider-native image content, got {other:?}"),
    }

    let conversation_id = rig
        .database()
        .get_or_create_scoped_conversation("xmpp", "test-user", &scope)
        .await
        .expect("scoped conversation should resolve");
    let history = rig
        .database()
        .list_conversation_messages(conversation_id)
        .await
        .expect("history should load");
    let persisted = history
        .iter()
        .find(|entry| entry.role == "user")
        .expect("compatibility history should contain the user message");
    assert_eq!(persisted.content, provider_message.content);
    assert!(!persisted.content.contains("data:image"));
    assert!(!persisted.content.contains("AQID"));

    rig.shutdown_and_wait().await;
    lunarwing::bridge::reset_engine_state().await;
}

async fn assert_xmpp_mixed_attachment_case() {
    lunarwing::bridge::reset_engine_state().await;

    let provider = Arc::new(CapturingMultimodalLlm::default());
    let llm: Arc<dyn LlmProvider> = provider.clone();
    let rig = TestRigBuilder::new()
        .with_channel_name("xmpp")
        .with_llm(llm)
        .build()
        .await;
    let scope = Uuid::new_v4().to_string();
    let mut message = IncomingMessage::new("xmpp", "test-user", "review mixed attachments")
        .with_conversation_scope(scope.clone())
        .with_metadata(serde_json::json!({"xmpp_target": "alice@example.org"}));
    message.attachments = vec![
        incoming_attachment(
            AttachmentKind::Document,
            "application/pdf",
            "report.pdf",
            vec![9],
            Some("DOCUMENT_SENTINEL"),
        ),
        incoming_attachment(
            AttachmentKind::Audio,
            "audio/ogg",
            "voice.ogg",
            vec![8],
            Some("AUDIO_SENTINEL"),
        ),
        incoming_attachment(
            AttachmentKind::Image,
            "image/png",
            "chart.png",
            vec![4, 5, 6],
            None,
        ),
    ];

    rig.send_incoming(message).await;
    let responses = rig.wait_for_responses(1, Duration::from_secs(10)).await;
    assert_eq!(responses.len(), 1);

    let provider_message = captured_user_message(&provider, "DOCUMENT_SENTINEL");
    assert_eq!(
        provider_message
            .content
            .matches("DOCUMENT_SENTINEL")
            .count(),
        1
    );
    assert_eq!(
        provider_message.content.matches("AUDIO_SENTINEL").count(),
        1
    );
    assert_eq!(provider_message.content_parts.len(), 1);
    assert!(!provider_message.content.contains("private.example"));
    assert!(!provider_message.content.contains("/srv/lunarwing/private/"));
    match &provider_message.content_parts[0] {
        ContentPart::ImageUrl { image_url } => {
            assert_eq!(image_url.url, "data:image/png;base64,BAUG");
        }
        other => panic!("expected provider-native image content, got {other:?}"),
    }

    let conversation_id = rig
        .database()
        .get_or_create_scoped_conversation("xmpp", "test-user", &scope)
        .await
        .expect("scoped conversation should resolve");
    let history = rig
        .database()
        .list_conversation_messages(conversation_id)
        .await
        .expect("history should load");
    let persisted = history
        .iter()
        .find(|entry| entry.role == "user")
        .expect("compatibility history should contain the user message");
    assert_eq!(persisted.content, provider_message.content);
    assert_eq!(persisted.content.matches("DOCUMENT_SENTINEL").count(), 1);
    assert_eq!(persisted.content.matches("AUDIO_SENTINEL").count(), 1);
    assert!(!persisted.content.contains("data:image"));
    assert!(!persisted.content.contains("BAUG"));

    rig.shutdown_and_wait().await;
    lunarwing::bridge::reset_engine_state().await;
}

async fn assert_attachment_secret_scan_case() {
    lunarwing::bridge::reset_engine_state().await;

    let provider = Arc::new(CapturingMultimodalLlm::default());
    let llm: Arc<dyn LlmProvider> = provider.clone();
    let rig = TestRigBuilder::new()
        .with_channel_name("xmpp")
        .with_llm(llm)
        .build()
        .await;
    let mut message = IncomingMessage::new("xmpp", "test-user", "review this document")
        .with_conversation_scope(Uuid::new_v4().to_string())
        .with_metadata(serde_json::json!({"xmpp_target": "alice@example.org"}));
    message.attachments.push(incoming_attachment(
        AttachmentKind::Document,
        "text/plain",
        "credentials.txt",
        Vec::new(),
        Some("AKIAIOSFODNN7EXAMPLE"),
    ));

    rig.send_incoming(message).await;
    let responses = rig.wait_for_responses(1, Duration::from_secs(10)).await;
    assert_eq!(responses.len(), 1);
    assert!(responses[0].content.contains("appears to contain a secret"));
    assert!(provider.captured_requests().is_empty());
    assert!(engine_threads_for_user().await.is_empty());

    rig.shutdown_and_wait().await;
    lunarwing::bridge::reset_engine_state().await;
}

async fn assert_hook_case(behavior: HookBehavior, expected_response: Option<&str>) {
    lunarwing::bridge::reset_engine_state().await;

    let llm: Arc<dyn LlmProvider> = Arc::new(DeterministicStreamingLlm);
    let rig = TestRigBuilder::new()
        .with_channel_name("xmpp")
        .with_llm(llm)
        .with_hook(Arc::new(OutboundHook { behavior }))
        .build()
        .await;
    let scope = Uuid::new_v4().to_string();
    let message = IncomingMessage::new("xmpp", "test-user", "apply outbound hook")
        .with_conversation_scope(scope)
        .with_metadata(serde_json::json!({"xmpp_target": "alice@example.org"}));

    rig.send_incoming(message).await;
    match expected_response {
        Some(expected) => {
            let responses = rig.wait_for_responses(1, Duration::from_secs(10)).await;
            assert_eq!(responses.len(), 1);
            assert_eq!(responses[0].content, expected);
        }
        None => {
            wait_for_engine_completion().await;
            assert!(rig.captured_responses().is_empty());
        }
    }

    rig.shutdown_and_wait().await;
    lunarwing::bridge::reset_engine_state().await;
}

async fn assert_error_case() {
    lunarwing::bridge::reset_engine_state().await;

    let llm: Arc<dyn LlmProvider> = Arc::new(FailingLlm);
    let rig = TestRigBuilder::new()
        .with_channel_name("xmpp")
        .with_llm(llm)
        .build()
        .await;
    let message = IncomingMessage::new("xmpp", "test-user", "fail once")
        .with_conversation_scope(Uuid::new_v4().to_string())
        .with_metadata(serde_json::json!({"xmpp_target": "alice@example.org"}));

    rig.send_incoming(message).await;
    let responses = rig.wait_for_responses(1, Duration::from_secs(10)).await;
    assert_eq!(responses.len(), 1);
    assert!(responses[0].content.starts_with("Error:"));
    assert_eq!(
        rig.captured_deliveries()
            .iter()
            .filter(|delivery| matches!(delivery, CapturedDelivery::Response { .. }))
            .count(),
        1
    );

    rig.shutdown_and_wait().await;
    lunarwing::bridge::reset_engine_state().await;
}

async fn assert_stopped_case() {
    lunarwing::bridge::reset_engine_state().await;

    let dropped = Arc::new(AtomicBool::new(false));
    let pending = Arc::new(PendingLlm::new(Arc::clone(&dropped)));
    let llm: Arc<dyn LlmProvider> = pending.clone();
    let rig = TestRigBuilder::new()
        .with_channel_name("xmpp")
        .with_llm(llm)
        .build()
        .await;
    let scope = Uuid::new_v4().to_string();
    let metadata = serde_json::json!({"xmpp_target": "alice@example.org"});
    let original = IncomingMessage::new("xmpp", "test-user", "wait forever")
        .with_conversation_scope(scope.clone())
        .with_metadata(metadata.clone());

    rig.send_incoming(original).await;
    tokio::time::timeout(Duration::from_secs(2), pending.started.notified())
        .await
        .expect("provider stream should start");
    let control_sentinel = "INTERRUPT_ATTACHMENT_MUST_NOT_ENTER_HISTORY";
    let mut interrupt = IncomingMessage::new("xmpp", "test-user", "/interrupt")
        .with_conversation_scope(scope.clone())
        .with_metadata(metadata);
    interrupt.attachments.push(incoming_attachment(
        AttachmentKind::Document,
        "text/plain",
        "interrupt.txt",
        Vec::new(),
        Some(control_sentinel),
    ));
    rig.send_incoming(interrupt).await;

    let responses = rig.wait_for_responses(1, Duration::from_secs(2)).await;
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].content, "Interrupted.");
    tokio::time::timeout(Duration::from_secs(1), async {
        while !dropped.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("interrupt should drop the provider stream");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.captured_responses().len(), 1);

    let conversation_id = rig
        .database()
        .get_or_create_scoped_conversation("xmpp", "test-user", &scope)
        .await
        .expect("scoped conversation should resolve");
    let messages = rig
        .database()
        .list_conversation_messages(conversation_id)
        .await
        .expect("history should load");
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.role == "assistant")
            .count(),
        0
    );
    assert!(
        messages
            .iter()
            .all(|message| !message.content.contains(control_sentinel))
    );

    rig.shutdown_and_wait().await;
    lunarwing::bridge::reset_engine_state().await;
}

async fn wait_for_approval_status(rig: &support::test_rig::TestRig) -> bool {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if rig
                .captured_status_events()
                .iter()
                .any(|status| matches!(status, StatusUpdate::ApprovalNeeded { .. }))
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok()
}

async fn wait_for_approval_completion_or_replay(rig: &support::test_rig::TestRig) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let approval_count = rig
                .captured_status_events()
                .iter()
                .filter(|status| matches!(status, StatusUpdate::ApprovalNeeded { .. }))
                .count();
            if !rig.captured_responses().is_empty() || approval_count > 1 {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("approved action should complete or expose an approval replay");
}

async fn wait_for_auth_status_count(rig: &support::test_rig::TestRig, expected: usize) -> bool {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let count = rig
                .captured_status_events()
                .iter()
                .filter(|status| matches!(status, StatusUpdate::AuthRequired { .. }))
                .count();
            if count >= expected {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok()
}

async fn wait_for_engine_thread(goal: &str) -> lunarwing::bridge::EngineThreadInfo {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(thread) = engine_threads_for_user()
                .await
                .into_iter()
                .find(|thread| thread.goal == goal)
            {
                return thread;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    match result {
        Ok(thread) => thread,
        Err(_) => panic!(
            "engine thread for goal {goal:?} missing; threads={:?}",
            engine_threads_for_user().await
        ),
    }
}

async fn assert_approval_case() {
    lunarwing::bridge::reset_engine_state().await;

    let provider = Arc::new(ApprovalLlm::new());
    let llm: Arc<dyn LlmProvider> = provider.clone();
    let executions = Arc::new(AtomicUsize::new(0));
    let tool: Arc<dyn Tool> = Arc::new(ApprovalTool {
        executions: Arc::clone(&executions),
    });
    let rig = TestRigBuilder::new()
        .with_channel_name("xmpp")
        .with_llm(llm)
        .with_auto_approve_tools(false)
        .with_extra_tools(vec![tool])
        .build()
        .await;
    let scope = Uuid::new_v4().to_string();
    let metadata = serde_json::json!({"xmpp_target": "room@conference.example.org"});
    let original = IncomingMessage::new("xmpp", "test-user", "run the gated fixture")
        .with_conversation_scope(scope.clone())
        .with_metadata(metadata.clone());

    rig.send_incoming(original).await;
    let approval_arrived = wait_for_approval_status(&rig).await;
    assert!(
        approval_arrived,
        "approval status missing: calls={}, statuses={:?}, responses={:?}, threads={:?}",
        provider.calls.load(Ordering::SeqCst),
        rig.captured_status_events(),
        rig.captured_responses(),
        engine_threads_for_user().await,
    );
    assert!(rig.captured_responses().is_empty());
    assert_eq!(
        rig.captured_status_events()
            .iter()
            .filter(|status| matches!(status, StatusUpdate::ApprovalNeeded { .. }))
            .count(),
        1
    );

    let control_sentinel = "APPROVAL_ATTACHMENT_MUST_NOT_ENTER_HISTORY";
    let mut approval = IncomingMessage::new("xmpp", "test-user", "yes")
        .with_conversation_scope(scope.clone())
        .with_metadata(metadata);
    approval.attachments.push(incoming_attachment(
        AttachmentKind::Document,
        "text/plain",
        "approval.txt",
        Vec::new(),
        Some(control_sentinel),
    ));
    rig.send_incoming(approval).await;
    wait_for_approval_completion_or_replay(&rig).await;
    assert_eq!(
        rig.captured_status_events()
            .iter()
            .filter(|status| matches!(status, StatusUpdate::ApprovalNeeded { .. }))
            .count(),
        1,
        "the approved action must not create a second approval gate"
    );
    let responses = rig.captured_responses();
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].content, "approved-final");
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);

    let conversation_id = rig
        .database()
        .get_or_create_scoped_conversation("xmpp", "test-user", &scope)
        .await
        .expect("scoped conversation should resolve");
    let messages = rig
        .database()
        .list_conversation_messages(conversation_id)
        .await
        .expect("history should load");
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.role == "assistant")
            .count(),
        1
    );
    assert!(
        messages
            .iter()
            .all(|message| !message.content.contains(control_sentinel))
    );

    rig.shutdown_and_wait().await;
    lunarwing::bridge::reset_engine_state().await;
}

async fn assert_auth_case() {
    lunarwing::bridge::reset_engine_state().await;

    let provider = Arc::new(AuthLlm::new());
    let llm: Arc<dyn LlmProvider> = provider.clone();
    let rig = TestRigBuilder::new()
        .with_channel_name("xmpp")
        .with_llm(llm)
        .build()
        .await;
    let scope_a = "xmpp:room:phase5-auth-a";
    let scope_b = "xmpp:room:phase5-auth-b";
    let metadata_a = serde_json::json!({"xmpp_target": "auth-a@example.org"});
    let metadata_b = serde_json::json!({"xmpp_target": "auth-b@example.org"});

    let first = IncomingMessage::new("xmpp", "test-user", "authenticate scope a")
        .with_conversation_scope(scope_a)
        .with_metadata(metadata_a.clone());
    rig.send_incoming(first).await;
    assert!(wait_for_auth_status_count(&rig, 1).await);
    assert!(rig.captured_responses().is_empty());
    let thread_a = wait_for_engine_thread("authenticate scope a").await;

    let second = IncomingMessage::new("xmpp", "test-user", "authenticate scope b")
        .with_conversation_scope(scope_b)
        .with_metadata(metadata_b.clone());
    rig.send_incoming(second).await;
    let thread_b = wait_for_engine_thread("authenticate scope b").await;
    assert!(wait_for_auth_status_count(&rig, 2).await);
    assert!(rig.captured_responses().is_empty());
    assert_eq!(
        rig.captured_status_events()
            .iter()
            .filter(|status| matches!(status, StatusUpdate::AuthRequired { .. }))
            .count(),
        2
    );

    assert!(
        lunarwing::bridge::get_engine_pending_auth("test-user", Some(&thread_a.id))
            .await
            .is_some()
    );
    assert!(
        lunarwing::bridge::get_engine_pending_auth("test-user", Some(&thread_b.id))
            .await
            .is_some()
    );

    let sentinel = "phase5_auth_token_not_for_history";
    let attachment_sentinel = "AUTH_ATTACHMENT_MUST_NOT_ENTER_HISTORY";
    let mut token = IncomingMessage::new("xmpp", "test-user", sentinel)
        .with_conversation_scope(scope_a)
        .with_metadata(metadata_a);
    token.attachments.push(incoming_attachment(
        AttachmentKind::Document,
        "text/plain",
        "auth.txt",
        Vec::new(),
        Some(attachment_sentinel),
    ));
    rig.send_incoming(token).await;
    let responses = rig.wait_for_responses(1, Duration::from_secs(10)).await;
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].content, AUTH_TERMINAL_RESPONSE);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    assert!(
        lunarwing::bridge::get_engine_pending_auth("test-user", Some(&thread_a.id))
            .await
            .is_none()
    );
    assert!(
        lunarwing::bridge::get_engine_pending_auth("test-user", Some(&thread_b.id))
            .await
            .is_some()
    );

    for scope in [scope_a, scope_b] {
        let conversation_id = rig
            .database()
            .get_or_create_scoped_conversation("xmpp", "test-user", scope)
            .await
            .expect("scoped conversation should resolve");
        let messages = rig
            .database()
            .list_conversation_messages(conversation_id)
            .await
            .expect("history should load");
        assert!(
            messages
                .iter()
                .all(|message| !message.content.contains(sentinel)
                    && !message.content.contains(attachment_sentinel))
        );
        let assistant_count = messages
            .iter()
            .filter(|message| message.role == "assistant")
            .count();
        assert_eq!(assistant_count, usize::from(scope == scope_a));
    }

    for thread in engine_threads_for_user().await {
        let detail = lunarwing::bridge::get_engine_thread(&thread.id, "test-user")
            .await
            .expect("engine thread lookup should work")
            .expect("engine thread should exist");
        assert!(detail.messages.iter().all(|message| {
            message
                .get("content")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|content| {
                    !content.contains(sentinel) && !content.contains(attachment_sentinel)
                })
        }));
    }

    lunarwing::bridge::clear_engine_pending_auth("test-user", Some(&thread_b.id)).await;
    assert!(
        lunarwing::bridge::get_engine_pending_auth("test-user", Some(&thread_b.id))
            .await
            .is_none()
    );

    rig.shutdown_and_wait().await;
    lunarwing::bridge::reset_engine_state().await;
}

#[tokio::test]
async fn engine_v2_channel_delivery_matrix() {
    let env = EngineV2EnvGuard::enable(Some("xmpp,darkirc,weechat"));
    let gateway_scope = Uuid::new_v4().to_string();
    let xmpp_scope = Uuid::new_v4().to_string();
    let cases = [
        DeliveryCase {
            channel: "gateway",
            scope: gateway_scope.clone(),
            metadata: serde_json::json!({
                "thread_id": gateway_scope,
                "user_id": "test-user",
                "client_marker": "preserve-me",
            }),
        },
        DeliveryCase {
            channel: "xmpp",
            scope: xmpp_scope,
            metadata: serde_json::json!({
                "xmpp_target": "room@conference.example.org",
                "xmpp_room": "room@conference.example.org",
                "xmpp_type": "groupchat",
            }),
        },
        DeliveryCase {
            channel: "darkirc",
            scope: "darkirc:dm:alice".to_string(),
            metadata: serde_json::json!({"nick": "alice"}),
        },
        DeliveryCase {
            channel: "weechat",
            scope: "weechat:group:libera:#lunarwing".to_string(),
            metadata: serde_json::json!({
                "buffer": "irc.libera.#lunarwing",
                "network": "libera",
                "target": "#lunarwing",
                "nick": "alice",
                "is_dm": false,
            }),
        },
    ];

    for case in cases {
        assert_delivery_case(case).await;
    }

    env.set_channels(None);
    assert_legacy_case("xmpp", &Uuid::new_v4().to_string()).await;

    env.set_channels(Some("telegram"));
    assert_legacy_case("telegram", "telegram:dm:alice").await;

    env.set_channels(Some("xmpp"));
    assert_xmpp_image_only_attachment_case().await;
    assert_xmpp_mixed_attachment_case().await;
    assert_attachment_secret_scan_case().await;
    assert_hook_case(HookBehavior::Modify, Some("modified-by-hook")).await;
    assert_hook_case(HookBehavior::Reject, None).await;
    assert_error_case().await;
    assert_stopped_case().await;
    assert_approval_case().await;
    assert_auth_case().await;

    env.cleanup().await;
}
