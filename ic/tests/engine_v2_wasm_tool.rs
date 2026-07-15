mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use rust_decimal::Decimal;
use uuid::Uuid;

use lunarwing::channels::IncomingMessage;
use lunarwing::error::LlmError;
use lunarwing::llm::{
    CompletionRequest, CompletionResponse, FinishReason, LlmProvider, LlmStream, LlmStreamChunk,
    Role, TokenUsage, ToolCall, ToolCompletionRequest, ToolCompletionResponse,
};

use support::engine_v2_env::EngineV2EnvGuard;
use support::test_rig::TestRigBuilder;

// ── Constants ─────────────────────────────────────────────────────────

const SOURCE_MARKER: &str = "engine-v2-test-wasm";
const ECHO_CALL_ID: &str = "callecho1";
const DENIED_CALL_ID: &str = "calldenied1";
const ECHO_TOOL_NAME: &str = "echo_tool";
const TERMINAL_RESPONSE: &str = "wasm-echo-ok";
const DENIED_TERMINAL: &str = "wasm-denied-ok";

const WASM_ARTIFACT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/test-echo-tool/target/wasm32-wasip2/release/test_echo_tool.wasm"
);
const CAPABILITIES_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/test-echo-tool/test-echo-tool.capabilities.json"
);

fn assert_artifact_exists() {
    if !std::path::Path::new(WASM_ARTIFACT).exists() {
        panic!(
            "WASM artifact not found at {WASM_ARTIFACT}.\n\
             Build it with:\n  \
             cd ic && taskset -c 0-5 cargo component build -j6 --release \
             --target wasm32-wasip2 \
             --manifest-path tests/fixtures/test-echo-tool/Cargo.toml"
        );
    }
}

// ── LLM: first call requests echo, then terminal ─────────────────────

struct EchoLlm {
    calls: AtomicUsize,
}

impl EchoLlm {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }

    fn has_echo_result(request: &ToolCompletionRequest) -> bool {
        request.messages.iter().any(|msg| {
            msg.role == Role::Tool
                && msg.tool_call_id.as_deref() == Some(ECHO_CALL_ID)
                && msg.content.contains(SOURCE_MARKER)
        })
    }
}

fn provider_failure() -> LlmError {
    LlmError::RequestFailed {
        provider: "wasm-test".to_string(),
        reason: "unexpected call".to_string(),
    }
}

