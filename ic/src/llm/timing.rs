//! Single timing boundary for LLM logical calls (WI#318 Phase B1/B3).
//!
//! One `LlmTimingProvider` decorates the provider chain built by
//! [`super::build_provider_chain`], so v1 chat, local jobs, routines, the
//! container path and the v2 `LlmBridgeAdapter` are all measured by the same
//! timer. It sits outside retry, smart routing, failover and circuit breaking,
//! but inside the response cache so hits bypass it. The clock includes stream
//! setup and same-provider retry time.
//!
//! The serving provider records its identity at dispatch in a scope local to
//! the call. Retry and routing layers record attempts and provider switches in
//! that same scope, without relying on the chain's last-used model or exposing
//! internal metadata in responses. Only a successful call with one known
//! provider/model trains; streams must reach `Done`.
//!
//! Cost is deliberately absent: a real per-call charge stays with
//! `CostGuard::record_llm_call`, and the v2 adapter's `cost_usd: 0.0` means
//! "unavailable", never "free".

use std::cell::RefCell;
use std::future::Future;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use rust_decimal::Decimal;
use tokio::time::Instant;

use super::error::LlmError;
use super::provider::{
    CompletionRequest, CompletionResponse, LlmProvider, LlmStream, LlmStreamChunk, ModelMetadata,
    ToolCompletionRequest, ToolCompletionResponse,
};

/// The application owns the observer and its models; the provider chain
/// supplies only parameter-free observations. Methods must not block on I/O.
pub trait LlmTimingObserver: Send + Sync {
    fn call_started(&self);
    fn predict(&self, provider: &str, model: &str) -> Option<Duration>;
    fn call_finished(&self, observation: LlmTimingObservation);
}

pub struct LlmTimingTarget {
    pub provider: String,
    pub model: String,
    pub predicted: Option<Duration>,
}

pub enum LlmTimingOutcome {
    Completed {
        elapsed: Duration,
        target: Option<LlmTimingTarget>,
    },
    Failed,
    Cancelled,
}

pub struct LlmTimingObservation {
    pub outcome: LlmTimingOutcome,
    pub attempts: u64,
    pub retries: u64,
    pub switched: bool,
    pub first_chunk: Option<Duration>,
}

pub(crate) fn valid_timing_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 256
        && !label.contains("://")
        && !label.chars().any(|c| c.is_whitespace() || c.is_control())
}

tokio::task_local! {
    // Scoped to the dispatch future, not the whole Tokio task. In particular,
    // interleaved calls in a single task have separate attribution.
    static CALL: RefCell<CallTiming>;
}

/// Observe a concrete dispatch. Only pass a model label from configuration,
/// never an unchecked request override, URL, or response field.
pub(super) fn record_dispatch(provider: &str, model: Option<&str>) {
    observe_dispatch(|call| call.dispatch(provider, model));
}

pub(super) fn record_retry_attempt() {
    observe_dispatch(CallTiming::retry);
}

pub(super) fn record_provider_switch() {
    observe_dispatch(CallTiming::switch_provider);
}

fn observe_dispatch(observe: impl FnOnce(&mut CallTiming)) {
    let _ = CALL.try_with(|call| {
        if let Ok(mut call) = call.try_borrow_mut() {
            observe(&mut call);
        }
    });
}

/// Decorator that produces one timing observation per logical call.
pub struct LlmTimingProvider {
    inner: Arc<dyn LlmProvider>,
    observer: Arc<dyn LlmTimingObserver>,
}

impl LlmTimingProvider {
    pub fn new(inner: Arc<dyn LlmProvider>, observer: Arc<dyn LlmTimingObserver>) -> Self {
        Self { inner, observer }
    }

    pub(super) fn wrap(
        inner: Arc<dyn LlmProvider>,
        observer: Option<Arc<dyn LlmTimingObserver>>,
    ) -> Arc<dyn LlmProvider> {
        match observer {
            Some(observer) => Arc::new(Self::new(inner, observer)),
            None => inner,
        }
    }

    async fn observe<T>(
        &self,
        future: impl Future<Output = Result<T, LlmError>>,
        on_success: impl FnOnce(T, CallTiming) -> T,
    ) -> Result<T, LlmError> {
        // Start before awaiting the inner call, including for stream setup.
        let scope = CALL.scope(RefCell::new(CallTiming::new(&self.observer)), future);
        tokio::pin!(scope);
        let result = scope.as_mut().await;
        match (result, scope.as_mut().take_value()) {
            (Ok(response), Some(call)) => Ok(on_success(response, call.into_inner())),
            (Err(error), Some(call)) => {
                call.into_inner().fail();
                Err(error)
            }
            (result, None) => result,
        }
    }

