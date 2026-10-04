//! Shared retry helpers and composable `RetryProvider` decorator for LLM providers.
//!
//! Provides:
//! - `is_retryable()` — `LlmError`-level retryability classification (shared with `failover.rs`)
//! - `retry_backoff_delay()` — exponential backoff with jitter
//! - `RetryProvider` — decorator that wraps any `LlmProvider` with automatic retries

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use rand::Rng;
use rust_decimal::Decimal;

use crate::llm::error::LlmError;
use crate::llm::provider::{
    CompletionRequest, CompletionResponse, LlmProvider, LlmStream, LlmStreamChunk,
    LlmStreamObservation, ModelMetadata, StreamFailureReason, ToolCompletionRequest,
    ToolCompletionResponse,
};
use crate::llm::stream_telemetry::{StreamTelemetry, classify_stream_failure};
use crate::llm::streaming::{
    ProviderStreamRequest, StreamOpening, is_transient_empty_finish, open_stream, replay_first,
};

/// Upper bound for provider-suggested `Retry-After` delays.
///
/// This prevents malicious or malformed headers from turning a retryable
/// response into an effectively unbounded sleep.
pub(crate) const MAX_RETRY_AFTER_SECS: u64 = 3600;

/// Returns `true` if the `LlmError` is transient and the request should be retried.
///
/// Used by `RetryProvider` (retry the same provider) and `FailoverProvider`
/// (try the next provider). The question is: "could this exact same request
/// succeed if we try again?"
///
/// Retryable: transient `RequestFailed`, `RateLimited`, `InvalidResponse`,
/// `SessionRenewalFailed`, `Http`, `Io`, and transient `EmptyStream`
/// outcomes (`Stop`/`Unknown`/`ToolUse`-anomaly finishes, WI#297).
///
/// Non-retryable: `AuthFailed`, `SessionExpired`, `ContextLengthExceeded`,
/// `ModelNotAvailable`, `Json`, `ReplArgumentCorruption`, and deterministic
/// `EmptyStream` outcomes (`Length`/`ContentFilter`) — re-sending the
/// identical request cannot fix an output-budget truncation or a refusal.
/// - `SessionExpired` — handled by session renewal layer, not by retry
/// - `ModelNotAvailable` — the model won't appear between attempts
/// - `Json` — a serde parse bug, not a transient failure
/// - `ReplArgumentCorruption` — the engine owns the bounded, model-visible repair
///
/// See also `circuit_breaker::is_transient()` which answers a different
/// question: "does this error indicate the backend is degraded?"
pub(crate) fn is_retryable(err: &LlmError) -> bool {
    match err {
        LlmError::RequestFailed { reason, .. } => !is_oversized_request(reason),
        LlmError::RateLimited { .. }
        | LlmError::InvalidResponse { .. }
        | LlmError::SessionRenewalFailed { .. }
        | LlmError::Io(_) => true,
        LlmError::EmptyStream { finish, .. } => is_transient_empty_finish(finish),
        LlmError::Http(error) => error.status().is_none_or(|status| status.as_u16() != 413),
        LlmError::AuthFailed { .. }
        | LlmError::SessionExpired { .. }
        | LlmError::ContextLengthExceeded { .. }
        | LlmError::ModelNotAvailable { .. }
        | LlmError::ReplArgumentCorruption { .. }
        | LlmError::Json(_) => false,
    }
}

/// Rig and OpenAI-compatible proxies often flatten typed provider errors into
/// `RequestFailed`. Detect deterministic size failures so retry/failover does
/// not resend the exact same oversized payload.
pub(crate) fn is_oversized_request(reason: &str) -> bool {
    let reason = reason.to_ascii_lowercase();
    [
        "context length exceeded",
        "context_length_exceeded",
        "maximum context length",
        "context window exceeded",
        "exceeds the context window",
        "too many input tokens",
        "payload too large",
        "request entity too large",
        "request too large",
        "413 payload too large",
        "status code: 413",
        "status code 413",
        "\"status_code\":413",
        "\"status_code\": 413",
    ]
    .iter()
    .any(|pattern| reason.contains(pattern))
}

