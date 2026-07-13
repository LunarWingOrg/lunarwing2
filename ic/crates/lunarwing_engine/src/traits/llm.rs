//! LLM backend trait.
//!
//! The engine's abstraction over language model providers. Deliberately
//! simpler than the main crate's `LlmProvider` — the engine only needs
//! to make completion calls. Cost tracking, caching, retry, and circuit
//! breaking are host concerns handled by the bridge adapter.

use std::collections::HashMap;

use futures::StreamExt;
use futures::stream::{self, BoxStream};

use crate::types::capability::ActionDef;
use crate::types::error::EngineError;
use crate::types::message::ThreadMessage;
use crate::types::step::{LlmResponse, TokenUsage};

/// Configuration for a single LLM call.
#[derive(Debug, Clone, Default)]
pub struct LlmCallConfig {
    /// Maximum tokens to generate.
    pub max_tokens: Option<u32>,
    /// Sampling temperature.
    pub temperature: Option<f32>,
    /// When true, the LLM should not return action calls.
    pub force_text: bool,
    /// Depth in the recursive call tree (0 = root, 1+ = sub-call).
    /// Implementations can use this to route to cheaper models for sub-calls.
    pub depth: u32,
    /// Opaque metadata forwarded to the LLM provider.
    pub metadata: HashMap<String, String>,
}

/// Output from a single LLM call.
#[derive(Debug, Clone)]
pub struct LlmOutput {
    pub response: LlmResponse,
    pub usage: TokenUsage,
}

/// Incremental output from an engine LLM completion stream.
#[derive(Debug, Clone, PartialEq)]
pub enum LlmStreamChunk {
    /// Incremental assistant text.
    TextDelta(String),
    /// Incremental action call data.
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        args_delta: String,
    },
    /// Terminal stream metadata. The stream ends after this chunk.
    Done {
        usage: Option<TokenUsage>,
        finish_reason: String,
    },
}

/// A boxed engine stream that can be consumed across async task boundaries.
pub type LlmStream<'a> = BoxStream<'a, Result<LlmStreamChunk, EngineError>>;

fn fallback_stream_chunks(output: LlmOutput) -> Vec<Result<LlmStreamChunk, EngineError>> {
    let mut chunks = Vec::new();
    let finish_reason = match output.response {
        LlmResponse::Text(text) => {
            chunks.push(Ok(LlmStreamChunk::TextDelta(text)));
            "unknown"
        }
        LlmResponse::Code { code, content } => {
            let text = content.unwrap_or_else(|| format!("```python\n{code}\n```"));
            chunks.push(Ok(LlmStreamChunk::TextDelta(text)));
            "unknown"
        }
        LlmResponse::ActionCalls { calls, content } => {
            if let Some(content) = content.filter(|content| !content.is_empty()) {
                chunks.push(Ok(LlmStreamChunk::TextDelta(content)));
            }
            for (index, call) in calls.into_iter().enumerate() {
                chunks.push(Ok(LlmStreamChunk::ToolCallDelta {
                    index,
                    id: Some(call.id),
                    name: Some(call.action_name),
                    args_delta: call.parameters.to_string(),
                }));
            }
            "tool_calls"
        }
    };
    chunks.push(Ok(LlmStreamChunk::Done {
        usage: Some(output.usage),
        finish_reason: finish_reason.to_string(),
    }));
    chunks
}

/// Abstraction over language model providers.
///
/// The main crate implements this by wrapping its `LlmProvider` trait,
/// converting between `ThreadMessage` and `ChatMessage`.
#[async_trait::async_trait]
pub trait LlmBackend: Send + Sync {
    /// Call the LLM with conversation messages and available action definitions.
    ///
    /// Returns either a text response or a set of action calls.
    async fn complete(
        &self,
        messages: &[ThreadMessage],
        actions: &[ActionDef],
        config: &LlmCallConfig,
    ) -> Result<LlmOutput, EngineError>;

