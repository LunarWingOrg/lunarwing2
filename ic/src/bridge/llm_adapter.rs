//! LLM bridge adapter — wraps `LlmProvider` as `lunarwing_engine::LlmBackend`.

use std::sync::Arc;

use base64::Engine;
use futures::StreamExt;
use lunarwing_engine::{
    ActionDef, EngineError, LlmBackend, LlmCallConfig, LlmOutput, LlmResponse, ThreadMessage,
    TokenUsage, TransientContentPart,
};

use crate::llm::{
    ChatMessage, CompletionRequest, CompletionResponse, ContentPart, ImageUrl, LlmError,
    LlmProvider, Role, ToolCall, ToolCompletionRequest, ToolCompletionResponse, ToolDefinition,
};

/// Wraps an existing `LlmProvider` to implement the engine's `LlmBackend` trait.
pub struct LlmBridgeAdapter {
    provider: Arc<dyn LlmProvider>,
    /// Optional cheaper provider for sub-calls (depth > 0).
    cheap_provider: Option<Arc<dyn LlmProvider>>,
}

impl LlmBridgeAdapter {
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        cheap_provider: Option<Arc<dyn LlmProvider>>,
    ) -> Self {
        Self {
            provider,
            cheap_provider,
        }
    }

    fn provider_for_depth(&self, depth: u32) -> &Arc<dyn LlmProvider> {
        if depth > 0 {
            self.cheap_provider.as_ref().unwrap_or(&self.provider)
        } else {
            &self.provider
        }
    }
}

enum BridgeRequest {
    Plain(CompletionRequest),
    Tools(ToolCompletionRequest),
}

fn build_request(
    messages: &[ThreadMessage],
    actions: &[ActionDef],
    config: &LlmCallConfig,
) -> BridgeRequest {
    let chat_messages = messages.iter().map(thread_msg_to_chat).collect();
    let tools: Vec<ToolDefinition> = if config.force_text {
        Vec::new()
    } else {
        actions.iter().map(action_def_to_tool_def).collect()
    };
    let max_tokens = config.max_tokens.unwrap_or(4096);

    if tools.is_empty() {
        let mut request = CompletionRequest::new(chat_messages).with_max_tokens(max_tokens);
        if let Some(temperature) = config.temperature {
            request = request.with_temperature(temperature);
        }
        request.metadata = config.metadata.clone();
        BridgeRequest::Plain(request)
    } else {
        let mut request = ToolCompletionRequest::new(chat_messages, tools)
            .with_max_tokens(max_tokens)
            .with_tool_choice("auto");
        if let Some(temperature) = config.temperature {
            request = request.with_temperature(temperature);
        }
        request.metadata = config.metadata.clone();
        BridgeRequest::Tools(request)
    }
}

fn map_provider_error(error: LlmError) -> EngineError {
    EngineError::Llm {
        reason: error.to_string(),
    }
}

fn map_usage(usage: crate::llm::TokenUsage) -> TokenUsage {
    map_usage_fields(
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_read_input_tokens,
        usage.cache_creation_input_tokens,
    )
}

fn map_usage_fields(
    input_tokens: u32,
    output_tokens: u32,
    cache_read_tokens: u32,
    cache_write_tokens: u32,
) -> TokenUsage {
    TokenUsage {
        input_tokens: u64::from(input_tokens),
        output_tokens: u64::from(output_tokens),
        cache_read_tokens: u64::from(cache_read_tokens),
        cache_write_tokens: u64::from(cache_write_tokens),
        cost_usd: 0.0,
    }
}

