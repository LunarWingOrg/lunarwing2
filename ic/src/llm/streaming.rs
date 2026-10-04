use futures::StreamExt;
use futures::stream;

use crate::llm::error::LlmError;
use crate::llm::provider::{
    CompletionRequest, FinishReason, LlmProvider, LlmStream, LlmStreamChunk, TokenUsage,
    ToolCompletionRequest,
};

#[derive(Clone)]
pub(crate) enum ProviderStreamRequest {
    Plain(CompletionRequest),
    Tools(ToolCompletionRequest),
}

impl ProviderStreamRequest {
    pub(crate) async fn open<'a>(
        &self,
        provider: &'a dyn LlmProvider,
    ) -> Result<LlmStream<'a>, LlmError> {
        match self {
            Self::Plain(request) => provider.complete_stream(request.clone()).await,
            Self::Tools(request) => provider.complete_with_tools_stream(request.clone()).await,
        }
    }
}

/// Outcome of opening a provider stream and consuming past any content-free
/// leading chunks (WI#297).
///
/// There is deliberately no `Empty` variant: an empty-but-successful stream
/// is reported as [`LlmError::EmptyStream`] so retry and failover layers
/// treat a 200-with-nothing response as a failure instead of a successful
/// empty stream.
pub(crate) enum StreamOpening<'a> {
    /// First meaningful chunk plus the rest of the stream. A terminal `Done`
    /// never appears here — it is terminal by contract, so a `Done` arriving
    /// first means the stream carried no content whatever its finish string.
    Content {
        first: LlmStreamChunk,
        rest: LlmStream<'a>,
    },
    /// Stream setup failed, a pre-content item errored, or the stream proved
    /// to be empty.
    Error(LlmError),
}

/// Maximum length of the provider's raw finish spelling kept in the
/// operator-facing detail string (MR !1395 F-6).
const MAX_RAW_FINISH_CHARS: usize = 64;

/// Sanitize the provider's raw finish string for the operator-facing detail:
/// strip control characters and cap the length so a hostile or buggy gateway
/// cannot inject unbounded or terminal-control text into host-side logs.
fn sanitize_raw_finish(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .take(MAX_RAW_FINISH_CHARS)
        .collect()
}

/// Build the typed empty-stream error for a provider stream that produced no
/// content (WI#297).
///
/// `raw_finish_reason` is the provider's terminal finish string when a `Done`
/// chunk was seen; `None` means the stream ended before any chunk at all. The
/// WI#292 classification is kept typed so retry/failover can distinguish
/// transient empties from deterministic `Length`/`ContentFilter` ones, while
/// `detail` keeps the raw spelling visible to operators. `usage` carries the
/// tokens the provider still billed for the empty completion (MR !1395 F-2).
pub(crate) fn empty_stream_error(
    provider: &str,
    raw_finish_reason: Option<&str>,
    usage: Option<TokenUsage>,
) -> LlmError {
    let (finish, detail) = match raw_finish_reason {
        Some(raw) => (
            FinishReason::from_stream_reason(raw),
            format!(
                "completed with no content (finish_reason: {})",
                sanitize_raw_finish(raw)
            ),
        ),
        None => (FinishReason::Unknown, "ended before any chunk".to_string()),
    };
    LlmError::EmptyStream {
        provider: provider.to_string(),
        finish,
        detail,
        usage,
    }
}

/// Whether an empty completion with this finish reason could change if the
/// same request were retried against this provider or failed over to the next
/// leg (WI#297).
///
/// `Stop`/`Unknown` (and `ToolUse` anomalies where a `Done` claims tool calls
/// that never arrived) are transient. `Length` is deterministic — the model
/// burned its output budget before any visible text — and `ContentFilter` is
/// a refusal; neither is fixed by re-sending the identical request.
pub(crate) fn is_transient_empty_finish(finish: &FinishReason) -> bool {
    matches!(
        finish,
        FinishReason::Stop | FinishReason::ToolUse | FinishReason::Unknown
    )
}

/// Whether `error` is a transient empty-stream outcome that callers may
/// retry, fail over, or cascade-escalate on (WI#297).
pub(crate) fn is_transient_empty_stream(error: &LlmError) -> bool {
    matches!(
        error,
        LlmError::EmptyStream { finish, .. } if is_transient_empty_finish(finish)
    )
}

/// Consume `stream` past leading content-free `TextDelta("")` chunks and
/// classify the first meaningful item.
///
/// Some providers (notably the blocking `complete_stream` fallback before
/// WI#297, and chat gateways) emit an empty text delta ahead of `Done` for an
/// empty completion; skipping those keeps detection shape-independent so the
/// failover chain cannot accept an empty answer as success.
pub(crate) async fn open_stream<'a>(
    mut stream: LlmStream<'a>,
    provider: &str,
) -> StreamOpening<'a> {
    loop {
        match stream.next().await {
            None => return StreamOpening::Error(empty_stream_error(provider, None, None)),
            Some(Err(error)) => return StreamOpening::Error(error),
            Some(Ok(LlmStreamChunk::TextDelta(text))) if text.is_empty() => continue,
            Some(Ok(LlmStreamChunk::Done {
                finish_reason,
                usage,
            })) => {
                return StreamOpening::Error(empty_stream_error(
                    provider,
                    Some(&finish_reason),
                    usage,
                ));
            }
            Some(Ok(first)) => {
                return StreamOpening::Content {
                    first,
                    rest: stream,
                };
            }
        }
    }
}

