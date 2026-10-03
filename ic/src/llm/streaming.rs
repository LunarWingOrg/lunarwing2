use futures::StreamExt;
use futures::stream;

use crate::llm::error::LlmError;
use crate::llm::provider::{
    CompletionRequest, FinishReason, LlmProvider, LlmStream, LlmStreamChunk, ToolCompletionRequest,
};

/// Message marker shared with `stream_telemetry::classify_stream_failure` so
/// empty-stream failures classify as `StreamFailureReason::Empty`.
pub(crate) const EMPTY_STREAM_MARKER: &str = "empty provider stream";

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

pub(crate) enum FirstStreamItem<'a> {
    Empty,
    Error(LlmError),
    Chunk {
        first: LlmStreamChunk,
        rest: LlmStream<'a>,
    },
}

pub(crate) async fn take_first<'a>(mut stream: LlmStream<'a>) -> FirstStreamItem<'a> {
    match stream.next().await {
        None => FirstStreamItem::Empty,
        Some(Err(error)) => FirstStreamItem::Error(error),
        Some(Ok(first)) => FirstStreamItem::Chunk {
            first,
            rest: stream,
        },
    }
}

pub(crate) fn replay_first<'a>(first: LlmStreamChunk, rest: LlmStream<'a>) -> LlmStream<'a> {
    stream::iter([Ok(first)]).chain(rest).boxed()
}

/// Build the retryable error for a provider stream that produced no content:
/// either zero chunks before EOF, or a terminal `Done` chunk arriving first
/// with a non-tool finish reason (WI#297). The WI#292 `finish_reason` is kept
/// in the message when the provider sent one so operators can distinguish
/// `stop` / `length` / `content_filter` empties.
pub(crate) fn empty_stream_error(provider: &str, finish_reason: Option<&str>) -> LlmError {
    let reason = match finish_reason {
        Some(finish) => {
            format!("{EMPTY_STREAM_MARKER}: completed with no content (finish_reason: {finish})")
        }
        None => format!("{EMPTY_STREAM_MARKER}: ended before any chunk"),
    };
    LlmError::InvalidResponse {
        provider: provider.to_string(),
        reason,
    }
}

/// A terminal `Done` chunk represents an empty completion eligible for
/// retry/failover when its finish reason is anything but a tool call.
fn is_empty_completion(finish_reason: &str) -> bool {
    !matches!(
        FinishReason::from_stream_reason(finish_reason),
        FinishReason::ToolUse
    )
}

/// Convert empty-stream outcomes into retryable errors so retry and failover
/// layers treat a 200-with-nothing response as a transient failure instead of
/// a successful empty stream (WI#297).
pub(crate) fn empty_as_error(first: FirstStreamItem<'_>, provider: &str) -> FirstStreamItem<'_> {
    match first {
        FirstStreamItem::Empty => FirstStreamItem::Error(empty_stream_error(provider, None)),
        FirstStreamItem::Chunk { first, rest } => match first {
            LlmStreamChunk::Done { finish_reason, .. } if is_empty_completion(&finish_reason) => {
                FirstStreamItem::Error(empty_stream_error(provider, Some(&finish_reason)))
            }
            first => FirstStreamItem::Chunk { first, rest },
        },
        error => error,
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::{EMPTY_STREAM_MARKER, FirstStreamItem, empty_as_error, replay_first, take_first};
    use crate::llm::{LlmError, LlmStreamChunk};

    #[tokio::test]
    async fn take_first_separates_the_first_successful_chunk() {
        let stream = futures::stream::iter([
            Ok(LlmStreamChunk::TextDelta("first".to_string())),
            Ok(LlmStreamChunk::TextDelta("second".to_string())),
        ])
        .boxed();

        let FirstStreamItem::Chunk { first, rest } = take_first(stream).await else {
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
    async fn take_first_separates_an_initial_error() {
        let stream = futures::stream::iter([Err(LlmError::RequestFailed {
            provider: "test".to_string(),
            reason: "failed before output".to_string(),
        })])
        .boxed();

        assert!(matches!(
            take_first(stream).await,
            FirstStreamItem::Error(LlmError::RequestFailed { provider, reason })
                if provider == "test" && reason == "failed before output"
        ));
    }

    #[tokio::test]
    async fn take_first_reports_an_empty_stream() {
        let stream = futures::stream::empty().boxed();

        assert!(matches!(take_first(stream).await, FirstStreamItem::Empty));
    }

    #[test]
    fn empty_as_error_maps_zero_chunks_to_retryable_invalid_response() {
        let mapped = empty_as_error(FirstStreamItem::Empty, "primary");

        assert!(matches!(
            mapped,
            FirstStreamItem::Error(LlmError::InvalidResponse { provider, reason })
                if provider == "primary"
                    && reason.contains(EMPTY_STREAM_MARKER)
                    && reason.contains("ended before any chunk")
        ));
    }

    #[tokio::test]
    async fn empty_as_error_maps_done_first_non_tool_finish_to_error() {
        for finish_reason in ["stop", "length", "content_filter", ""] {
            let stream = futures::stream::iter([Ok(LlmStreamChunk::Done {
                usage: None,
                finish_reason: finish_reason.to_string(),
            })])
            .boxed();
            let first = take_first(stream).await;

            let mapped = empty_as_error(first, "primary");

            assert!(
                matches!(
                    mapped,
                    FirstStreamItem::Error(LlmError::InvalidResponse { provider, reason })
                        if provider == "primary"
                            && reason.contains(EMPTY_STREAM_MARKER)
                            && reason.contains(finish_reason.as_str())
                ),
                "finish_reason {finish_reason:?} should map to an empty-stream error"
            );
        }
    }

    #[tokio::test]
    async fn empty_as_error_passes_through_done_first_tool_finish() {
        let stream = futures::stream::iter([Ok(LlmStreamChunk::Done {
            usage: None,
            finish_reason: "tool_calls".to_string(),
        })])
        .boxed();
        let first = take_first(stream).await;

        let mapped = empty_as_error(first, "primary");

        assert!(matches!(
            mapped,
            FirstStreamItem::Chunk {
                first: LlmStreamChunk::Done { finish_reason, .. },
                ..
            } if finish_reason == "tool_calls"
        ));
    }

    #[tokio::test]
    async fn empty_as_error_passes_through_content_chunks() {
        let stream = futures::stream::iter([
            Ok(LlmStreamChunk::TextDelta("hello".to_string())),
            Ok(LlmStreamChunk::Done {
                usage: None,
                finish_reason: "stop".to_string(),
            }),
        ])
        .boxed();
        let first = take_first(stream).await;

        let mapped = empty_as_error(first, "primary");

        assert!(matches!(
            mapped,
            FirstStreamItem::Chunk {
                first: LlmStreamChunk::TextDelta(_),
                ..
            }
        ));
    }
}