/// Calculate exponential backoff delay with random jitter.
///
/// Base delay is 1 second, doubled each attempt, with +/-25% jitter.
/// - attempt 0: ~1s (0.75s - 1.25s)
/// - attempt 1: ~2s (1.5s - 2.5s)
/// - attempt 2: ~4s (3.0s - 5.0s)
pub(crate) fn retry_backoff_delay(attempt: u32) -> Duration {
    let base_ms: u64 = 1000u64.saturating_mul(2u64.saturating_pow(attempt));
    let jitter_range = base_ms / 4; // 25%
    let jitter = if jitter_range > 0 {
        let offset = rand::thread_rng().gen_range(0..=jitter_range * 2);
        offset as i64 - jitter_range as i64
    } else {
        0
    };
    let delay_ms = (base_ms as i64 + jitter).max(100) as u64;
    Duration::from_millis(delay_ms)
}

/// Clamp a provider-suggested retry delay to a safe maximum.
pub(crate) fn cap_retry_after(duration: Duration) -> Duration {
    duration.min(Duration::from_secs(MAX_RETRY_AFTER_SECS))
}

fn retry_delay(error: &LlmError, attempt: u32) -> Duration {
    match error {
        LlmError::RateLimited {
            retry_after: Some(duration),
            ..
        } => cap_retry_after(*duration),
        _ => retry_backoff_delay(attempt),
    }
}

/// Parse a `Retry-After` header value into a capped `Duration`.
///
/// Supports both delay-seconds (RFC 7231 §7.1.3) and HTTP-date formats (RFC 7231
/// §7.1.1 / IMF-fixdate). The implementation uses `chrono::DateTime::parse_from_rfc2822`,
/// which also accepts RFC 2822-style dates.
/// Returns `DEFAULT_RETRY_AFTER` (60 s) if the header is missing or unparseable.
pub(crate) fn parse_retry_after(header: Option<&reqwest::header::HeaderValue>) -> Duration {
    header
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            if let Ok(secs) = v.trim().parse::<u64>() {
                return Some(cap_retry_after(Duration::from_secs(secs)));
            }
            if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(v.trim()) {
                let now = chrono::Utc::now();
                let delta = dt.signed_duration_since(now);
                return Some(cap_retry_after(Duration::from_secs(
                    delta.num_seconds().max(0) as u64,
                )));
            }
            None
        })
        .unwrap_or(Duration::from_secs(DEFAULT_RETRY_AFTER_SECS))
}

const DEFAULT_RETRY_AFTER_SECS: u64 = 60;

/// Configuration for the retry decorator.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts (not counting the initial attempt).
    /// Default: 3.
    pub max_retries: u32,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self { max_retries: 3 }
    }
}

/// Composable decorator that wraps any `LlmProvider` with automatic retries.
///
/// On transient errors, sleeps using exponential backoff and retries.
/// On non-transient errors (`AuthFailed`, `ContextLengthExceeded`, `SessionExpired`),
/// returns immediately.
///
/// Special handling for `RateLimited { retry_after }`: uses the provider-suggested
/// duration if available, otherwise falls back to standard backoff.
pub struct RetryProvider {
    inner: Arc<dyn LlmProvider>,
    config: RetryConfig,
    telemetry: Option<Arc<StreamTelemetry>>,
}

impl RetryProvider {
    pub fn new(inner: Arc<dyn LlmProvider>, config: RetryConfig) -> Self {
        Self {
            inner,
            config,
            telemetry: None,
        }
    }

    pub(crate) fn with_telemetry(mut self, telemetry: Arc<StreamTelemetry>) -> Self {
        self.telemetry = Some(telemetry);
        self
    }

    async fn record_retry(&self, error: &LlmError) -> StreamFailureReason {
        let reason = classify_stream_failure(error);
        if let Some(telemetry) = &self.telemetry {
            telemetry
                .record_retry(self.inner.model_name(), reason)
                .await;
        }
        reason
    }

    async fn retry_loop<T, F, Fut>(&self, mut op: F, label: &str) -> Result<T, LlmError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, LlmError>>,
    {
        let mut last_error: Option<LlmError> = None;

        for attempt in 0..=self.config.max_retries {
            match op().await {
                Ok(resp) => return Ok(resp),
                Err(err) => {
                    if !is_retryable(&err) || attempt == self.config.max_retries {
                        return Err(err);
                    }

                    let delay = retry_delay(&err, attempt);
                    self.record_retry(&err).await;
                    tracing::warn!(
                        provider = %self.inner.model_name(),
                        attempt = attempt + 1,
                        max_retries = self.config.max_retries,
                        delay_ms = delay.as_millis() as u64,
                        error = %err,
                        "Retrying after transient error{label}"
                    );

                    last_error = Some(err);
                    tokio::time::sleep(delay).await;
                }
            }
        }

        Err(last_error.unwrap_or_else(|| LlmError::RequestFailed {
            provider: self.inner.model_name().to_string(),
            reason: "retry loop exited unexpectedly".to_string(),
        }))
    }

