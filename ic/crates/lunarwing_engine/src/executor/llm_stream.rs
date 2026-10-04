use std::collections::BTreeMap;

use futures::StreamExt;

use crate::traits::llm::{LlmOutput, LlmStream, LlmStreamChunk, ProviderStreamObservation};
use crate::types::error::{EngineError, ModelFailureKind};
use crate::types::repl::{
    ReplArgumentCorruption, extract_repl_code, is_repl_action, validate_repl_arguments,
};
use crate::types::step::{ActionCall, LlmResponse, TokenUsage};

pub(super) struct LlmStreamCollection {
    pub(super) result: Result<LlmOutput, EngineError>,
    pub(super) observations: Vec<ProviderStreamObservation>,
}

#[derive(Default)]
struct StreamAccumulator {
    text: String,
    tool_calls: BTreeMap<usize, ToolCallAccumulator>,
    observations: Vec<ProviderStreamObservation>,
    usage: TokenUsage,
    /// Tokens providers billed for empty attempts discarded before this
    /// stream produced content (WI#297, MR !1395 F-2). Folded into `usage`
    /// when the terminal `Done` arrives.
    discarded_usage: TokenUsage,
    finish_reason: String,
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
) -> LlmStreamCollection {
    let mut accumulator = StreamAccumulator::default();
    while let Some(item) = stream.next().await {
        let result = match item {
            Ok(chunk) => accumulator.push(chunk, &mut on_text_delta),
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            return accumulator.with_error(error);
        }
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
            LlmStreamChunk::ProviderObservation(observation) => {
                if let ProviderStreamObservation::Retried {
                    discarded_usage: Some(usage),
                } = &observation
                {
                    self.discarded_usage = self.discarded_usage.saturating_add(*usage);
                }
                if !self.observations.contains(&observation) {
                    self.observations.push(observation);
                }
            }
            LlmStreamChunk::Done {
                usage,
                finish_reason,
            } => {
                self.usage = usage
                    .unwrap_or_default()
                    .saturating_add(self.discarded_usage);
                self.finish_reason = finish_reason;
                self.saw_done = true;
            }
        }
        Ok(())
    }

    fn finish(mut self) -> LlmStreamCollection {
        let corrupted = self
            .observations
            .contains(&ProviderStreamObservation::Corrupted);
        let observations = std::mem::take(&mut self.observations);
        let result = self.finish_output(corrupted);
        LlmStreamCollection {
            result,
            observations,
        }
    }

    fn with_error(mut self, error: EngineError) -> LlmStreamCollection {
        LlmStreamCollection {
            result: Err(error),
            observations: std::mem::take(&mut self.observations),
        }
    }

    fn finish_output(self, corrupted: bool) -> Result<LlmOutput, EngineError> {
        if !self.saw_done {
            return Err(llm_error("LLM stream ended before Done"));
        }

        let response = if corrupted {
            LlmResponse::Text(self.text)
        } else if self.tool_calls.is_empty() {
            extract_repl_code(&self.text)?;
            LlmResponse::from_text(self.text)
        } else {
            let mut calls = Vec::with_capacity(self.tool_calls.len());
            for (index, call) in self.tool_calls {
                calls.push(call.finish(index)?);
            }
            LlmResponse::from_action_calls(calls, (!self.text.is_empty()).then_some(self.text))
        };

        Ok(LlmOutput {
            response,
            usage: self.usage,
            finish_reason: self.finish_reason,
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
            if is_repl_action(&action_name) {
                return EngineError::from(ReplArgumentCorruption::InvalidJson);
            }
            llm_error(format!(
                "tool call at index {index} has invalid JSON arguments: {error}"
            ))
        })?;
        if is_repl_action(&action_name) {
            validate_repl_arguments(&parameters)?;
        }
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
        kind: ModelFailureKind::InvalidOutput,
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
        let collection = collect_llm_stream(stream, |delta| deltas.push(delta.to_string())).await;
        (collection.result, deltas)
    }

    #[tokio::test]
    async fn collects_provider_observations_outside_text() {
        let stream = stream::iter([
            Ok(LlmStreamChunk::TextDelta("clean output".to_string())),
            Ok(LlmStreamChunk::ProviderObservation(
                ProviderStreamObservation::Retried {
                    discarded_usage: None,
                },
            )),
            Ok(LlmStreamChunk::ProviderObservation(
                ProviderStreamObservation::Retried {
                    discarded_usage: None,
                },
            )),
            done(None),
        ])
        .boxed();
        let collection = collect_llm_stream(stream, |_| {}).await;

        assert!(matches!(
            collection.result.expect("stream should collect").response,
            LlmResponse::Text(text) if text == "clean output"
        ));
        assert_eq!(
            collection.observations,
            vec![ProviderStreamObservation::Retried {
                discarded_usage: None
            }]
        );
    }

    // WI#297 (MR !1395 F-2): billed-but-empty usage riding a `Retried`
    // observation is folded into the call's recorded usage so cost accounting
    // sees every billed attempt.
    #[tokio::test]
    async fn retried_discarded_usage_folds_into_terminal_usage() {
        let stream = stream::iter([
            Ok(LlmStreamChunk::TextDelta("clean output".to_string())),
            Ok(LlmStreamChunk::ProviderObservation(
                ProviderStreamObservation::Retried {
                    discarded_usage: Some(TokenUsage {
                        input_tokens: 900,
                        output_tokens: 0,
                        cache_read_tokens: 100,
                        cache_write_tokens: 0,
                        cost_usd: 0.0,
                    }),
                },
            )),
            Ok(LlmStreamChunk::ProviderObservation(
                ProviderStreamObservation::Retried {
                    discarded_usage: Some(TokenUsage {
                        input_tokens: 1100,
                        output_tokens: 0,
                        cache_read_tokens: 0,
                        cache_write_tokens: 0,
                        cost_usd: 0.0,
                    }),
                },
            )),
            done(Some(TokenUsage {
                input_tokens: 2100,
                output_tokens: 12,
                ..TokenUsage::default()
            })),
        ])
        .boxed();
        let collection = collect_llm_stream(stream, |_| {}).await;

        let output = collection.result.expect("stream should collect");
        assert_eq!(output.usage.input_tokens, 4100);
        assert_eq!(output.usage.output_tokens, 12);
        assert_eq!(output.usage.cache_read_tokens, 100);
        // Distinct discard payloads are not deduplicated — each discarded
        // attempt's usage is accounted exactly once.
        assert_eq!(collection.observations.len(), 2);
    }

    #[tokio::test]
    async fn corrupted_output_is_retained_as_non_executable_text() {
        let original = "```repl\ntest_tool()\n```";
        let stream = stream::iter([
            Ok(LlmStreamChunk::TextDelta(original.to_string())),
            tool(0, Some("call-1"), Some("test_tool"), "{}"),
            Ok(LlmStreamChunk::ProviderObservation(
                ProviderStreamObservation::Corrupted,
            )),
            done(None),
        ])
        .boxed();
        let collection = collect_llm_stream(stream, |_| {}).await;

        assert!(matches!(
            collection.result.expect("stream should collect").response,
            LlmResponse::Text(text) if text == original
        ));
    }

    #[tokio::test]
    async fn preserves_abort_observation_when_stream_fails() {
        let stream = stream::iter([
            Ok(LlmStreamChunk::TextDelta("partial".to_string())),
            Ok(LlmStreamChunk::ProviderObservation(
                ProviderStreamObservation::Aborted,
            )),
            Err(EngineError::Llm {
                kind: ModelFailureKind::Transient,
                reason: "stream failed".to_string(),
            }),
        ])
        .boxed();
        let collection = collect_llm_stream(stream, |_| {}).await;

        assert!(collection.result.is_err());
        assert_eq!(
            collection.observations,
            vec![ProviderStreamObservation::Aborted]
        );
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
        assert_eq!(output.finish_reason, "stop");
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
            matches!(result, Err(EngineError::Llm { reason, .. }) if reason.contains("before Done"))
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
            matches!(result, Err(EngineError::Llm { reason, .. }) if reason.contains("after Done"))
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
            matches!(result, Err(EngineError::Llm { reason, .. }) if reason.contains("after Done"))
        );
    }

    #[tokio::test]
    async fn rejects_second_done() {
        let (result, _) = collect(vec![done(None), done(None)]).await;
        assert!(
            matches!(result, Err(EngineError::Llm { reason, .. }) if reason.contains("after Done"))
        );
    }

    #[tokio::test]
    async fn propagates_mid_stream_error_after_callback() {
        let (result, deltas) = collect(vec![
            Ok(LlmStreamChunk::TextDelta("visible".into())),
            Err(EngineError::Llm {
                kind: ModelFailureKind::Transient,
                reason: "stream failed".into(),
            }),
        ])
        .await;
        assert_eq!(deltas, vec!["visible"]);
        assert!(
            matches!(result, Err(EngineError::Llm { reason, .. }) if reason == "stream failed")
        );
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
                matches!(result, Err(EngineError::Llm { reason, .. }) if reason.contains("conflicting"))
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
                finish_reason: "stop".to_string(),
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
            .result
            .expect("fallback stream should collect");

        assert_eq!(deltas, vec!["fallback text"]);
        assert_eq!(output.usage.input_tokens, 5);
        assert_eq!(output.usage.output_tokens, 2);
        assert_eq!(output.finish_reason, "stop");
        assert!(matches!(
            output.response,
            LlmResponse::Text(text) if text == "fallback text"
        ));
    }
}