fn map_stream_chunk(chunk: crate::llm::LlmStreamChunk) -> lunarwing_engine::LlmStreamChunk {
    match chunk {
        crate::llm::LlmStreamChunk::TextDelta(text) => {
            lunarwing_engine::LlmStreamChunk::TextDelta(text)
        }
        crate::llm::LlmStreamChunk::ToolCallDelta {
            index,
            id,
            name,
            args_delta,
        } => lunarwing_engine::LlmStreamChunk::ToolCallDelta {
            index,
            id,
            name,
            args_delta,
        },
        crate::llm::LlmStreamChunk::Done {
            usage,
            finish_reason,
        } => lunarwing_engine::LlmStreamChunk::Done {
            usage: usage.map(map_usage),
            finish_reason,
        },
    }
}

fn map_completion_response(response: CompletionResponse) -> LlmOutput {
    LlmOutput {
        response: LlmResponse::from_text(response.content),
        usage: map_usage_fields(
            response.input_tokens,
            response.output_tokens,
            response.cache_read_input_tokens,
            response.cache_creation_input_tokens,
        ),
    }
}

fn map_tool_response(response: ToolCompletionResponse) -> LlmOutput {
    let usage = map_usage_fields(
        response.input_tokens,
        response.output_tokens,
        response.cache_read_input_tokens,
        response.cache_creation_input_tokens,
    );

    let llm_response = if response.tool_calls.is_empty() {
        LlmResponse::from_text(response.content.unwrap_or_default())
    } else {
        LlmResponse::ActionCalls {
            calls: response
                .tool_calls
                .into_iter()
                .map(|call| lunarwing_engine::ActionCall {
                    id: call.id,
                    action_name: call.name,
                    parameters: call.arguments,
                })
                .collect(),
            content: response.content,
        }
    };

    LlmOutput {
        response: llm_response,
        usage,
    }
}

#[async_trait::async_trait]
impl LlmBackend for LlmBridgeAdapter {
    async fn complete(
        &self,
        messages: &[ThreadMessage],
        actions: &[ActionDef],
        config: &LlmCallConfig,
    ) -> Result<LlmOutput, EngineError> {
        let provider = self.provider_for_depth(config.depth);

        match build_request(messages, actions, config) {
            BridgeRequest::Plain(request) => provider
                .complete(request)
                .await
                .map(map_completion_response)
                .map_err(map_provider_error),
            BridgeRequest::Tools(request) => provider
                .complete_with_tools(request)
                .await
                .map(map_tool_response)
                .map_err(map_provider_error),
        }
    }

    async fn complete_stream<'a>(
        &'a self,
        messages: &[ThreadMessage],
        actions: &[ActionDef],
        config: &LlmCallConfig,
    ) -> Result<lunarwing_engine::LlmStream<'a>, EngineError> {
        let provider = self.provider_for_depth(config.depth);
        let stream = match build_request(messages, actions, config) {
            BridgeRequest::Plain(request) => provider.complete_stream(request).await,
            BridgeRequest::Tools(request) => provider.complete_with_tools_stream(request).await,
        }
        .map_err(map_provider_error)?;

        Ok(stream
            .map(|item| item.map(map_stream_chunk).map_err(map_provider_error))
            .boxed())
    }

    fn model_name(&self) -> &str {
        self.provider.model_name()
    }
}

// ── Conversion helpers ──────────────────────────────────────

fn thread_msg_to_chat(msg: &ThreadMessage) -> ChatMessage {
    use lunarwing_engine::MessageRole;

    let role = match msg.role {
        MessageRole::System => Role::System,
        MessageRole::User => Role::User,
        MessageRole::Assistant => Role::Assistant,
        MessageRole::ActionResult => Role::Tool,
    };

    let content_parts = if role == Role::User {
        msg.transient_content_parts
            .iter()
            .map(|part| match part {
                TransientContentPart::Image { mime_type, data } => ContentPart::ImageUrl {
                    image_url: ImageUrl {
                        url: format!(
                            "data:{mime_type};base64,{}",
                            base64::engine::general_purpose::STANDARD.encode(data)
                        ),
                        detail: None,
                    },
                },
            })
            .collect()
    } else {
        Vec::new()
    };

    let mut chat = ChatMessage {
        role,
        content: msg.content.clone(),
        content_parts,
        tool_call_id: msg.action_call_id.clone(),
        name: msg.action_name.clone(),
        tool_calls: None,
    };

    // Convert action calls if present (assistant message with tool calls)
    if let Some(ref calls) = msg.action_calls {
        chat.tool_calls = Some(
            calls
                .iter()
                .map(|c| ToolCall {
                    id: c.id.clone(),
                    name: c.action_name.clone(),
                    arguments: c.parameters.clone(),
                    reasoning: None,
                })
                .collect(),
        );
    }

    chat
}