pub(crate) fn replay_first<'a>(first: LlmStreamChunk, rest: LlmStream<'a>) -> LlmStream<'a> {
    stream::iter([Ok(first)]).chain(rest).boxed()
}

/// The billed usage carried by a typed empty-stream error, if any (WI#297).
pub(crate) fn empty_stream_usage(error: &LlmError) -> Option<TokenUsage> {
    match error {
        LlmError::EmptyStream { usage, .. } => *usage,
        _ => None,
    }
}

/// Sum the billed usage of an intercepted empty attempt into `acc`
/// (MR !1395 F-2). Non-empty errors carry no usage and leave `acc` untouched.
pub(crate) fn accumulate_empty_usage(acc: &mut Option<TokenUsage>, error: &LlmError) {
    if let Some(usage) = empty_stream_usage(error) {
        *acc = Some(acc.map_or(usage, |total| total.saturating_add(usage)));
    }
}

/// Fold the accumulated discarded usage into a terminal empty-stream error so
/// the next layer — or the terminal warn log — accounts for every discarded
/// attempt, not just the last one (MR !1395 F-2).
pub(crate) fn fold_discarded_usage(error: LlmError, discarded: Option<TokenUsage>) -> LlmError {
    match (error, discarded) {
        (
            LlmError::EmptyStream {
                provider,
                finish,
                detail,
                usage,
            },
            Some(discarded),
        ) => {
            let usage = Some(usage.map_or(discarded, |u| u.saturating_add(discarded)));
            LlmError::EmptyStream {
                provider,
                finish,
                detail,
                usage,
            }
        }
        (error, _) => error,
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::{
        StreamOpening, empty_stream_error, is_transient_empty_finish, is_transient_empty_stream,
        open_stream, replay_first,
    };
    use crate::llm::provider::{FinishReason, TokenUsage};
    use crate::llm::{LlmError, LlmStreamChunk};

    fn done_chunk(finish_reason: &str) -> LlmStreamChunk {
        LlmStreamChunk::Done {
            usage: None,
            finish_reason: finish_reason.to_string(),
        }
    }

    #[tokio::test]
    async fn open_stream_separates_the_first_successful_chunk() {
        let stream = futures::stream::iter([
            Ok(LlmStreamChunk::TextDelta("first".to_string())),
            Ok(LlmStreamChunk::TextDelta("second".to_string())),
        ])
        .boxed();

        let StreamOpening::Content { first, rest } = open_stream(stream, "test").await else {
            panic!("expected a successful first chunk");
        };

        assert!(matches!(
            &first,
            LlmStreamChunk::TextDelta(text) if text == "first"
        ));
        let replayed = replay_first(first, rest).collect::<Vec<_>>().await;
        assert!(matches!(
            replayed.first(),
            Some(Ok(LlmStreamChunk::TextDelta(text))) if text == "first"
        ));
        assert!(matches!(
            replayed.get(1),
            Some(Ok(LlmStreamChunk::TextDelta(text))) if text == "second"
        ));
    }

    #[tokio::test]
    async fn open_stream_separates_an_initial_error() {
        let stream = futures::stream::iter([Err(LlmError::RequestFailed {
            provider: "test".to_string(),
            reason: "failed before output".to_string(),
        })])
        .boxed();

        assert!(matches!(
            open_stream(stream, "test").await,
            StreamOpening::Error(LlmError::RequestFailed { provider, reason })
                if provider == "test" && reason == "failed before output"
        ));
    }

    #[tokio::test]
    async fn open_stream_maps_zero_chunks_to_typed_empty_stream_error() {
        let stream = futures::stream::empty().boxed();

        assert!(matches!(
            open_stream(stream, "primary").await,
            StreamOpening::Error(LlmError::EmptyStream {
                provider,
                finish: FinishReason::Unknown,
                detail,
                usage: None,
            }) if provider == "primary" && detail == "ended before any chunk"
        ));
    }

    #[tokio::test]
    async fn open_stream_maps_done_first_to_typed_empty_stream_error() {
        // `stop`/`length`/`content_filter`/`tool_calls`/"" all count: a
        // terminal `Done` first means the stream carried no content (WI#297
        // F5). Assert the exact detail so an empty finish reason cannot make
        // the assertion vacuous (F8).
        for finish_reason in ["stop", "length", "content_filter", "tool_calls", ""] {
            let stream = futures::stream::iter([Ok(done_chunk(finish_reason))]).boxed();

            let opening = open_stream(stream, "primary").await;

            let expected_detail =
                format!("completed with no content (finish_reason: {finish_reason})");
            assert!(
                matches!(
                    opening,
                    StreamOpening::Error(LlmError::EmptyStream { provider, detail, .. })
                        if provider == "primary" && detail == expected_detail
                ),
                "finish_reason {finish_reason:?} should map to an empty-stream error"
            );
        }
    }

    #[tokio::test]
    async fn open_stream_skips_leading_empty_text_deltas() {
        let stream = futures::stream::iter([
            Ok(LlmStreamChunk::TextDelta(String::new())),
            Ok(LlmStreamChunk::TextDelta(String::new())),
            Ok(LlmStreamChunk::TextDelta("real content".to_string())),
        ])
        .boxed();

        let StreamOpening::Content { first, .. } = open_stream(stream, "primary").await else {
            panic!("expected the leading empty deltas to be skipped");
        };

        assert!(matches!(
            &first,
            LlmStreamChunk::TextDelta(text) if text == "real content"
        ));
    }

    #[tokio::test]
    async fn open_stream_maps_empty_deltas_then_done_to_empty_stream_error() {
        // The WI#297 F1 production shape: `[TextDelta(""), Done{stop}]`.
        let stream = futures::stream::iter([
            Ok(LlmStreamChunk::TextDelta(String::new())),
            Ok(done_chunk("stop")),
        ])
        .boxed();

        assert!(matches!(
            open_stream(stream, "primary").await,
            StreamOpening::Error(LlmError::EmptyStream {
                provider,
                finish: FinishReason::Stop,
                ..
            }) if provider == "primary"
        ));
    }

    #[tokio::test]
    async fn open_stream_treats_tool_call_chunks_as_content() {
        let stream = futures::stream::iter([
            Ok(LlmStreamChunk::ToolCallDelta {
                index: 0,
                id: Some("call_1".to_string()),
                name: Some("search".to_string()),
                args_delta: "{}".to_string(),
            }),
            Ok(done_chunk("tool_calls")),
        ])
        .boxed();

        assert!(matches!(
            open_stream(stream, "primary").await,
            StreamOpening::Content {
                first: LlmStreamChunk::ToolCallDelta { index: 0, .. },
                ..
            }
        ));
    }

    #[test]
    fn empty_stream_error_classifies_finish_reasons() {
        let length = empty_stream_error("primary", Some("max_tokens"), None);
        assert!(matches!(
            length,
            LlmError::EmptyStream {
                finish: FinishReason::Length,
                ..
            }
        ));

        let eof = empty_stream_error("primary", None, None);
        assert!(matches!(
            eof,
            LlmError::EmptyStream {
                finish: FinishReason::Unknown,
                ..
            }
        ));
    }

    #[test]
    fn transient_empty_finish_classification() {
        assert!(is_transient_empty_finish(&FinishReason::Stop));
        assert!(is_transient_empty_finish(&FinishReason::Unknown));
        assert!(is_transient_empty_finish(&FinishReason::ToolUse));
        assert!(!is_transient_empty_finish(&FinishReason::Length));
        assert!(!is_transient_empty_finish(&FinishReason::ContentFilter));
    }

    #[test]
    fn transient_empty_stream_only_matches_typed_empty_errors() {
        let transient = empty_stream_error("primary", Some("stop"), None);
        assert!(is_transient_empty_stream(&transient));

        let deterministic = empty_stream_error("primary", Some("content_filter"), None);
        assert!(!is_transient_empty_stream(&deterministic));

        let other = LlmError::InvalidResponse {
            provider: "primary".to_string(),
            reason: "some other invalid response".to_string(),
        };
        assert!(!is_transient_empty_stream(&other));
    }

    // MR !1395 F-2: the usage of a billed-but-empty `Done`-first completion
    // rides along on the typed error so decorators can account for it.
    #[tokio::test]
    async fn open_stream_carries_done_usage_into_the_empty_stream_error() {
        let stream = futures::stream::iter([Ok(LlmStreamChunk::Done {
            usage: Some(TokenUsage {
                input_tokens: 1200,
                output_tokens: 0,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            }),
            finish_reason: "length".to_string(),
        })])
        .boxed();

        assert!(matches!(
            open_stream(stream, "primary").await,
            StreamOpening::Error(LlmError::EmptyStream {
                provider,
                finish: FinishReason::Length,
                usage: Some(TokenUsage {
                    input_tokens: 1200,
                    output_tokens: 0,
                    ..
                }),
                ..
            }) if provider == "primary"
        ));
    }

    // MR !1395 F-6: the raw finish spelling embedded in the operator-facing
    // detail is capped and stripped of control characters.
    #[tokio::test]
    async fn open_stream_sanitizes_the_raw_finish_reason_in_detail() {
        let hostile = format!("stop\u{1b}[2J\u{07}{}", "x".repeat(200));
        let stream = futures::stream::iter([Ok(LlmStreamChunk::Done {
            usage: None,
            finish_reason: hostile,
        })])
        .boxed();

        let StreamOpening::Error(LlmError::EmptyStream { detail, .. }) =
            open_stream(stream, "primary").await
        else {
            panic!("expected an empty-stream error");
        };

        assert!(!detail.chars().any(char::is_control));
        assert!(detail.len() <= "completed with no content (finish_reason: )".len() + 64);
    }
}