    fn finish_blocking<T>(response: T, mut call: CallTiming) -> T {
        call.complete();
        response
    }

    fn wrap_stream(mut stream: LlmStream<'_>, mut call: CallTiming) -> LlmStream<'_> {
        futures::stream::poll_fn(move |cx| {
            let item = stream.poll_next_unpin(cx);
            match &item {
                Poll::Ready(Some(Ok(chunk))) => call.observe_chunk(chunk),
                Poll::Ready(Some(Err(_))) => call.fail(),
                Poll::Ready(None) => call.cancel(),
                Poll::Pending => {}
            }
            item
        })
        .boxed()
    }
}

struct CallTiming {
    observer: Arc<dyn LlmTimingObserver>,
    started_at: Instant,
    target: Option<LlmTimingTarget>,
    attempts: u64,
    retries: u64,
    switched: bool,
    first_chunk: Option<Duration>,
    finished: bool,
}

impl CallTiming {
    fn new(observer: &Arc<dyn LlmTimingObserver>) -> Self {
        observer.call_started();
        Self {
            observer: Arc::clone(observer),
            started_at: Instant::now(),
            target: None,
            attempts: 0,
            retries: 0,
            switched: false,
            first_chunk: None,
            finished: false,
        }
    }

    fn dispatch(&mut self, provider: &str, model: Option<&str>) {
        self.attempts = self.attempts.saturating_add(1);
        if self.switched {
            return;
        }
        if self.attempts == 1 {
            self.target = model
                .filter(|model| valid_timing_label(provider) && valid_timing_label(model))
                .map(|model| LlmTimingTarget {
                    provider: provider.to_owned(),
                    model: model.to_owned(),
                    predicted: self.observer.predict(provider, model),
                });
        } else if let Some(target) = &self.target
            && (target.provider != provider || model != Some(target.model.as_str()))
        {
            self.switch_provider();
        }
    }

    fn retry(&mut self) {
        self.retries = self.retries.saturating_add(1);
    }

    fn switch_provider(&mut self) {
        self.switched = true;
        self.target = None;
    }

    fn observe_chunk(&mut self, chunk: &LlmStreamChunk) {
        if self.finished {
            return;
        }
        match chunk {
            LlmStreamChunk::Done { .. } => self.complete(),
            LlmStreamChunk::TextDelta(_)
            | LlmStreamChunk::ReasoningDelta(_)
            | LlmStreamChunk::ToolCallDelta { .. } => {
                self.first_chunk
                    .get_or_insert_with(|| self.started_at.elapsed());
            }
            _ => {}
        }
    }

    fn complete(&mut self) {
        let target = self.target.take();
        self.finish(LlmTimingOutcome::Completed {
            elapsed: self.started_at.elapsed(),
            target,
        });
    }

    fn fail(&mut self) {
        self.finish(LlmTimingOutcome::Failed);
    }

    /// Dropped future or truncated stream: never trained as a success.
    fn cancel(&mut self) {
        self.finish(LlmTimingOutcome::Cancelled);
    }

    fn finish(&mut self, outcome: LlmTimingOutcome) {
        if !std::mem::replace(&mut self.finished, true) {
            self.observer.call_finished(LlmTimingObservation {
                outcome,
                attempts: self.attempts,
                retries: self.retries,
                switched: self.switched,
                first_chunk: self.first_chunk,
            });
        }
    }
}

impl Drop for CallTiming {
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
        self.observe(self.inner.complete(request), Self::finish_blocking)
            .await
    }