fn action_def_to_tool_def(action: &ActionDef) -> ToolDefinition {
    ToolDefinition {
        name: action.name.clone(),
        description: action.description.clone(),
        parameters: action.parameters_schema.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use futures::StreamExt;
    use lunarwing_engine::LlmStreamChunk as EngineChunk;
    use rust_decimal::Decimal;

    use super::*;
    use crate::llm::streaming_test_support::{ScriptedStreamingProvider, StreamScript};
    use crate::llm::{
        CompletionRequest, CompletionResponse, FinishReason, LlmError, LlmStreamChunk as HostChunk,
        TokenUsage as HostUsage, ToolCompletionResponse,
    };

    fn search_action() -> ActionDef {
        ActionDef {
            name: "search".to_string(),
            description: "Search".to_string(),
            parameters_schema: serde_json::json!({"type": "object"}),
            effects: Vec::new(),
            requires_approval: false,
        }
    }

    #[test]
    fn thread_message_maps_transient_image_at_provider_boundary() {
        let message = ThreadMessage::user_with_transient_parts(
            "inspect image",
            vec![TransientContentPart::Image {
                mime_type: "image/png".to_string(),
                data: vec![1, 2, 3],
            }],
        );

        let chat = thread_msg_to_chat(&message);
        assert_eq!(chat.content, "inspect image");
        assert_eq!(chat.content_parts.len(), 1);
        match &chat.content_parts[0] {
            ContentPart::ImageUrl { image_url } => {
                assert_eq!(image_url.url, "data:image/png;base64,AQID");
                assert_eq!(image_url.detail, None);
            }
            other => panic!("expected image URL content, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn complete_stream_maps_plain_host_chunks_and_usage() {
        let provider = Arc::new(ScriptedStreamingProvider::new(
            "primary",
            vec![StreamScript::Items(vec![
                Ok(HostChunk::TextDelta("hel".into())),
                Ok(HostChunk::TextDelta("lo".into())),
                Ok(HostChunk::Done {
                    usage: Some(HostUsage {
                        input_tokens: 8,
                        output_tokens: 3,
                        cache_read_input_tokens: 2,
                        cache_creation_input_tokens: 1,
                    }),
                    finish_reason: "stop".into(),
                }),
            ])],
            vec![],
        ));
        let adapter = LlmBridgeAdapter::new(provider.clone(), None);

        let chunks = adapter
            .complete_stream(&[], &[], &LlmCallConfig::default())
            .await
            .expect("plain stream should open")
            .collect::<Vec<_>>()
            .await;

        assert_eq!(provider.plain_calls(), 1);
        assert!(matches!(&chunks[0], Ok(EngineChunk::TextDelta(text)) if text == "hel"));
        assert!(matches!(&chunks[1], Ok(EngineChunk::TextDelta(text)) if text == "lo"));
        match &chunks[2] {
            Ok(EngineChunk::Done {
                usage: Some(usage),
                finish_reason,
            }) => {
                assert_eq!(usage.input_tokens, 8);
                assert_eq!(usage.output_tokens, 3);
                assert_eq!(usage.cache_read_tokens, 2);
                assert_eq!(usage.cache_write_tokens, 1);
                assert_eq!(usage.cost_usd, 0.0);
                assert_eq!(finish_reason, "stop");
            }
            other => panic!("expected mapped terminal usage, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn complete_stream_maps_fragmented_tool_chunks() {
        let provider = Arc::new(ScriptedStreamingProvider::new(
            "primary",
            vec![],
            vec![StreamScript::Items(vec![
                Ok(HostChunk::ToolCallDelta {
                    index: 0,
                    id: Some("call-1".into()),
                    name: Some("search".into()),
                    args_delta: "{\"q\":\"".into(),
                }),
                Ok(HostChunk::ToolCallDelta {
                    index: 0,
                    id: None,
                    name: None,
                    args_delta: "rust\"}".into(),
                }),
                Ok(HostChunk::Done {
                    usage: None,
                    finish_reason: "tool_calls".into(),
                }),
            ])],
        ));
        let adapter = LlmBridgeAdapter::new(provider.clone(), None);

        let chunks = adapter
            .complete_stream(&[], &[search_action()], &LlmCallConfig::default())
            .await
            .expect("tool stream should open")
            .collect::<Vec<_>>()
            .await;

        assert_eq!(provider.tool_calls(), 1);
        assert!(matches!(
            &chunks[0],
            Ok(EngineChunk::ToolCallDelta {
                index: 0,
                id: Some(id),
                name: Some(name),
                args_delta,
            }) if id == "call-1" && name == "search" && args_delta == "{\"q\":\""
        ));
        assert!(matches!(
            &chunks[1],
            Ok(EngineChunk::ToolCallDelta { args_delta, .. }) if args_delta == "rust\"}"
        ));
    }

    #[tokio::test]
    async fn complete_stream_uses_cheap_provider_at_nonzero_depth() {
        let primary = Arc::new(ScriptedStreamingProvider::new("primary", vec![], vec![]));
        let cheap = Arc::new(ScriptedStreamingProvider::new(
            "cheap",
            vec![StreamScript::Items(vec![
                Ok(HostChunk::TextDelta("cheap".into())),
                Ok(HostChunk::Done {
                    usage: None,
                    finish_reason: "stop".into(),
                }),
            ])],
            vec![],
        ));
        let adapter = LlmBridgeAdapter::new(primary.clone(), Some(cheap.clone()));
        let config = LlmCallConfig {
            depth: 1,
            ..LlmCallConfig::default()
        };

        let chunks = adapter
            .complete_stream(&[], &[], &config)
            .await
            .expect("cheap stream should open")
            .collect::<Vec<_>>()
            .await;

        assert_eq!(primary.plain_calls(), 0);
        assert_eq!(cheap.plain_calls(), 1);
        assert!(matches!(&chunks[0], Ok(EngineChunk::TextDelta(text)) if text == "cheap"));
    }

    #[tokio::test]
    async fn complete_stream_maps_setup_error() {
        let provider = Arc::new(ScriptedStreamingProvider::new(
            "primary",
            vec![StreamScript::SetupError(LlmError::RequestFailed {
                provider: "primary".into(),
                reason: "setup failed".into(),
            })],
            vec![],
        ));
        let adapter = LlmBridgeAdapter::new(provider, None);

        let error = match adapter
            .complete_stream(&[], &[], &LlmCallConfig::default())
            .await
        {
            Ok(_) => panic!("setup error should cross the bridge"),
            Err(error) => error,
        };

        assert!(matches!(error, EngineError::Llm { reason } if reason.contains("setup failed")));
    }

    #[tokio::test]
    async fn complete_stream_maps_item_error() {
        let provider = Arc::new(ScriptedStreamingProvider::new(
            "primary",
            vec![StreamScript::Items(vec![Err(LlmError::RequestFailed {
                provider: "primary".into(),
                reason: "item failed".into(),
            })])],
            vec![],
        ));
        let adapter = LlmBridgeAdapter::new(provider, None);
        let items = adapter
            .complete_stream(&[], &[], &LlmCallConfig::default())
            .await
            .expect("stream should open")
            .collect::<Vec<_>>()
            .await;

        assert!(matches!(
            items.first(),
            Some(Err(EngineError::Llm { reason })) if reason.contains("item failed")
        ));
    }

    #[derive(Default)]
    struct CapturingProvider {
        plain_requests: Mutex<Vec<CompletionRequest>>,
        tool_requests: Mutex<Vec<ToolCompletionRequest>>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for CapturingProvider {
        fn model_name(&self) -> &str {
            "capturing"
        }

        fn cost_per_token(&self) -> (Decimal, Decimal) {
            (Decimal::ZERO, Decimal::ZERO)
        }

        async fn complete(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse, LlmError> {
            self.plain_requests
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(request);
            Ok(CompletionResponse {
                content: "plain".to_string(),
                input_tokens: 1,
                output_tokens: 1,
                finish_reason: FinishReason::Stop,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            })
        }

        async fn complete_with_tools(
            &self,
            request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, LlmError> {
            self.tool_requests
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(request);
            Ok(ToolCompletionResponse {
                content: Some("tools".to_string()),
                tool_calls: Vec::new(),
                input_tokens: 1,
                output_tokens: 1,
                finish_reason: FinishReason::Stop,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            })
        }
    }

    #[tokio::test]
    async fn stream_request_preserves_plain_config_and_metadata() {
        let provider = Arc::new(CapturingProvider::default());
        let adapter = LlmBridgeAdapter::new(provider.clone(), None);
        let config = LlmCallConfig {
            max_tokens: Some(1234),
            temperature: Some(0.25),
            force_text: true,
            depth: 0,
            metadata: [("thread_id".to_string(), "thread-1".to_string())]
                .into_iter()
                .collect(),
        };

        adapter
            .complete_stream(&[ThreadMessage::user("hello")], &[search_action()], &config)
            .await
            .expect("plain stream should open")
            .collect::<Vec<_>>()
            .await;

        let requests = provider.plain_requests.lock().expect("plain request lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].max_tokens, Some(1234));
        assert_eq!(requests[0].temperature, Some(0.25));
        assert_eq!(
            requests[0].metadata.get("thread_id").map(String::as_str),
            Some("thread-1")
        );
        assert!(
            provider
                .tool_requests
                .lock()
                .expect("tool request lock")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn stream_request_preserves_tool_config_and_choice() {
        let provider = Arc::new(CapturingProvider::default());
        let adapter = LlmBridgeAdapter::new(provider.clone(), None);
        let config = LlmCallConfig {
            max_tokens: Some(2048),
            temperature: Some(0.5),
            metadata: [("trace".to_string(), "abc".to_string())]
                .into_iter()
                .collect(),
            ..LlmCallConfig::default()
        };

        adapter
            .complete_stream(
                &[ThreadMessage::user("search")],
                &[search_action()],
                &config,
            )
            .await
            .expect("tool stream should open")
            .collect::<Vec<_>>()
            .await;

        let requests = provider.tool_requests.lock().expect("tool request lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].max_tokens, Some(2048));
        assert_eq!(requests[0].temperature, Some(0.5));
        assert_eq!(requests[0].tool_choice.as_deref(), Some("auto"));
        assert_eq!(
            requests[0].metadata.get("trace").map(String::as_str),
            Some("abc")
        );
    }

    #[tokio::test]
    async fn stream_request_uses_blocking_defaults() {
        let provider = Arc::new(CapturingProvider::default());
        let adapter = LlmBridgeAdapter::new(provider.clone(), None);

        adapter
            .complete_stream(
                &[ThreadMessage::user("hello")],
                &[],
                &LlmCallConfig::default(),
            )
            .await
            .expect("default plain stream should open")
            .collect::<Vec<_>>()
            .await;

        let requests = provider.plain_requests.lock().expect("plain request lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].max_tokens, Some(4096));
        assert_eq!(requests[0].temperature, None);
    }
}