#[async_trait]
impl LlmProvider for EchoLlm {
    fn model_name(&self) -> &str {
        "wasm-echo-test"
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
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match call {
            0 => Ok(ToolCompletionResponse {
                content: None,
                tool_calls: vec![ToolCall {
                    id: ECHO_CALL_ID.to_string(),
                    name: ECHO_TOOL_NAME.to_string(),
                    arguments: serde_json::json!({
                        "action": "echo",
                        "message": "hello-from-engine-v2"
                    }),
                    reasoning: None,
                }],
                input_tokens: 1,
                output_tokens: 1,
                finish_reason: FinishReason::ToolUse,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            }),
            1 if Self::has_echo_result(&request) => Ok(ToolCompletionResponse {
                content: Some(format!("```repl\nFINAL('{TERMINAL_RESPONSE}')\n```")),
                tool_calls: Vec::new(),
                input_tokens: 1,
                output_tokens: 1,
                finish_reason: FinishReason::Stop,
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
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let has_result = Self::has_echo_result(&request);
        let chunks: Vec<Result<LlmStreamChunk, LlmError>> = match call {
            0 => vec![
                Ok(LlmStreamChunk::ToolCallDelta {
                    index: 0,
                    id: Some(ECHO_CALL_ID.to_string()),
                    name: Some(ECHO_TOOL_NAME.to_string()),
                    args_delta: serde_json::json!({
                        "action": "echo",
                        "message": "hello-from-engine-v2"
                    })
                    .to_string(),
                }),
                Ok(LlmStreamChunk::Done {
                    usage: Some(TokenUsage::default()),
                    finish_reason: "tool_calls".to_string(),
                }),
            ],
            1 if has_result => vec![
                Ok(LlmStreamChunk::TextDelta(format!(
                    "```repl\nFINAL('{TERMINAL_RESPONSE}')\n```"
                ))),
                Ok(LlmStreamChunk::Done {
                    usage: Some(TokenUsage::default()),
                    finish_reason: "stop".to_string(),
                }),
            ],
            _ => return Err(provider_failure()),
        };
        Ok(futures::stream::iter(chunks).boxed())
    }
}

// ── LLM: first call requests denied_http_probe, then terminal ────────

struct DeniedLlm {
    calls: AtomicUsize,
}

impl DeniedLlm {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }

    fn has_denied_result(request: &ToolCompletionRequest) -> bool {
        request.messages.iter().any(|msg| {
            msg.role == Role::Tool && msg.tool_call_id.as_deref() == Some(DENIED_CALL_ID)
        })
    }
}

#[async_trait]
impl LlmProvider for DeniedLlm {
    fn model_name(&self) -> &str {
        "wasm-denied-test"
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
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match call {
            0 => Ok(ToolCompletionResponse {
                content: None,
                tool_calls: vec![ToolCall {
                    id: DENIED_CALL_ID.to_string(),
                    name: ECHO_TOOL_NAME.to_string(),
                    arguments: serde_json::json!({
                        "action": "denied_http_probe"
                    }),
                    reasoning: None,
                }],
                input_tokens: 1,
                output_tokens: 1,
                finish_reason: FinishReason::ToolUse,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            }),
            1 if Self::has_denied_result(&request) => Ok(ToolCompletionResponse {
                content: Some(format!("```repl\nFINAL('{DENIED_TERMINAL}')\n```")),
                tool_calls: Vec::new(),
                input_tokens: 1,
                output_tokens: 1,
                finish_reason: FinishReason::Stop,
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
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let has_result = Self::has_denied_result(&request);
        let chunks: Vec<Result<LlmStreamChunk, LlmError>> = match call {
            0 => vec![
                Ok(LlmStreamChunk::ToolCallDelta {
                    index: 0,
                    id: Some(DENIED_CALL_ID.to_string()),
                    name: Some(ECHO_TOOL_NAME.to_string()),
                    args_delta: serde_json::json!({
                        "action": "denied_http_probe"
                    })
                    .to_string(),
                }),
                Ok(LlmStreamChunk::Done {
                    usage: Some(TokenUsage::default()),
                    finish_reason: "tool_calls".to_string(),
                }),
            ],
            1 if has_result => vec![
                Ok(LlmStreamChunk::TextDelta(format!(
                    "```repl\nFINAL('{DENIED_TERMINAL}')\n```"
                ))),
                Ok(LlmStreamChunk::Done {
                    usage: Some(TokenUsage::default()),
                    finish_reason: "stop".to_string(),
                }),
            ],
            _ => return Err(provider_failure()),
        };
        Ok(futures::stream::iter(chunks).boxed())
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

fn gateway_message(thread_id: Uuid, content: &str) -> IncomingMessage {
    IncomingMessage::new("gateway", "test-user", content)
        .with_thread(thread_id.to_string())
        .with_metadata(serde_json::json!({
            "thread_id": thread_id,
            "user_id": "test-user",
        }))
}

// ── Tests ────────────────────────────────────────────────────────────

#[tokio::test]
async fn engine_v2_wasm_echo_executes_through_wasmtime() {
    assert_artifact_exists();

    let env = EngineV2EnvGuard::enable(Some("gateway"));
    lunarwing::bridge::reset_engine_state().await;

    let provider = Arc::new(EchoLlm::new());
    let llm: Arc<dyn LlmProvider> = provider.clone();
    let rig = TestRigBuilder::new()
        .with_channel_name("gateway")
        .with_llm(llm)
        .with_wasm_tool(
            ECHO_TOOL_NAME,
            WASM_ARTIFACT,
            Some(std::path::PathBuf::from(CAPABILITIES_PATH)),
        )
        .build()
        .await;

    let thread_id = Uuid::new_v4();
    rig.send_incoming(gateway_message(thread_id, "run echo"))
        .await;

    let responses = rig.wait_for_responses(1, Duration::from_secs(15)).await;
    assert_eq!(responses.len(), 1, "exactly one terminal response expected");
    assert_eq!(responses[0].content, TERMINAL_RESPONSE);

    // Verify tool was executed (name includes action suffix in status events).
    let completed = rig.tool_calls_completed();
    assert!(
        completed
            .iter()
            .any(|(name, success)| name.starts_with(ECHO_TOOL_NAME) && *success),
        "echo tool should be completed successfully; completed={completed:?}"
    );

    // The tool result (output JSON) is visible in ToolResult or the
    // ToolCompleted error/parameters field. Check both paths.
    let tool_results = rig.tool_results();
    let all_tool_data: String = tool_results
        .iter()
        .filter(|(name, _)| name.starts_with(ECHO_TOOL_NAME))
        .map(|(_, preview)| preview.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    // Also check ToolCompleted events for tools that only emit those.
    let completed_data: String = rig
        .captured_status_events()
        .iter()
        .filter_map(|s| match s {
            lunarwing::channels::StatusUpdate::ToolCompleted {
                name,
                success: true,
                parameters: Some(params),
                ..
            } if name.starts_with(ECHO_TOOL_NAME) => Some(params.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let combined = format!("{all_tool_data}\n{completed_data}");
    assert!(
        combined.contains(SOURCE_MARKER),
        "echo result must contain source marker; got combined: {combined}"
    );

    // Verify exactly one assistant message persisted.
    let conversation = rig
        .database()
        .get_or_create_scoped_conversation("gateway", "test-user", &thread_id.to_string())
        .await
        .expect("conversation should resolve");
    let messages = rig
        .database()
        .list_conversation_messages(conversation)
        .await
        .expect("history should load");
    let assistant_count = messages.iter().filter(|m| m.role == "assistant").count();
    assert_eq!(assistant_count, 1, "exactly one assistant message expected");

    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);

    rig.shutdown_and_wait().await;
    env.cleanup().await;
}

#[tokio::test]
async fn engine_v2_wasm_denied_capability_rejects_http_without_connection() {
    assert_artifact_exists();

    let env = EngineV2EnvGuard::enable(Some("gateway"));
    lunarwing::bridge::reset_engine_state().await;

    let provider = Arc::new(DeniedLlm::new());
    let llm: Arc<dyn LlmProvider> = provider.clone();
    let rig = TestRigBuilder::new()
        .with_channel_name("gateway")
        .with_llm(llm)
        .with_wasm_tool(
            ECHO_TOOL_NAME,
            WASM_ARTIFACT,
            Some(std::path::PathBuf::from(CAPABILITIES_PATH)),
        )
        .build()
        .await;

    let thread_id = Uuid::new_v4();
    rig.send_incoming(gateway_message(thread_id, "run denied probe"))
        .await;

    let responses = rig.wait_for_responses(1, Duration::from_secs(15)).await;
    assert_eq!(
        responses.len(),
        1,
        "exactly one terminal response expected (no duplicate delivery)"
    );
    assert_eq!(responses[0].content, DENIED_TERMINAL);

    // Collect all tool-related data for the denied_http_probe action.
    let tool_results = rig.tool_results();
    let all_tool_data: String = tool_results
        .iter()
        .filter(|(name, _)| name.starts_with(ECHO_TOOL_NAME))
        .map(|(_, preview)| preview.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let completed_data: String = rig
        .captured_status_events()
        .iter()
        .filter_map(|s| match s {
            lunarwing::channels::StatusUpdate::ToolCompleted {
                name,
                error: Some(err),
                ..
            } if name.starts_with(ECHO_TOOL_NAME) => Some(err.as_str()),
            lunarwing::channels::StatusUpdate::ToolCompleted {
                name,
                parameters: Some(params),
                ..
            } if name.starts_with(ECHO_TOOL_NAME) => Some(params.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let combined = format!("{all_tool_data}\n{completed_data}");
    assert!(
        !combined.is_empty(),
        "denied_http_probe result should be captured somewhere; \
         statuses={:?}, results={tool_results:?}",
        rig.captured_status_events()
    );

    let result_lower = combined.to_lowercase();
    assert!(
        result_lower.contains("not allowed")
            || result_lower.contains("denied")
            || result_lower.contains("not granted")
            || result_lower.contains("http request not allowed"),
        "denied_http_probe result must indicate denial; got: {combined}"
    );

    let sentinel = "secret_token_should_not_appear";
    assert!(
        !combined.contains(sentinel),
        "no credential sentinel should appear in denied output"
    );

    // Verify exactly one terminal response (no duplicate delivery).
    assert_eq!(rig.captured_responses().len(), 1);

    rig.shutdown_and_wait().await;
    env.cleanup().await;
}
