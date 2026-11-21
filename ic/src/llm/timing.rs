//! Single timing boundary for LLM logical calls (WI#318 Phase B1/B3).
//!
//! One `LlmTimingProvider` decorates the provider chain built by
//! [`crate::llm::build_provider_chain`], so v1 chat, local jobs, routines, the
//! container path and the v2 `LlmBridgeAdapter` are all measured by the same
//! timer. It sits outside retry, smart routing, failover, circuit breaking and
//! the response cache, which makes the primary training clock *completed
//! logical-call latency*: one sample per call the caller actually awaited.
//!
//! Retries and failover report their recovered attempts on the stream as
//! `LlmStreamObservation::Retried`; those are counted separately and never
//! become duration samples. A cache hit never reaches the inner provider, so it
//! does not train either. Failures train nothing. Streaming calls train only
//! when the terminal `Done` chunk arrives, so a partial or cancelled stream is
//! counted as a cancellation instead of a fast success.
//!
//! Cost is deliberately absent: a real per-call charge stays with
//! `CostGuard::record_llm_call`, and the v2 adapter's `cost_usd: 0.0` means
//! "unavailable", never "free".

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use futures::StreamExt;
use rust_decimal::Decimal;

use crate::estimation::{LlmCall, LlmTimings};
use crate::llm::error::LlmError;
use crate::llm::provider::{
    CompletionRequest, CompletionResponse, LlmProvider, LlmStream, LlmStreamChunk,
    LlmStreamObservation, ModelMetadata, ToolCompletionRequest, ToolCompletionResponse,
};

/// Decorator that feeds completed-call latency into [`LlmTimings`].
pub struct LlmTimingProvider {
    inner: Arc<dyn LlmProvider>,
    timings: Arc<LlmTimings>,
}

impl LlmTimingProvider {
    pub fn new(inner: Arc<dyn LlmProvider>, timings: Arc<LlmTimings>) -> Self {
        Self { inner, timings }
    }

    /// Label used before dispatch. It is the chain's configured model, never a
    /// prompt, result, user identity or request parameter.
    fn entry_label(&self) -> String {
        self.inner.active_model_name()
    }

    /// Label used after a completed call. Failover updates the chain's active
    /// provider, so this attributes the sample to the model that served it.
    fn served_label(&self) -> String {
        self.inner.active_model_name()
    }

    fn record_blocking<T>(
        &self,
        call: LlmCall,
        started_at: Instant,
        result: Result<T, LlmError>,
    ) -> Result<T, LlmError> {
        match result {
            Ok(response) => {
                self.timings
                    .complete(call, &self.served_label(), started_at.elapsed(), None);
                Ok(response)
            }
            Err(error) => {
                self.timings.fail();
                Err(error)
            }
        }
    }

    fn wrap_stream<'a>(&'a self, call: LlmCall, stream: LlmStream<'a>) -> LlmStream<'a> {
        let timings = Arc::clone(&self.timings);
        let served_label = self.served_label();
        let started_at = Instant::now();
        let mut state = StreamTimingState {
            stream,
            call: Some(call),
            timings,
            served_label,
            started_at,
            first_chunk_at: None,
            finished: false,
        };
        futures::stream::poll_fn(move |cx| {
            use std::task::Poll;

            if state.finished {
                return Poll::Ready(None);
            }
            match state.stream.poll_next_unpin(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    state.observe(&chunk);
                    Poll::Ready(Some(Ok(chunk)))
                }
                Poll::Ready(Some(Err(error))) => {
                    state.fail();
                    Poll::Ready(Some(Err(error)))
                }
                Poll::Ready(None) => {
                    state.cancel();
                    Poll::Ready(None)
                }
                Poll::Pending => Poll::Pending,
            }
        })
        .boxed()
    }
}

struct StreamTimingState<'a> {
    stream: LlmStream<'a>,
    /// Taken exactly once, so a duplicate terminal chunk cannot train twice.
    call: Option<LlmCall>,
    timings: Arc<LlmTimings>,
    served_label: String,
    started_at: Instant,
    first_chunk_at: Option<Instant>,
    finished: bool,
}