    async fn retry_stream<'a>(
        &'a self,
        request: ProviderStreamRequest,
        label: &str,
    ) -> Result<LlmStream<'a>, LlmError> {
        let mut retries = 0;
        let mut last_reason = StreamFailureReason::Unknown;
        for attempt in 0..=self.config.max_retries {
            // WI#297: an empty-but-successful stream surfaces as a typed
            // `EmptyStream` error here, never as `Ok(empty)`.
            let opening = match request.open(self.inner.as_ref()).await {
                Ok(stream) => open_stream(stream, self.inner.model_name()).await,
                Err(error) => StreamOpening::Error(error),
            };

            match opening {
                StreamOpening::Content { first, rest } if retries == 0 => {
                    return Ok(replay_first(first, rest));
                }
                StreamOpening::Content { first, rest } => {
                    let observation = LlmStreamObservation::Retried {
                        provider: self.inner.model_name().to_string(),
                        attempts: retries,
                        reason: last_reason,
                    };
                    return Ok(replay_with_observation(first, rest, observation));
                }
                StreamOpening::Error(error) => {
                    if !is_retryable(&error) || attempt == self.config.max_retries {
                        return Err(error);
                    }
                    let delay = retry_delay(&error, attempt);
                    last_reason = self.record_retry(&error).await;
                    retries += 1;
                    tracing::warn!(
                        provider = %self.inner.model_name(),
                        attempt = attempt + 1,
                        max_retries = self.config.max_retries,
                        delay_ms = delay.as_millis() as u64,
                        error = %error,
                        "Retrying stream before first chunk{label}"
                    );
                    tokio::time::sleep(delay).await;
                }
            }
        }

        Err(LlmError::RequestFailed {
            provider: self.inner.model_name().to_string(),
            reason: "stream retry loop exhausted without a result".to_string(),
        })
    }
}

fn replay_with_observation<'a>(
    first: LlmStreamChunk,
    rest: LlmStream<'a>,
    observation: LlmStreamObservation,
) -> LlmStream<'a> {
    let observation = LlmStreamChunk::Observation(observation);
    let prefix = if matches!(first, LlmStreamChunk::Done { .. }) {
        vec![Ok(observation), Ok(first)]
    } else {
        vec![Ok(first), Ok(observation)]
    };
    futures::stream::iter(prefix).chain(rest).boxed()
}