    /// Call the LLM as an incremental stream.
    ///
    /// Backends that do not support native streaming inherit a safe fallback
    /// that performs the existing blocking completion and converts its output
    /// into a finite stream of typed chunks.
    async fn complete_stream<'a>(
        &'a self,
        messages: &[ThreadMessage],
        actions: &[ActionDef],
        config: &LlmCallConfig,
    ) -> Result<LlmStream<'a>, EngineError> {
        let output = self.complete(messages, actions, config).await?;
        Ok(stream::iter(fallback_stream_chunks(output)).boxed())
    }

    /// The model identifier (e.g. "gpt-4", "claude-opus-4-20250514").
    fn model_name(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::sync::Arc;

    struct NonStreamingBackend {
        response: LlmResponse,
    }

    impl NonStreamingBackend {
        fn new(response: LlmResponse) -> Self {
            Self { response }
        }
    }

    #[async_trait::async_trait]
    impl LlmBackend for NonStreamingBackend {
        async fn complete(
            &self,
            _messages: &[ThreadMessage],
            _actions: &[ActionDef],
            _config: &LlmCallConfig,
        ) -> Result<LlmOutput, EngineError> {
            Ok(LlmOutput {
                response: self.response.clone(),
                usage: TokenUsage {
                    input_tokens: 3,
                    output_tokens: 2,
                    ..TokenUsage::default()
                },
            })
        }

        fn model_name(&self) -> &str {
            "test-model"
        }
    }

    #[tokio::test]
    async fn non_streaming_backend_falls_back_to_single_text_delta() {
        let backend: Arc<dyn LlmBackend> = Arc::new(NonStreamingBackend::new(LlmResponse::Text(
            "hello".to_string(),
        )));
        let chunks = backend
            .complete_stream(&[], &[], &LlmCallConfig::default())
            .await
            .expect("fallback stream should be created")
            .collect::<Vec<_>>()
            .await;

        assert_eq!(chunks.len(), 2);
        assert!(matches!(&chunks[0], Ok(LlmStreamChunk::TextDelta(text)) if text == "hello"));
        match &chunks[1] {
            Ok(LlmStreamChunk::Done {
                usage: Some(usage),
                finish_reason,
            }) => {
                assert_eq!(usage.input_tokens, 3);
                assert_eq!(usage.output_tokens, 2);
                assert_eq!(finish_reason, "unknown");
            }
            other => panic!("expected terminal chunk, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn code_fallback_streams_original_response_text() {
        let backend: Arc<dyn LlmBackend> = Arc::new(NonStreamingBackend::new(LlmResponse::Code {
            code: "print('hello')".to_string(),
            content: Some("```python\nprint('hello')\n```".to_string()),
        }));

        let chunks = backend
            .complete_stream(&[], &[], &LlmCallConfig::default())
            .await
            .expect("fallback stream should be created")
            .collect::<Vec<_>>()
            .await;

        assert!(matches!(
            &chunks[0],
            Ok(LlmStreamChunk::TextDelta(text))
                if text == "```python\nprint('hello')\n```"
        ));
        assert!(matches!(
            &chunks[1],
            Ok(LlmStreamChunk::Done { finish_reason, .. }) if finish_reason == "unknown"
        ));
    }

    #[tokio::test]
    async fn code_fallback_without_content_preserves_code_fence() {
        let backend: Arc<dyn LlmBackend> = Arc::new(NonStreamingBackend::new(LlmResponse::Code {
            code: "print('hello')".to_string(),
            content: None,
        }));

        let chunks = backend
            .complete_stream(&[], &[], &LlmCallConfig::default())
            .await
            .expect("fallback stream should be created")
            .collect::<Vec<_>>()
            .await;

        assert!(matches!(
            &chunks[0],
            Ok(LlmStreamChunk::TextDelta(text))
                if text == "```python\nprint('hello')\n```"
        ));
    }

    #[tokio::test]
    async fn action_fallback_streams_content_and_complete_call_deltas() {
        let backend: Arc<dyn LlmBackend> =
            Arc::new(NonStreamingBackend::new(LlmResponse::ActionCalls {
                calls: vec![crate::types::step::ActionCall {
                    id: "call-1".to_string(),
                    action_name: "search".to_string(),
                    parameters: serde_json::json!({"query": "lunarwing"}),
                }],
                content: Some("I will search.".to_string()),
            }));

        let chunks = backend
            .complete_stream(&[], &[], &LlmCallConfig::default())
            .await
            .expect("fallback stream should be created")
            .collect::<Vec<_>>()
            .await;

        assert!(matches!(
            &chunks[0],
            Ok(LlmStreamChunk::TextDelta(text)) if text == "I will search."
        ));
        assert!(matches!(
            &chunks[1],
            Ok(LlmStreamChunk::ToolCallDelta {
                index: 0,
                id: Some(id),
                name: Some(name),
                args_delta,
            }) if id == "call-1"
                && name == "search"
                && args_delta == "{\"query\":\"lunarwing\"}"
        ));
        assert!(matches!(
            &chunks[2],
            Ok(LlmStreamChunk::Done { finish_reason, .. }) if finish_reason == "tool_calls"
        ));
    }
}