impl StreamTimingState<'_> {
    fn observe(&mut self, chunk: &LlmStreamChunk) {
        match chunk {
            LlmStreamChunk::Done { .. } => self.complete(),
            LlmStreamChunk::Observation(LlmStreamObservation::Retried { attempts, .. }) => {
                self.timings.record_retry(*attempts);
            }
            LlmStreamChunk::TextDelta(_)
            | LlmStreamChunk::ReasoningDelta(_)
            | LlmStreamChunk::ToolCallDelta { .. }
            | LlmStreamChunk::Observation(_) => {
                self.first_chunk_at.get_or_insert_with(Instant::now);
            }
        }
    }

    fn complete(&mut self) {
        let Some(call) = self.call.take() else {
            return;
        };
        self.finished = true;
        self.timings.complete(
            call,
            &self.served_label,
            self.started_at.elapsed(),
            self.first_chunk_at
                .map(|at| at.duration_since(self.started_at)),
        );
    }

    fn fail(&mut self) {
        self.finished = true;
        if self.call.take().is_some() {
            self.timings.fail();
        }
    }

    /// Dropped or truncated stream: counted, never trained as a success.
    fn cancel(&mut self) {
        self.finished = true;
        if self.call.take().is_some() {
            self.timings.cancel();
        }
    }
}

impl Drop for StreamTimingState<'_> {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[async_trait]
impl LlmProvider for LlmTimingProvider {
    fn model_name(&self) -> &str {
        self.inner.model_name()
    }

    fn cost_per_token(&self) -> (Decimal, Decimal) {
        self.inner.cost_per_token()
    }

    fn cache_write_multiplier(&self) -> Decimal {
        self.inner.cache_write_multiplier()
    }

    fn cache_read_discount(&self) -> Decimal {
        self.inner.cache_read_discount()
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let Some(call) = self.timings.begin(&self.entry_label()) else {
            return self.inner.complete(request).await;
        };
        let started_at = Instant::now();
        let result = self.inner.complete(request).await;
        self.record_blocking(call, started_at, result)
    }

