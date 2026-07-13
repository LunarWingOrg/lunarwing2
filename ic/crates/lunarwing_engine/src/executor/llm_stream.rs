use std::collections::BTreeMap;

use futures::StreamExt;

use crate::traits::llm::{LlmOutput, LlmStream, LlmStreamChunk};
use crate::types::error::EngineError;
use crate::types::step::{ActionCall, LlmResponse, TokenUsage};

#[derive(Default)]
struct StreamAccumulator {
    text: String,
    tool_calls: BTreeMap<usize, ToolCallAccumulator>,
    usage: TokenUsage,
    saw_done: bool,
}

#[derive(Default)]
struct ToolCallAccumulator {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

pub(super) async fn collect_llm_stream(
    mut stream: LlmStream<'_>,
    mut on_text_delta: impl FnMut(&str),
) -> Result<LlmOutput, EngineError> {
    let mut accumulator = StreamAccumulator::default();
    while let Some(item) = stream.next().await {
        accumulator.push(item?, &mut on_text_delta)?;
    }
    accumulator.finish()
}

impl StreamAccumulator {
    fn push(
        &mut self,
        chunk: LlmStreamChunk,
        on_text_delta: &mut impl FnMut(&str),
    ) -> Result<(), EngineError> {
        if self.saw_done {
            return Err(llm_error("LLM stream emitted a chunk after Done"));
        }

        match chunk {
            LlmStreamChunk::TextDelta(delta) => {
                if !delta.is_empty() {
                    on_text_delta(&delta);
                }
                self.text.push_str(&delta);
            }
            LlmStreamChunk::ToolCallDelta {
                index,
                id,
                name,
                args_delta,
            } => {
                let call = self.tool_calls.entry(index).or_default();
                merge_identity(&mut call.id, id, "id", index)?;
                merge_identity(&mut call.name, name, "name", index)?;
                call.arguments.push_str(&args_delta);
            }
            LlmStreamChunk::Done {
                usage,
                finish_reason: _,
            } => {
                self.usage = usage.unwrap_or_default();
                self.saw_done = true;
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<LlmOutput, EngineError> {
        if !self.saw_done {
            return Err(llm_error("LLM stream ended before Done"));
        }

        let response = if self.tool_calls.is_empty() {
            LlmResponse::from_text(self.text)
        } else {
            let mut calls = Vec::with_capacity(self.tool_calls.len());
            for (index, call) in self.tool_calls {
                calls.push(call.finish(index)?);
            }
            LlmResponse::ActionCalls {
                calls,
                content: (!self.text.is_empty()).then_some(self.text),
            }
        };

        Ok(LlmOutput {
            response,
            usage: self.usage,
        })
    }
}

impl ToolCallAccumulator {
    fn finish(self, index: usize) -> Result<ActionCall, EngineError> {
        let id = self
            .id
            .ok_or_else(|| llm_error(format!("tool call at index {index} is missing an id")))?;
        let action_name = self
            .name
            .ok_or_else(|| llm_error(format!("tool call at index {index} is missing a name")))?;
        let parameters = serde_json::from_str(&self.arguments).map_err(|error| {
            llm_error(format!(
                "tool call at index {index} has invalid JSON arguments: {error}"
            ))
        })?;
        Ok(ActionCall {
            id,
            action_name,
            parameters,
        })
    }
}

fn merge_identity(
    current: &mut Option<String>,
    incoming: Option<String>,
    field: &str,
    index: usize,
) -> Result<(), EngineError> {
    let Some(value) = incoming.filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    match current {
        None => {
            *current = Some(value);
            Ok(())
        }
        Some(existing) if existing == &value => Ok(()),
        Some(existing) => Err(llm_error(format!(
            "conflicting tool call {field} at index {index}: '{existing}' versus '{value}'"
        ))),
    }
}

fn llm_error(reason: impl Into<String>) -> EngineError {
    EngineError::Llm {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use futures::stream;

    use super::*;
    use crate::traits::llm::{LlmBackend, LlmCallConfig};

    fn done(usage: Option<TokenUsage>) -> Result<LlmStreamChunk, EngineError> {
        Ok(LlmStreamChunk::Done {
            usage,
            finish_reason: "stop".to_string(),
        })
    }

    fn tool(
        index: usize,
        id: Option<&str>,
        name: Option<&str>,
        args_delta: &str,
    ) -> Result<LlmStreamChunk, EngineError> {
        Ok(LlmStreamChunk::ToolCallDelta {
            index,
            id: id.map(str::to_string),
            name: name.map(str::to_string),
            args_delta: args_delta.to_string(),
        })
    }

    async fn collect(
        items: Vec<Result<LlmStreamChunk, EngineError>>,
    ) -> (Result<LlmOutput, EngineError>, Vec<String>) {
        let stream = stream::iter(items).boxed();
        let mut deltas = Vec::new();
        let result = collect_llm_stream(stream, |delta| deltas.push(delta.to_string())).await;
        (result, deltas)
    }

    #[tokio::test]
    async fn collects_text_deltas_and_terminal_usage() {
        let usage = TokenUsage {
            input_tokens: 8,
            output_tokens: 3,
            ..TokenUsage::default()
        };
        let (result, deltas) = collect(vec![
            Ok(LlmStreamChunk::TextDelta("hel".to_string())),
            Ok(LlmStreamChunk::TextDelta(String::new())),
            Ok(LlmStreamChunk::TextDelta("lo".to_string())),
            done(Some(usage)),
        ])
        .await;

        let output = result.expect("valid text stream should collect");
        assert_eq!(deltas, vec!["hel", "lo"]);
        assert_eq!(output.usage, usage);
        assert!(matches!(output.response, LlmResponse::Text(text) if text == "hello"));
    }

    #[tokio::test]
    async fn classifies_collected_code() {
        let (result, _) = collect(vec![
            Ok(LlmStreamChunk::TextDelta(
                "```repl\nFINAL('done')\n```".to_string(),
            )),
            done(None),
        ])
        .await;

        assert!(matches!(
            result.expect("code stream should collect").response,
            LlmResponse::Code { code, .. } if code == "FINAL('done')"
        ));
    }

    #[tokio::test]
    async fn missing_usage_defaults_to_zero() {
        let (result, _) = collect(vec![done(None)]).await;
        assert_eq!(
            result.expect("usage-less terminal should collect").usage,
            TokenUsage::default()
        );
    }

    #[tokio::test]
    async fn rejects_eof_before_done() {
        let (result, _) = collect(vec![Ok(LlmStreamChunk::TextDelta("partial".into()))]).await;
        assert!(
            matches!(result, Err(EngineError::Llm { reason }) if reason.contains("before Done"))
        );
    }

    #[tokio::test]
    async fn rejects_chunk_after_done() {
        let (result, _) = collect(vec![
            done(None),
            Ok(LlmStreamChunk::TextDelta("late".into())),
        ])
        .await;
        assert!(
            matches!(result, Err(EngineError::Llm { reason }) if reason.contains("after Done"))
        );
    }

    #[tokio::test]
    async fn rejects_tool_chunk_after_done() {
        let (result, _) = collect(vec![
            done(None),
            Ok(LlmStreamChunk::ToolCallDelta {
                index: 0,
                id: Some("call-1".to_string()),
                name: Some("search".to_string()),
                args_delta: "{}".to_string(),
            }),
        ])
        .await;
        assert!(
            matches!(result, Err(EngineError::Llm { reason }) if reason.contains("after Done"))
        );
    }

    #[tokio::test]
    async fn rejects_second_done() {
        let (result, _) = collect(vec![done(None), done(None)]).await;
        assert!(
            matches!(result, Err(EngineError::Llm { reason }) if reason.contains("after Done"))
        );
    }

    #[tokio::test]
    async fn propagates_mid_stream_error_after_callback() {
        let (result, deltas) = collect(vec![
            Ok(LlmStreamChunk::TextDelta("visible".into())),
            Err(EngineError::Llm {
                reason: "stream failed".into(),
            }),
        ])
        .await;
        assert_eq!(deltas, vec!["visible"]);
        assert!(matches!(result, Err(EngineError::Llm { reason }) if reason == "stream failed"));
    }

    #[tokio::test]
    async fn reconstructs_fragmented_interleaved_tool_calls_in_index_order() {
        let (result, deltas) = collect(vec![
            Ok(LlmStreamChunk::TextDelta("I will run tools.".into())),
            tool(1, Some("call-2"), Some("second"), "{\"b\":"),
            tool(0, Some("call-1"), Some("first"), "{\"a\":"),
            tool(1, None, None, "2}"),
            tool(0, Some("call-1"), Some("first"), "1}"),
            done(Some(TokenUsage::default())),
        ])
        .await;

        let output = result.expect("fragmented tool stream should collect");
        assert_eq!(deltas, vec!["I will run tools."]);
        let LlmResponse::ActionCalls { calls, content } = output.response else {
            panic!("expected action calls");
        };
        assert_eq!(content.as_deref(), Some("I will run tools."));
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call-1");
        assert_eq!(calls[0].action_name, "first");
        assert_eq!(calls[0].parameters, serde_json::json!({"a": 1}));
        assert_eq!(calls[1].id, "call-2");
        assert_eq!(calls[1].parameters, serde_json::json!({"b": 2}));
    }

    #[tokio::test]
    async fn tool_stream_without_text_has_no_content() {
        let (result, deltas) = collect(vec![
            tool(0, Some("call-1"), Some("search"), "{}"),
            done(None),
        ])
        .await;

        assert!(deltas.is_empty());
        assert!(matches!(
            result.expect("tool stream should collect").response,
            LlmResponse::ActionCalls { content: None, .. }
        ));
    }

    #[tokio::test]
    async fn rejects_conflicting_tool_identity() {
        for items in [
            vec![
                tool(0, Some("call-1"), Some("search"), "{}"),
                tool(0, Some("call-2"), None, ""),
                done(None),
            ],
            vec![
                tool(0, Some("call-1"), Some("search"), "{}"),
                tool(0, None, Some("fetch"), ""),
                done(None),
            ],
        ] {
            let (result, _) = collect(items).await;
            assert!(
                matches!(result, Err(EngineError::Llm { reason }) if reason.contains("conflicting"))
            );
        }
    }

    #[tokio::test]
    async fn rejects_incomplete_or_malformed_tool_calls() {
        let cases = [
            vec![tool(0, None, Some("search"), "{}"), done(None)],
            vec![tool(0, Some("call-1"), None, "{}"), done(None)],
            vec![
                tool(0, Some("call-1"), Some("search"), "not-json"),
                done(None),
            ],
        ];

        for items in cases {
            let (result, _) = collect(items).await;
            assert!(matches!(result, Err(EngineError::Llm { .. })));
        }
    }

    struct CompleteOnlyBackend;

    #[async_trait::async_trait]
    impl LlmBackend for CompleteOnlyBackend {
        async fn complete(
            &self,
            _messages: &[crate::types::message::ThreadMessage],
            _actions: &[crate::types::capability::ActionDef],
            _config: &LlmCallConfig,
        ) -> Result<LlmOutput, EngineError> {
            Ok(LlmOutput {
                response: LlmResponse::Text("fallback text".to_string()),
                usage: TokenUsage {
                    input_tokens: 5,
                    output_tokens: 2,
                    ..TokenUsage::default()
                },
            })
        }

        fn model_name(&self) -> &str {
            "complete-only"
        }
    }

    #[tokio::test]
    async fn collects_complete_only_backend_fallback() {
        let backend = CompleteOnlyBackend;
        let stream = backend
            .complete_stream(&[], &[], &LlmCallConfig::default())
            .await
            .expect("fallback stream should open");
        let mut deltas = Vec::new();
        let output = collect_llm_stream(stream, |delta| deltas.push(delta.to_string()))
            .await
            .expect("fallback stream should collect");

        assert_eq!(deltas, vec!["fallback text"]);
        assert_eq!(output.usage.input_tokens, 5);
        assert_eq!(output.usage.output_tokens, 2);
        assert!(matches!(
            output.response,
            LlmResponse::Text(text) if text == "fallback text"
        ));
    }
}