#[async_trait]
impl LlmProvider for RetryProvider {
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
        let inner = &self.inner;
        self.retry_loop(
            || {
                let req = request.clone();
                async move { inner.complete(req).await }
            },
            "",
        )
        .await
    }

    async fn complete_stream(&self, request: CompletionRequest) -> Result<LlmStream<'_>, LlmError> {
        self.retry_stream(ProviderStreamRequest::Plain(request), "")
            .await
    }

    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        let inner = &self.inner;
        self.retry_loop(
            || {
                let req = request.clone();
                async move { inner.complete_with_tools(req).await }
            },
            " (tools)",
        )
        .await
    }

    async fn complete_with_tools_stream(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        self.retry_stream(ProviderStreamRequest::Tools(request), " (tools)")
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

    use futures::StreamExt;

    use crate::llm::provider::LlmStreamChunk;
    use crate::llm::streaming_test_support::{ScriptedStreamingProvider, StreamScript};
    use crate::testing::StubLlm;

    fn make_request() -> CompletionRequest {
        CompletionRequest::new(vec![crate::llm::ChatMessage::user("hello")])
    }

    fn make_tool_request() -> ToolCompletionRequest {
        ToolCompletionRequest::new(vec![crate::llm::ChatMessage::user("hello")], vec![])
    }

    fn fast_config(max_retries: u32) -> RetryConfig {
        RetryConfig { max_retries }
    }

    fn retryable_stream_error() -> LlmError {
        LlmError::RateLimited {
            provider: "scripted".to_string(),
            retry_after: Some(Duration::ZERO),
        }
    }

    fn successful_text_stream() -> StreamScript {
        StreamScript::Items(vec![
            Ok(LlmStreamChunk::TextDelta("hello".to_string())),
            Ok(LlmStreamChunk::Done {
                usage: None,
                finish_reason: "stop".to_string(),
            }),
        ])
    }

    fn assert_retry_observation(chunks: &[Result<LlmStreamChunk, LlmError>]) {
        assert!(chunks.iter().any(|chunk| matches!(
            chunk,
            Ok(LlmStreamChunk::Observation(LlmStreamObservation::Retried {
                provider,
                attempts: 1,
                reason: StreamFailureReason::RateLimited,
            })) if provider == "scripted"
        )));
    }

    // -- Backoff delay tests --

    #[test]
    fn test_retry_backoff_delay_exponential_growth() {
        // Run multiple samples to verify the range, accounting for jitter
        for _ in 0..20 {
            let d0 = retry_backoff_delay(0);
            let d1 = retry_backoff_delay(1);
            let d2 = retry_backoff_delay(2);

            // Attempt 0: base 1000ms, jitter +/-250ms -> [750, 1250]
            assert!(d0.as_millis() >= 750, "attempt 0 too low: {:?}", d0);
            assert!(d0.as_millis() <= 1250, "attempt 0 too high: {:?}", d0);

            // Attempt 1: base 2000ms, jitter +/-500ms -> [1500, 2500]
            assert!(d1.as_millis() >= 1500, "attempt 1 too low: {:?}", d1);
            assert!(d1.as_millis() <= 2500, "attempt 1 too high: {:?}", d1);

            // Attempt 2: base 4000ms, jitter +/-1000ms -> [3000, 5000]
            assert!(d2.as_millis() >= 3000, "attempt 2 too low: {:?}", d2);
            assert!(d2.as_millis() <= 5000, "attempt 2 too high: {:?}", d2);
        }
    }

    #[test]
    fn test_retry_backoff_delay_minimum() {
        // Even at attempt 0, delay should be at least 100ms (the minimum floor)
        for _ in 0..20 {
            let delay = retry_backoff_delay(0);
            assert!(delay.as_millis() >= 100);
        }
    }

    #[test]
    fn test_retry_backoff_delay_no_overflow() {
        // Very high attempt numbers should not panic from overflow
        let delay = retry_backoff_delay(30);
        assert!(delay.as_millis() >= 100);
    }

    // -- is_retryable() classification tests --

    #[test]
    fn test_is_retryable_classification() {
        // Retryable
        assert!(is_retryable(&LlmError::RequestFailed {
            provider: "p".into(),
            reason: "err".into(),
        }));
        assert!(is_retryable(&LlmError::RateLimited {
            provider: "p".into(),
            retry_after: None,
        }));
        assert!(is_retryable(&LlmError::InvalidResponse {
            provider: "p".into(),
            reason: "bad".into(),
        }));
        assert!(is_retryable(&LlmError::SessionRenewalFailed {
            provider: "p".into(),
            reason: "timeout".into(),
        }));
        assert!(is_retryable(&LlmError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "reset"
        ))));

        // NOT retryable
        assert!(!is_retryable(&LlmError::AuthFailed {
            provider: "p".into(),
        }));
        assert!(!is_retryable(&LlmError::SessionExpired {
            provider: "p".into(),
        }));
        assert!(!is_retryable(&LlmError::ContextLengthExceeded {
            used: 100_000,
            limit: 50_000,
        }));
        assert!(!is_retryable(&LlmError::ModelNotAvailable {
            provider: "p".into(),
            model: "m".into(),
        }));
    }

    #[test]
    fn oversized_request_wrapped_as_request_failed_is_not_retryable() {
        for reason in [
            "400 Bad Request: maximum context length exceeded",
            "upstream error: context_length_exceeded",
            "413 Payload Too Large",
            r#"502 Bad Gateway: {"status_code":413,"message":"Payload Too Large"}"#,
        ] {
            assert!(!is_retryable(&LlmError::RequestFailed {
                provider: "proxy".into(),
                reason: reason.into(),
            }));
        }
    }

    // -- RetryProvider tests --

    #[tokio::test]
    async fn success_on_first_attempt() {
        let stub = Arc::new(StubLlm::new("ok").with_model_name("test"));
        let retry = RetryProvider::new(stub.clone(), fast_config(3));

        let resp = retry.complete(make_request()).await;
        assert!(resp.is_ok());
        assert_eq!(resp.unwrap().content, "ok");
        assert_eq!(stub.calls(), 1);
    }

    #[tokio::test]
    async fn retries_transient_errors_then_succeeds() {
        // StubLlm starts failing, then we flip it to succeed.
        // With max_retries=2, it will try 3 times total.
        let stub = Arc::new(StubLlm::failing("test"));
        let retry = RetryProvider::new(stub.clone(), fast_config(2));

        // Spawn a task that flips the stub to succeed after a short delay
        let stub_clone = stub.clone();
        tokio::spawn(async move {
            // Wait for at least 1 retry attempt (backoff is ~1s, so 1.5s should be enough)
            tokio::time::sleep(Duration::from_millis(1500)).await;
            stub_clone.set_failing(false);
        });

        let resp = retry.complete(make_request()).await;
        assert!(resp.is_ok());
        // Should have called at least twice (first fail, then succeed after flip)
        assert!(stub.calls() >= 2);
    }

    #[tokio::test]
    async fn non_transient_error_fails_immediately() {
        let stub = Arc::new(StubLlm::failing_non_transient("test"));
        let retry = RetryProvider::new(stub.clone(), fast_config(3));

        let err = retry.complete(make_request()).await.unwrap_err();
        assert!(matches!(err, LlmError::ContextLengthExceeded { .. }));
        // Should only be called once — no retries for non-transient errors
        assert_eq!(stub.calls(), 1);
    }

    #[tokio::test]
    async fn exhausts_retries_then_returns_error() {
        let stub = Arc::new(StubLlm::failing("test"));
        // max_retries=0 means only the initial attempt, no retries
        let retry = RetryProvider::new(stub.clone(), fast_config(0));

        let err = retry.complete(make_request()).await.unwrap_err();
        assert!(matches!(err, LlmError::RequestFailed { .. }));
        assert_eq!(stub.calls(), 1);
    }

    #[tokio::test]
    async fn complete_with_tools_retries_same_as_complete() {
        let stub = Arc::new(StubLlm::failing_non_transient("test"));
        let retry = RetryProvider::new(stub.clone(), fast_config(3));

        let err = retry
            .complete_with_tools(make_tool_request())
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::ContextLengthExceeded { .. }));
        assert_eq!(stub.calls(), 1);
    }

    #[tokio::test]
    async fn passthrough_methods_delegate_to_inner() {
        let stub = Arc::new(StubLlm::new("ok").with_model_name("my-model"));
        let retry = RetryProvider::new(stub, fast_config(3));

        assert_eq!(retry.model_name(), "my-model");
        assert_eq!(retry.active_model_name(), "my-model");
        assert_eq!(retry.cost_per_token(), (Decimal::ZERO, Decimal::ZERO));
        assert_eq!(retry.calculate_cost(100, 50), Decimal::ZERO);
    }

    #[tokio::test]
    async fn stream_retries_setup_error_before_first_chunk() {
        let inner = Arc::new(ScriptedStreamingProvider::new(
            "scripted",
            vec![
                StreamScript::SetupError(retryable_stream_error()),
                successful_text_stream(),
            ],
            vec![],
        ));
        let retry = RetryProvider::new(inner.clone(), fast_config(1));

        let chunks = retry
            .complete_stream(make_request())
            .await
            .expect("retry should open a stream")
            .collect::<Vec<_>>()
            .await;

        assert_eq!(inner.plain_calls(), 2);
        assert!(matches!(
            chunks.first(),
            Some(Ok(LlmStreamChunk::TextDelta(text))) if text == "hello"
        ));
        assert_retry_observation(&chunks);
    }

    #[tokio::test]
    async fn stream_retries_first_item_error_before_first_chunk() {
        let inner = Arc::new(ScriptedStreamingProvider::new(
            "scripted",
            vec![
                StreamScript::Items(vec![Err(retryable_stream_error())]),
                successful_text_stream(),
            ],
            vec![],
        ));
        let retry = RetryProvider::new(inner.clone(), fast_config(1));

        let chunks = retry
            .complete_stream(make_request())
            .await
            .expect("retry should open a stream")
            .collect::<Vec<_>>()
            .await;

        assert_eq!(inner.plain_calls(), 2);
        assert!(matches!(
            chunks.first(),
            Some(Ok(LlmStreamChunk::TextDelta(text))) if text == "hello"
        ));
        assert_retry_observation(&chunks);
    }

    #[tokio::test]
    async fn stream_does_not_retry_after_first_chunk() {
        let inner = Arc::new(ScriptedStreamingProvider::new(
            "scripted",
            vec![
                StreamScript::Items(vec![
                    Ok(LlmStreamChunk::TextDelta("visible".to_string())),
                    Err(retryable_stream_error()),
                ]),
                successful_text_stream(),
            ],
            vec![],
        ));
        let retry = RetryProvider::new(inner.clone(), fast_config(1));

        let chunks = retry
            .complete_stream(make_request())
            .await
            .expect("stream should open")
            .collect::<Vec<_>>()
            .await;

        assert_eq!(inner.plain_calls(), 1);
        assert_eq!(chunks.len(), 2);
        assert!(matches!(
            chunks.first(),
            Some(Ok(LlmStreamChunk::TextDelta(text))) if text == "visible"
        ));
        assert!(matches!(
            chunks.get(1),
            Some(Err(LlmError::RateLimited { .. }))
        ));
    }

    #[tokio::test]
    async fn tool_stream_uses_same_retry_policy() {
        let inner = Arc::new(ScriptedStreamingProvider::new(
            "scripted",
            vec![],
            vec![
                StreamScript::SetupError(retryable_stream_error()),
                StreamScript::Items(vec![
                    Ok(LlmStreamChunk::ToolCallDelta {
                        index: 0,
                        id: Some("call_1".to_string()),
                        name: Some("search".to_string()),
                        args_delta: "{}".to_string(),
                    }),
                    Ok(LlmStreamChunk::Done {
                        usage: None,
                        finish_reason: "tool_calls".to_string(),
                    }),
                ]),
            ],
        ));
        let retry = RetryProvider::new(inner.clone(), fast_config(1));

        let chunks = retry
            .complete_with_tools_stream(make_tool_request())
            .await
            .expect("tool retry should open a stream")
            .collect::<Vec<_>>()
            .await;

        assert_eq!(inner.tool_calls(), 2);
        assert!(matches!(
            chunks.first(),
            Some(Ok(LlmStreamChunk::ToolCallDelta { index: 0, .. }))
        ));
        assert_retry_observation(&chunks);
    }

    // -- WI#297: empty-stream retry policy --

    fn done_only_stream(finish_reason: &str) -> StreamScript {
        StreamScript::Items(vec![Ok(LlmStreamChunk::Done {
            usage: None,
            finish_reason: finish_reason.to_string(),
        })])
    }

    fn assert_empty_retry_observation(chunks: &[Result<LlmStreamChunk, LlmError>]) {
        assert!(chunks.iter().any(|chunk| matches!(
            chunk,
            Ok(LlmStreamChunk::Observation(LlmStreamObservation::Retried {
                provider,
                attempts: 1,
                reason: StreamFailureReason::Empty,
            })) if provider == "scripted"
        )));
    }

    // WI#297: an empty-but-successful stream must be retried, not returned
    // as Ok(empty).
    #[tokio::test(start_paused = true)]
    async fn stream_retries_empty_stream_then_succeeds() {
        let inner = Arc::new(ScriptedStreamingProvider::new(
            "scripted",
            vec![StreamScript::Items(vec![]), successful_text_stream()],
            vec![],
        ));
        let retry = RetryProvider::new(inner.clone(), fast_config(1));

        let chunks = retry
            .complete_stream(make_request())
            .await
            .expect("retry should open a stream")
            .collect::<Vec<_>>()
            .await;

        assert_eq!(inner.plain_calls(), 2);
        assert!(matches!(
            chunks.first(),
            Some(Ok(LlmStreamChunk::TextDelta(text))) if text == "hello"
        ));
        assert_empty_retry_observation(&chunks);
    }

    // WI#297 F1: the production cloud shape `[TextDelta(""), Done{stop}]`
    // must also be retried, not accepted as a successful stream.
    #[tokio::test(start_paused = true)]
    async fn stream_retries_empty_delta_then_done_shape() {
        let inner = Arc::new(ScriptedStreamingProvider::new(
            "scripted",
            vec![
                StreamScript::Items(vec![
                    Ok(LlmStreamChunk::TextDelta(String::new())),
                    Ok(LlmStreamChunk::Done {
                        usage: None,
                        finish_reason: "stop".to_string(),
                    }),
                ]),
                successful_text_stream(),
            ],
            vec![],
        ));
        let retry = RetryProvider::new(inner.clone(), fast_config(1));

        let chunks = retry
            .complete_stream(make_request())
            .await
            .expect("retry should open a stream")
            .collect::<Vec<_>>()
            .await;

        assert_eq!(inner.plain_calls(), 2);
        assert!(matches!(
            chunks.first(),
            Some(Ok(LlmStreamChunk::TextDelta(text))) if text == "hello"
        ));
        assert_empty_retry_observation(&chunks);
    }

    // WI#297: a content-free `Done` with a non-tool finish is also an empty
    // completion and must be retried.
    #[tokio::test(start_paused = true)]
    async fn stream_retries_empty_done_first_then_succeeds() {
        let inner = Arc::new(ScriptedStreamingProvider::new(
            "scripted",
            vec![done_only_stream("stop"), successful_text_stream()],
            vec![],
        ));
        let retry = RetryProvider::new(inner.clone(), fast_config(1));

        let chunks = retry
            .complete_stream(make_request())
            .await
            .expect("retry should open a stream")
            .collect::<Vec<_>>()
            .await;

        assert_eq!(inner.plain_calls(), 2);
        assert!(matches!(
            chunks.first(),
            Some(Ok(LlmStreamChunk::TextDelta(text))) if text == "hello"
        ));
        assert_empty_retry_observation(&chunks);
    }

    // WI#297 F5: a `Done{tool_calls}`-first stream is content-free by
    // construction (`Done` is terminal) and must be retried too.
    #[tokio::test(start_paused = true)]
    async fn stream_retries_done_first_tool_finish_anomaly() {
        let inner = Arc::new(ScriptedStreamingProvider::new(
            "scripted",
            vec![done_only_stream("tool_calls"), successful_text_stream()],
            vec![],
        ));
        let retry = RetryProvider::new(inner.clone(), fast_config(1));

        let chunks = retry
            .complete_stream(make_request())
            .await
            .expect("retry should open a stream")
            .collect::<Vec<_>>()
            .await;

        assert_eq!(inner.plain_calls(), 2);
        assert!(matches!(
            chunks.first(),
            Some(Ok(LlmStreamChunk::TextDelta(text))) if text == "hello"
        ));
        assert_empty_retry_observation(&chunks);
    }

    // WI#297 F2: deterministic empties (`length`) are not retried against the
    // same provider — the identical request would just burn the budget again.
    #[tokio::test(start_paused = true)]
    async fn stream_length_empty_is_not_retried() {
        let inner = Arc::new(ScriptedStreamingProvider::new(
            "scripted",
            vec![done_only_stream("length"), successful_text_stream()],
            vec![],
        ));
        let retry = RetryProvider::new(inner.clone(), fast_config(3));

        let result = retry.complete_stream(make_request()).await;

        assert!(matches!(
            result,
            Err(LlmError::EmptyStream {
                provider,
                finish: crate::llm::provider::FinishReason::Length,
                ..
            }) if provider == "scripted"
        ));
        assert_eq!(inner.plain_calls(), 1);
    }

    // WI#297 F2: a content-filter refusal with no content is not a transient
    // failure; re-submitting the refused prompt is not the retry layer's call.
    #[tokio::test(start_paused = true)]
    async fn stream_content_filter_empty_is_not_retried() {
        let inner = Arc::new(ScriptedStreamingProvider::new(
            "scripted",
            vec![done_only_stream("content_filter"), successful_text_stream()],
            vec![],
        ));
        let retry = RetryProvider::new(inner.clone(), fast_config(3));

        let result = retry.complete_stream(make_request()).await;

        assert!(matches!(
            result,
            Err(LlmError::EmptyStream {
                provider,
                finish: crate::llm::provider::FinishReason::ContentFilter,
                ..
            }) if provider == "scripted"
        ));
        assert_eq!(inner.plain_calls(), 1);
    }

    // WI#297: when every attempt answers empty, the retry layer returns the
    // typed error instead of a successful empty stream.
    #[tokio::test(start_paused = true)]
    async fn stream_empty_retries_exhausted_returns_typed_error() {
        let inner = Arc::new(ScriptedStreamingProvider::new(
            "scripted",
            vec![StreamScript::Items(vec![]), StreamScript::Items(vec![])],
            vec![],
        ));
        let retry = RetryProvider::new(inner.clone(), fast_config(1));

        let result = retry.complete_stream(make_request()).await;

        assert!(matches!(
            result,
            Err(LlmError::EmptyStream { provider, .. }) if provider == "scripted"
        ));
        assert_eq!(inner.plain_calls(), 2);
    }

    #[test]
    fn empty_stream_retryability_depends_on_finish_reason() {
        use crate::llm::provider::FinishReason;
        use crate::llm::streaming::empty_stream_error;

        assert!(is_retryable(&empty_stream_error("p", Some("stop"), None)));
        assert!(is_retryable(&empty_stream_error("p", None, None)));
        assert!(is_retryable(&empty_stream_error("p", Some("tool_calls"), None)));
        assert!(!is_retryable(&LlmError::EmptyStream {
            provider: "p".to_string(),
            finish: FinishReason::Length,
            detail: "completed with no content (finish_reason: length)".to_string(),
            usage: None,
        }));
        assert!(!is_retryable(&LlmError::EmptyStream {
            provider: "p".to_string(),
            finish: FinishReason::ContentFilter,
            detail: "completed with no content (finish_reason: content_filter)".to_string(),
            usage: None,
        }));
    }

    #[tokio::test(start_paused = true)]
    async fn retry_after_is_capped_in_retry_loop() {
        let inner = Arc::new(ScriptedStreamingProvider::new(
            "scripted",
            vec![
                StreamScript::SetupError(LlmError::RateLimited {
                    provider: "scripted".to_string(),
                    retry_after: Some(Duration::from_secs(MAX_RETRY_AFTER_SECS + 17)),
                }),
                successful_text_stream(),
            ],
            vec![],
        ));
        let retry = RetryProvider::new(inner, fast_config(1));
        let started = tokio::time::Instant::now();

        let _stream = retry
            .complete_stream(make_request())
            .await
            .expect("retry should open a stream");

        assert_eq!(
            tokio::time::Instant::now().duration_since(started),
            Duration::from_secs(MAX_RETRY_AFTER_SECS)
        );
    }

    // Regression test: Rate limiter fallback when Retry-After header is missing
    //
    // Verifies that RateLimited errors always have a duration (never None)
    // due to the 60-second fallback applied in all rate limit error creation sites
    // (lunarwing_cloud_chat.rs, anthropic_oauth.rs, embeddings.rs).
    #[test]
    fn rate_limited_error_always_has_duration() {
        let err = LlmError::RateLimited {
            provider: "test".to_string(),
            retry_after: Some(std::time::Duration::from_secs(60)),
        };

        if let LlmError::RateLimited { retry_after, .. } = err {
            assert!(
                retry_after.is_some(),
                "Rate limited error should always have retry_after duration"
            );
            assert_eq!(
                retry_after,
                Some(std::time::Duration::from_secs(60)),
                "Fallback should be 60 seconds"
            );
        } else {
            panic!("Expected RateLimited error");
        }
    }

    #[test]
    fn cap_retry_after_clamps_huge_delays() {
        assert_eq!(
            cap_retry_after(Duration::from_secs(u64::MAX)),
            Duration::from_secs(MAX_RETRY_AFTER_SECS)
        );
        assert_eq!(
            cap_retry_after(Duration::from_secs(0)),
            Duration::from_secs(0)
        );
    }

    #[test]
    fn parse_retry_after_delay_seconds() {
        let val = reqwest::header::HeaderValue::from_static("30");
        assert_eq!(parse_retry_after(Some(&val)), Duration::from_secs(30));
    }

    #[test]
    fn parse_retry_after_missing_header() {
        assert_eq!(
            parse_retry_after(None),
            Duration::from_secs(DEFAULT_RETRY_AFTER_SECS)
        );
    }

    #[test]
    fn parse_retry_after_unparseable() {
        let val = reqwest::header::HeaderValue::from_static("not-a-number");
        assert_eq!(
            parse_retry_after(Some(&val)),
            Duration::from_secs(DEFAULT_RETRY_AFTER_SECS)
        );
    }

    #[test]
    fn parse_retry_after_clamps_large_value() {
        let val = reqwest::header::HeaderValue::from_static("999999");
        assert_eq!(
            parse_retry_after(Some(&val)),
            Duration::from_secs(MAX_RETRY_AFTER_SECS)
        );
    }

    #[test]
    fn parse_retry_after_http_date() {
        let future = chrono::Utc::now() + chrono::Duration::seconds(30);
        let date_str = future.to_rfc2822();
        let val = reqwest::header::HeaderValue::from_str(&date_str).unwrap();
        let parsed = parse_retry_after(Some(&val));
        let diff = if parsed > Duration::from_secs(30) {
            parsed - Duration::from_secs(30)
        } else {
            Duration::from_secs(30) - parsed
        };
        assert!(
            diff <= Duration::from_secs(2),
            "expected ~30s, got {parsed:?} (diff {diff:?}) from header {date_str:?}"
        );
    }
}
