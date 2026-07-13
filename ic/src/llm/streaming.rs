use futures::StreamExt;
use futures::stream;

use crate::llm::error::LlmError;
use crate::llm::provider::{
    CompletionRequest, LlmProvider, LlmStream, LlmStreamChunk, ToolCompletionRequest,
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

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::{FirstStreamItem, replay_first, take_first};
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
}