    async fn complete_stream(&self, request: CompletionRequest) -> Result<LlmStream<'_>, LlmError> {
        self.observe(self.inner.complete_stream(request), Self::wrap_stream)
            .await
    }

    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        self.observe(
            self.inner.complete_with_tools(request),
            Self::finish_blocking,
        )
        .await
    }

    async fn complete_with_tools_stream(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        self.observe(
            self.inner.complete_with_tools_stream(request),
            Self::wrap_stream,
        )
        .await
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
    use std::sync::{Mutex, MutexGuard};

    use super::super::provider::{ChatMessage, FinishReason, LlmStreamObservation};
    use super::super::{
        CachedProvider, FailoverProvider, ResponseCacheConfig, RetryConfig, RetryProvider,
        SmartRoutingConfig, SmartRoutingProvider,
    };

    struct Stub {
        name: String,
        provider: &'static str,
        failures: u64,
        delay: Duration,
        content: &'static str,
        chunks: Vec<LlmStreamChunk>,
        calls: AtomicU64,
    }

    impl Stub {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                provider: "fixture-provider",
                failures: 0,
                delay: Duration::from_millis(40),
                content: "hello",
                chunks: vec![
                    LlmStreamChunk::TextDelta("hello".to_string()),
                    LlmStreamChunk::Done {
                        usage: None,
                        finish_reason: "stop".to_string(),
                    },
                ],
                calls: AtomicU64::new(0),
            }
        }

        fn failing(name: &str) -> Arc<Self> {
            Arc::new(Self {
                failures: u64::MAX,
                ..Self::new(name)
            })
        }

        fn with_chunks(name: &str, chunks: Vec<LlmStreamChunk>) -> Arc<Self> {
            Arc::new(Self {
                chunks,
                ..Self::new(name)
            })
        }

        async fn dispatch(&self) -> Result<(), LlmError> {
            record_dispatch(self.provider, Some(&self.name));
            let attempt = self.calls.fetch_add(1, Ordering::Relaxed);
            tokio::time::sleep(self.delay).await;
            if attempt < self.failures {
                return Err(LlmError::RateLimited {
                    provider: self.name.clone(),
                    retry_after: Some(Duration::from_millis(20)),
                });
            }
            Ok(())
        }

        fn response(&self) -> CompletionResponse {
            CompletionResponse {
                content: self.content.to_string(),
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
            self.dispatch().await?;
            Ok(self.response())
        }

        async fn complete_stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<LlmStream<'_>, LlmError> {
            self.dispatch().await?;
            Ok(
                futures::stream::iter(self.chunks.iter().cloned().map(Ok).collect::<Vec<_>>())
                    .boxed(),
            )
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, LlmError> {
            self.dispatch().await?;
            Ok(ToolCompletionResponse {
                content: Some(self.content.to_string()),
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
            self.dispatch().await?;
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

    #[derive(Default)]
    struct Recorder {
        started: AtomicU64,
        observations: Mutex<Vec<LlmTimingObservation>>,
    }

    impl LlmTimingObserver for Recorder {
        fn call_started(&self) {
            self.started.fetch_add(1, Ordering::Relaxed);
        }

        fn predict(&self, _: &str, _: &str) -> Option<Duration> {
            Some(Duration::from_millis(50))
        }

        fn call_finished(&self, observation: LlmTimingObservation) {
            self.observations.lock().unwrap().push(observation);
        }
    }

    impl Recorder {
        fn observations(&self) -> MutexGuard<'_, Vec<LlmTimingObservation>> {
            self.observations.lock().unwrap()
        }
    }

    fn timing() -> Arc<Recorder> {
        Arc::new(Recorder::default())
    }

    fn timed(inner: Arc<dyn LlmProvider>, recorder: &Arc<Recorder>) -> LlmTimingProvider {
        LlmTimingProvider::new(inner, recorder.clone())
    }

    fn assert_sample(observation: &LlmTimingObservation, model: &str, elapsed_ms: u64) {
        let LlmTimingOutcome::Completed {
            elapsed,
            target: Some(target),
        } = &observation.outcome
        else {
            panic!("expected an attributed completion");
        };
        assert_eq!(target.provider, "fixture-provider");
        assert_eq!(target.model, model);
        assert_eq!(target.predicted, Some(Duration::from_millis(50)));
        assert_eq!(*elapsed, Duration::from_millis(elapsed_ms));
    }

    #[tokio::test(start_paused = true)]
    async fn blocking_success_trains_one_sample() {
        let timings = timing();
        let stub = Arc::new(Stub::new("model-a"));
        let provider = timed(stub, &timings);
        let response = provider.complete(request()).await.unwrap();
        assert_eq!(response.content, "hello");
        let calls = timings.observations();
        assert_eq!(timings.started.load(Ordering::Relaxed), 1);
        assert_eq!(calls.len(), 1);
        assert_sample(&calls[0], "model-a", 40);
    }

    #[tokio::test(start_paused = true)]
    async fn blocking_failure_trains_nothing() {
        let timings = timing();
        let stub = Stub::failing("model-a");
        let provider = timed(stub, &timings);
        assert!(provider.complete(request()).await.is_err());
        assert!(provider.complete_with_tools(tool_request()).await.is_err());
        let calls = timings.observations();
        assert_eq!(calls.len(), 2);
        assert!(
            calls
                .iter()
                .all(|call| matches!(call.outcome, LlmTimingOutcome::Failed))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stream_trains_only_after_the_terminal_chunk() {
        let timings = timing();
        let provider = timed(Arc::new(Stub::new("model-a")), &timings);

        let partial = provider.complete_stream(request()).await.unwrap();
        assert!(timings.observations().is_empty());
        drop(partial);
        assert!(matches!(
            timings.observations()[0].outcome,
            LlmTimingOutcome::Cancelled
        ));

        let stream = provider.complete_stream(request()).await.unwrap();
        let chunks: Vec<_> = stream.collect().await;
        assert_eq!(chunks.len(), 2);
        let calls = timings.observations();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].first_chunk, Some(Duration::from_millis(40)));
        assert_sample(&calls[1], "model-a", 40);
    }

    #[tokio::test(start_paused = true)]
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
        let provider = timed(stub, &timings);
        let stream = provider
            .complete_with_tools_stream(tool_request())
            .await
            .unwrap();
        let chunks: Vec<_> = stream.collect().await;
        assert_eq!(chunks.len(), 2, "observing must not alter the stream");
        let calls = timings.observations();
        assert_eq!(calls.len(), 1);
        assert_sample(&calls[0], "model-a", 40);
    }

    #[tokio::test(start_paused = true)]
    async fn blocking_and_stream_retries_count_without_per_attempt_training() {
        let timings = timing();
        for stream in [false, true] {
            let stub = Arc::new(Stub {
                failures: 2,
                ..Stub::new("model-a")
            });
            let provider = timed(
                Arc::new(RetryProvider::new(stub, RetryConfig::default())),
                &timings,
            );
            if stream {
                let chunks: Vec<_> = provider
                    .complete_with_tools_stream(tool_request())
                    .await
                    .unwrap()
                    .collect()
                    .await;
                assert!(chunks.iter().any(|chunk| matches!(
                    chunk,
                    Ok(LlmStreamChunk::Observation(LlmStreamObservation::Retried {
                        attempts: 2,
                        ..
                    }))
                )));
            } else {
                provider.complete(request()).await.unwrap();
            }
        }
        let calls = timings.observations();
        assert_eq!(calls.len(), 2);
        for call in &*calls {
            assert_eq!(call.retries, 2);
            assert_eq!(call.attempts, 3);
            assert_sample(call, "model-a", 160);
        }
        assert_eq!(calls[1].first_chunk, Some(Duration::from_millis(160)));
    }

    #[tokio::test(start_paused = true)]
    async fn failover_success_counts_the_switch_but_never_trains_either_model() {
        let timings = timing();
        let primary = Stub::failing("shared-model");
        let fallback = Arc::new(Stub {
            provider: "fallback-provider",
            content: "fallback response",
            ..Stub::new("shared-model")
        });
        let chain = FailoverProvider::new(vec![primary, fallback]).unwrap();
        let provider = timed(Arc::new(chain), &timings);

        assert_eq!(
            provider.complete(request()).await.unwrap().content,
            "fallback response"
        );
        let chunks: Vec<_> = provider
            .complete_with_tools_stream(tool_request())
            .await
            .unwrap()
            .collect()
            .await;
        assert!(
            chunks
                .iter()
                .any(|chunk| matches!(chunk, Ok(LlmStreamChunk::Done { .. })))
        );
        let calls = timings.observations();
        assert_eq!(timings.started.load(Ordering::Relaxed), 2);
        assert_eq!(calls.len(), 2);
        for call in &*calls {
            assert!(call.switched);
            assert_eq!(call.attempts, 2);
            assert_eq!(call.retries, 0);
            assert!(matches!(
                call.outcome,
                LlmTimingOutcome::Completed { target: None, .. }
            ));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn smart_routing_attributes_each_call_even_when_interleaved_on_one_task() {
        let timings = timing();
        let primary = Arc::new(Stub {
            delay: Duration::from_millis(80),
            ..Stub::new("primary")
        });
        let chain = SmartRoutingProvider::new(
            primary,
            Arc::new(Stub::new("cheap")),
            SmartRoutingConfig::default(),
        );
        let provider = timed(Arc::new(chain), &timings);

        let cheap_request = CompletionRequest::new(vec![ChatMessage::user("[tier:flash] hello")]);
        let (cheap, tools) = tokio::join!(provider.complete(cheap_request), async {
            provider
                .complete_with_tools_stream(tool_request())
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await
        });
        assert!(cheap.is_ok());
        assert_eq!(tools.len(), 2);
        let calls = timings.observations();
        assert_eq!(calls.len(), 2);
        assert!(
            calls
                .iter()
                .all(|call| !call.switched && call.attempts == 1)
        );
        assert_sample(&calls[0], "cheap", 40);
        assert_sample(&calls[1], "primary", 80);
        assert_eq!(calls[1].first_chunk, Some(Duration::from_millis(80)));
    }

    #[tokio::test(start_paused = true)]
    async fn smart_routing_cascade_does_not_train_with_discarded_cheap_latency() {
        let timings = timing();
        let cheap = Arc::new(Stub {
            content: "I'm not sure.",
            ..Stub::new("cheap")
        });
        let chain = SmartRoutingProvider::new(
            Arc::new(Stub::new("primary")),
            cheap,
            SmartRoutingConfig {
                cascade_enabled: true,
                ..SmartRoutingConfig::default()
            },
        );
        let provider = timed(Arc::new(chain), &timings);
        let request = CompletionRequest::new(vec![ChatMessage::user("[tier:pro] question")]);
        assert_eq!(provider.complete(request).await.unwrap().content, "hello");
        let calls = timings.observations();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].switched);
        assert_eq!(calls[0].attempts, 2);
        assert!(matches!(
            calls[0].outcome,
            LlmTimingOutcome::Completed { target: None, .. }
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn stream_setup_timeout_and_truncated_stream_do_not_train() {
        let timings = timing();
        let stub = Stub::with_chunks("model-a", vec![LlmStreamChunk::TextDelta("partial".into())]);
        let provider = timed(stub, &timings);
        let opening = provider.complete_stream(request());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), opening)
                .await
                .is_err()
        );
        let chunks: Vec<_> = provider
            .complete_stream(request())
            .await
            .unwrap()
            .collect()
            .await;
        assert_eq!(chunks.len(), 1);
        let calls = timings.observations();
        assert_eq!(calls.len(), 2);
        assert!(
            calls
                .iter()
                .all(|call| matches!(call.outcome, LlmTimingOutcome::Cancelled))
        );
        assert_eq!(calls[0].first_chunk, None);
        assert_eq!(calls[1].first_chunk, Some(Duration::from_millis(40)));
    }

    /// Mirrors the production nesting: the response cache wraps the timer, so
    /// a cache hit is served before the timer's inner provider runs.
    #[tokio::test(start_paused = true)]
    async fn cache_hit_inside_the_chain_never_trains() {
        let timings = timing();
        let stub = Arc::new(Stub::new("model-a"));
        let timer = timed(stub, &timings);
        let cache = CachedProvider::new(Arc::new(timer), ResponseCacheConfig::default());

        for _ in 0..2 {
            let stream = cache
                .complete_stream(request())
                .await
                .expect("stream should open");
            let _: Vec<_> = stream.collect().await;
        }
        assert_eq!(cache.total_hits(), 1);
        let calls = timings.observations();
        assert_eq!(timings.started.load(Ordering::Relaxed), 1);
        assert_eq!(calls.len(), 1);
        assert_sample(&calls[0], "model-a", 40);
    }

    #[tokio::test(start_paused = true)]
    async fn disabled_timing_passes_through_without_work() {
        let stub = Arc::new(Stub::new("model-a"));
        let inner = stub.clone() as Arc<dyn LlmProvider>;
        let provider = LlmTimingProvider::wrap(inner.clone(), None);
        assert!(Arc::ptr_eq(&inner, &provider));
        provider.complete(request()).await.unwrap();
        let stream = provider.complete_stream(request()).await.unwrap();
        let _: Vec<_> = stream.collect().await;
        assert_eq!(stub.calls.load(Ordering::Relaxed), 2);
    }
}