    async fn complete_stream(&self, request: CompletionRequest) -> Result<LlmStream<'_>, LlmError> {
        let call = self.timings.begin(&self.entry_label());
        let result = self.inner.complete_stream(request).await;
        let stream = match result {
            Ok(stream) => stream,
            Err(error) => {
                if call.is_some() {
                    self.timings.fail();
                }
                return Err(error);
            }
        };
        Ok(match call {
            Some(call) => self.wrap_stream(call, stream),
            None => stream,
        })
    }

    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        let Some(call) = self.timings.begin(&self.entry_label()) else {
            return self.inner.complete_with_tools(request).await;
        };
        let started_at = Instant::now();
        let result = self.inner.complete_with_tools(request).await;
        self.record_blocking(call, started_at, result)
    }

    async fn complete_with_tools_stream(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        let call = self.timings.begin(&self.entry_label());
        let result = self.inner.complete_with_tools_stream(request).await;
        let stream = match result {
            Ok(stream) => stream,
            Err(error) => {
                if call.is_some() {
                    self.timings.fail();
                }
                return Err(error);
            }
        };
        Ok(match call {
            Some(call) => self.wrap_stream(call, stream),
            None => stream,
        })
    }

    async fn list_models(&self) -> Result<Vec<String>, LlmError> {
        self.inner.list_models().await
    }

    async fn model_metadata(&self) -> Result<ModelMetadata, LlmError> {
        self.inner.model_metadata().await
    }

    fn effective_model_name(&self, requested_model: Option<&str>) -> String {
        self.inner.effective_model_name(requested_model)
    }

    fn cache_key_model_name(&self, requested_model: Option<&str>) -> Option<String> {
        self.inner.cache_key_model_name(requested_model)
    }

    fn active_model_name(&self) -> String {
        self.inner.active_model_name()
    }

    fn set_model(&self, model: &str) -> Result<(), LlmError> {
        self.inner.set_model(model)
    }

    fn calculate_cost(&self, input_tokens: u32, output_tokens: u32) -> Decimal {
        self.inner.calculate_cost(input_tokens, output_tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::estimation::{LlmTimingMode, LlmTimings};
    use crate::llm::provider::{FinishReason, LlmStreamObservation, StreamFailureReason};

    struct Stub {
        name: String,
        fail: bool,
        chunks: Vec<LlmStreamChunk>,
        calls: AtomicU64,
    }

    impl Stub {
        fn new(name: &str) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                fail: false,
                chunks: vec![
                    LlmStreamChunk::TextDelta("hello".to_string()),
                    LlmStreamChunk::Done {
                        usage: None,
                        finish_reason: "stop".to_string(),
                    },
                ],
                calls: AtomicU64::new(0),
            })
        }

        fn failing(name: &str) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                fail: true,
                chunks: Vec::new(),
                calls: AtomicU64::new(0),
            })
        }

        fn with_chunks(name: &str, chunks: Vec<LlmStreamChunk>) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                fail: false,
                chunks,
                calls: AtomicU64::new(0),
            })
        }

        fn response(&self) -> CompletionResponse {
            CompletionResponse {
                content: "hello".to_string(),
                reasoning: None,
                input_tokens: 1,
                output_tokens: 1,
                reasoning_tokens: 0,
                finish_reason: FinishReason::Stop,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            }
        }
    }

    #[async_trait]
    impl LlmProvider for Stub {
        fn model_name(&self) -> &str {
            &self.name
        }

        fn cost_per_token(&self) -> (Decimal, Decimal) {
            (Decimal::ZERO, Decimal::ZERO)
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, LlmError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.fail {
                return Err(LlmError::RequestFailed {
                    provider: self.name.clone(),
                    reason: "stub failure".to_string(),
                });
            }
            Ok(self.response())
        }

        async fn complete_stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<LlmStream<'_>, LlmError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.fail {
                return Err(LlmError::RequestFailed {
                    provider: self.name.clone(),
                    reason: "stub failure".to_string(),
                });
            }
            Ok(
                futures::stream::iter(self.chunks.iter().cloned().map(Ok).collect::<Vec<_>>())
                    .boxed(),
            )
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, LlmError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.fail {
                return Err(LlmError::RequestFailed {
                    provider: self.name.clone(),
                    reason: "stub failure".to_string(),
                });
            }
            Ok(ToolCompletionResponse {
                content: Some("hello".to_string()),
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

        async fn complete_with_tools_stream(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<LlmStream<'_>, LlmError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.fail {
                return Err(LlmError::RequestFailed {
                    provider: self.name.clone(),
                    reason: "stub failure".to_string(),
                });
            }
            Ok(
                futures::stream::iter(self.chunks.iter().cloned().map(Ok).collect::<Vec<_>>())
                    .boxed(),
            )
        }
    }

    fn request() -> CompletionRequest {
        CompletionRequest::new(Vec::new())
    }

    fn tool_request() -> ToolCompletionRequest {
        ToolCompletionRequest::new(Vec::new(), Vec::new())
    }

    fn timing() -> Arc<LlmTimings> {
        Arc::new(LlmTimings::new(LlmTimingMode::Observe))
    }

    #[tokio::test]
    async fn blocking_success_trains_one_sample() {
        let timings = timing();
        let stub = Stub::new("model-a");
        let provider = LlmTimingProvider::new(stub, Arc::clone(&timings));
        let response = provider.complete(request()).await.unwrap();
        assert_eq!(response.content, "hello");
        let diagnostics = timings.diagnostics();
        assert_eq!(diagnostics.counters.calls_started, 1);
        assert_eq!(diagnostics.counters.completions, 1);
        assert_eq!(diagnostics.models.len(), 1);
        assert_eq!(diagnostics.models[0].model, "model-a");
    }

    #[tokio::test]
    async fn blocking_failure_trains_nothing() {
        let timings = timing();
        let stub = Stub::failing("model-a");
        let provider = LlmTimingProvider::new(stub, Arc::clone(&timings));
        assert!(provider.complete(request()).await.is_err());
        assert!(provider.complete_with_tools(tool_request()).await.is_err());
        let diagnostics = timings.diagnostics();
        assert!(diagnostics.models.is_empty());
        assert_eq!(diagnostics.counters.failures, 2);
        assert_eq!(diagnostics.counters.completions, 0);
    }

    #[tokio::test]
    async fn stream_trains_only_after_the_terminal_chunk() {
        let timings = timing();
        let provider = LlmTimingProvider::new(Stub::new("model-a"), Arc::clone(&timings));

        let partial = provider.complete_stream(request()).await.unwrap();
        assert!(timings.diagnostics().models.is_empty());
        drop(partial);
        let after_cancel = timings.diagnostics();
        assert_eq!(after_cancel.counters.cancellations, 1);
        assert!(after_cancel.models.is_empty());

        let stream = provider.complete_stream(request()).await.unwrap();
        let chunks: Vec<_> = stream.collect().await;
        assert_eq!(chunks.len(), 2);
        let diagnostics = timings.diagnostics();
        assert_eq!(diagnostics.counters.completions, 1);
        assert_eq!(diagnostics.counters.cancellations, 1);
        assert_eq!(diagnostics.models.len(), 1);
        assert_eq!(diagnostics.counters.first_chunk_samples, 1);
    }

    #[tokio::test]
    async fn duplicate_terminal_chunks_train_once() {
        let timings = timing();
        let stub = Stub::with_chunks(
            "model-a",
            vec![
                LlmStreamChunk::Done {
                    usage: None,
                    finish_reason: "stop".to_string(),
                },
                LlmStreamChunk::Done {
                    usage: None,
                    finish_reason: "stop".to_string(),
                },
            ],
        );
        let provider = LlmTimingProvider::new(stub, Arc::clone(&timings));
        let stream = provider
            .complete_with_tools_stream(tool_request())
            .await
            .unwrap();
        let _: Vec<_> = stream.collect().await;
        assert_eq!(timings.diagnostics().counters.completions, 1);
    }

    #[tokio::test]
    async fn retry_observations_count_attempts_without_training_them() {
        let timings = timing();
        let stub = Stub::with_chunks(
            "model-a",
            vec![
                LlmStreamChunk::Observation(LlmStreamObservation::Retried {
                    provider: "model-a".to_string(),
                    attempts: 3,
                    reason: StreamFailureReason::Decode,
                    discarded_usage: None,
                }),
                LlmStreamChunk::TextDelta("ok".to_string()),
                LlmStreamChunk::Done {
                    usage: None,
                    finish_reason: "stop".to_string(),
                },
            ],
        );
        let provider = LlmTimingProvider::new(stub, Arc::clone(&timings));
        let stream = provider.complete_stream(request()).await.unwrap();
        let _: Vec<_> = stream.collect().await;
        let diagnostics = timings.diagnostics();
        assert_eq!(diagnostics.counters.retried_calls, 1);
        assert_eq!(diagnostics.counters.retry_attempts, 2);
        assert_eq!(diagnostics.counters.completions, 1);
        assert_eq!(diagnostics.models[0].duration_samples, 1);
    }

    #[tokio::test]
    async fn disabled_timing_passes_through_without_work() {
        let timings = Arc::new(LlmTimings::new(LlmTimingMode::Disabled));
        let stub = Stub::new("model-a");
        let provider = LlmTimingProvider::new(Arc::clone(&stub) as Arc<dyn LlmProvider>, timings);
        provider.complete(request()).await.unwrap();
        let stream = provider.complete_stream(request()).await.unwrap();
        let _: Vec<_> = stream.collect().await;
        assert_eq!(stub.calls.load(Ordering::Relaxed), 2);
    }
}
