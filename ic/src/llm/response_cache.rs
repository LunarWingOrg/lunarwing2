//! In-memory LLM response cache with TTL and LRU eviction.
//!
//! Wraps any [`LlmProvider`] and caches [`complete()`] responses keyed
//! by a SHA-256 hash of the messages and model name. Tool-calling
//! requests are never cached since they can trigger side effects.
//!
//! ```text
//! ┌──────────────────────────────────────────────────┐
//! │               CachedProvider                      │
//! │  complete() ──► cache lookup ──► hit? return      │
//! │                                  miss? call inner │
//! │                                  store response   │
//! │                                                    │
//! │  complete_with_tools() ──► always call inner       │
//! └──────────────────────────────────────────────────┘
//! ```

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};

use crate::llm::error::LlmError;
use crate::llm::provider::{
    CompletionRequest, CompletionResponse, FinishReason, LlmProvider, LlmStream, LlmStreamChunk,
    ModelMetadata, TokenUsage, ToolCompletionRequest, ToolCompletionResponse,
};

/// How often (in requests) to emit a cache statistics log line.
const STATS_LOG_EVERY_N: u64 = 100;

/// Configuration for the response cache.
#[derive(Debug, Clone)]
pub struct ResponseCacheConfig {
    /// Time-to-live for cache entries.
    pub ttl: Duration,
    /// Maximum number of cached entries before LRU eviction.
    pub max_entries: usize,
}

impl Default for ResponseCacheConfig {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(3600), // 1 hour
            max_entries: 1000,
        }
    }
}

struct CacheEntry {
    response: CompletionResponse,
    created_at: Instant,
    last_accessed: Instant,
    hit_count: u64,
}

/// LLM provider wrapper that caches `complete()` responses.
///
/// Tool completion requests are always forwarded without caching since
/// tool calls can have side effects that should not be replayed.
pub struct CachedProvider {
    inner: Arc<dyn LlmProvider>,
    /// `std::sync::Mutex` (not tokio) — never held across an `.await` point,
    /// so blocking acquisition is safe and keeps `set_model()` synchronous.
    cache: Mutex<HashMap<String, CacheEntry>>,
    config: ResponseCacheConfig,
    /// Total `complete()` calls (hits + misses) for periodic stats logging.
    request_count: AtomicU64,
    /// Running total of cache hits, independent of entry lifecycle.
    /// Never decremented on eviction, so `hit_rate_pct` in stats doesn't
    /// drift down as entries expire or are LRU-evicted.
    total_hit_count: AtomicU64,
}

impl CachedProvider {
    /// Wrap an existing provider with response caching.
    pub fn new(inner: Arc<dyn LlmProvider>, config: ResponseCacheConfig) -> Self {
        Self {
            inner,
            cache: Mutex::new(HashMap::new()),
            config,
            request_count: AtomicU64::new(0),
            total_hit_count: AtomicU64::new(0),
        }
    }

    /// Number of entries currently in the cache.
    pub fn len(&self) -> usize {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }

    /// Total cache hits since this provider was created.
    ///
    /// Backed by an atomic counter that is never decremented on eviction,
    /// so the value is accurate even under high eviction pressure.
    pub fn total_hits(&self) -> u64 {
        self.total_hit_count.load(Ordering::Relaxed)
    }

    /// Clear all cached entries.
    pub fn clear(&self) {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// Emit a cache statistics log line if `req_no` is a multiple of
    /// [`STATS_LOG_EVERY_N`]. `total_hits` must come from the `total_hit_count`
    /// atomic so it accurately reflects hits that occurred on since-evicted
    /// entries. Must be called while holding the cache lock so that
    /// `entry_count` is consistent with the snapshot.
    fn maybe_log_stats(guard: &HashMap<String, CacheEntry>, req_no: u64, total_hits: u64) {
        if req_no.is_multiple_of(STATS_LOG_EVERY_N) {
            let hit_rate = total_hits as f64 / req_no as f64 * 100.0;
            tracing::info!(
                total_requests = req_no,
                total_hits,
                hit_rate_pct = format!("{hit_rate:.1}"),
                entry_count = guard.len(),
                "LLM response cache statistics"
            );
        }
    }

    fn lookup(&self, key: &str, now: Instant, request_number: u64) -> Option<CompletionResponse> {
        let mut guard = self.cache.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(entry) = guard.get_mut(key) {
            if now.duration_since(entry.created_at) < self.config.ttl {
                entry.last_accessed = now;
                entry.hit_count += 1;
                let hit_count = entry.hit_count;
                let response = entry.response.clone();
                tracing::trace!(hits = hit_count, "response cache hit");
                let _ = entry;
                let total_hits = self.total_hit_count.fetch_add(1, Ordering::Relaxed) + 1;
                Self::maybe_log_stats(&guard, request_number, total_hits);
                return Some(response);
            }
            guard.remove(key);
        }
        None
    }

    fn insert(&self, key: String, response: CompletionResponse, now: Instant, request_number: u64) {
        let mut guard = self.cache.lock().unwrap_or_else(|error| error.into_inner());
        guard.retain(|_, entry| now.duration_since(entry.created_at) < self.config.ttl);

        while guard.len() >= self.config.max_entries {
            let oldest_key = guard
                .iter()
                .min_by_key(|(_, entry)| entry.last_accessed)
                .map(|(key, _)| key.clone());
            let Some(oldest_key) = oldest_key else {
                break;
            };
            guard.remove(&oldest_key);
        }

        guard.insert(
            key,
            CacheEntry {
                response,
                created_at: now,
                last_accessed: now,
                hit_count: 0,
            },
        );

        let total_hits = self.total_hit_count.load(Ordering::Relaxed);
        Self::maybe_log_stats(&guard, request_number, total_hits);
    }

    fn log_miss_without_insert(&self, request_number: u64) {
        let guard = self.cache.lock().unwrap_or_else(|error| error.into_inner());
        let total_hits = self.total_hit_count.load(Ordering::Relaxed);
        Self::maybe_log_stats(&guard, request_number, total_hits);
    }

    fn cache_miss_stream<'a>(
        &'a self,
        inner: LlmStream<'a>,
        key: String,
        now: Instant,
        request_number: u64,
    ) -> LlmStream<'a> {
        futures::stream::unfold(
            (inner, key, String::new(), false),
            move |(mut inner, key, mut content, finished)| async move {
                if finished {
                    return None;
                }

                let Some(item) = inner.next().await else {
                    self.log_miss_without_insert(request_number);
                    return None;
                };

                match &item {
                    Ok(LlmStreamChunk::TextDelta(delta)) => content.push_str(delta),
                    Ok(LlmStreamChunk::Done {
                        usage,
                        finish_reason,
                    }) => {
                        let usage = usage.unwrap_or_default();
                        self.insert(
                            key.clone(),
                            CompletionResponse {
                                content: content.clone(),
                                input_tokens: usage.input_tokens,
                                output_tokens: usage.output_tokens,
                                finish_reason: finish_reason_from_stream(finish_reason),
                                cache_read_input_tokens: usage.cache_read_input_tokens,
                                cache_creation_input_tokens: usage.cache_creation_input_tokens,
                            },
                            now,
                            request_number,
                        );
                    }
                    Err(_) => self.log_miss_without_insert(request_number),
                    Ok(LlmStreamChunk::ToolCallDelta { .. }) => {}
                }

                let finished = matches!(&item, Ok(LlmStreamChunk::Done { .. }) | Err(_));
                Some((item, (inner, key, content, finished)))
            },
        )
        .boxed()
    }
}

fn finish_reason_from_stream(reason: &str) -> FinishReason {
    match reason {
        "stop" => FinishReason::Stop,
        "length" => FinishReason::Length,
        "tool_calls" => FinishReason::ToolUse,
        "content_filter" => FinishReason::ContentFilter,
        _ => FinishReason::Unknown,
    }
}

fn cached_stream(response: CompletionResponse) -> LlmStream<'static> {
    let usage = TokenUsage {
        input_tokens: response.input_tokens,
        output_tokens: response.output_tokens,
        cache_read_input_tokens: response.cache_read_input_tokens,
        cache_creation_input_tokens: response.cache_creation_input_tokens,
    };
    futures::stream::iter([
        Ok(LlmStreamChunk::TextDelta(response.content)),
        Ok(LlmStreamChunk::Done {
            usage: Some(usage),
            finish_reason: response.finish_reason.as_str().to_string(),
        }),
    ])
    .boxed()
}

/// Build a deterministic cache key from a completion request.
///
/// Hashes the model name, messages, and response-affecting parameters
/// (max_tokens, temperature, stop_sequences) via SHA-256. Two requests
/// with identical content and parameters produce the same key.
fn cache_key(model: &str, request: &CompletionRequest) -> String {
    let mut hasher = Sha256::new();
    hasher.update(model.as_bytes());
    hasher.update(b"|");

    // Messages are Serialize, so we can deterministically hash them.
    // serde_json produces stable output for the same input structure.
    if let Ok(json) = serde_json::to_string(&request.messages) {
        hasher.update(json.as_bytes());
    }

    // Include response-affecting parameters so different temperatures,
    // max_tokens, or stop sequences produce distinct cache keys.
    hasher.update(b"|");
    if let Some(max_tokens) = request.max_tokens {
        hasher.update(max_tokens.to_le_bytes());
    }
    hasher.update(b"|");
    if let Some(temp) = request.temperature {
        hasher.update(temp.to_le_bytes());
    }
    hasher.update(b"|");
    if let Some(ref stops) = request.stop_sequences {
        for s in stops {
            hasher.update(s.as_bytes());
            hasher.update(b"\x00");
        }
    }

    format!("{:x}", hasher.finalize())
}

#[async_trait]
impl LlmProvider for CachedProvider {
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
        let effective_model = self.inner.effective_model_name(request.model.as_deref());
        let key = cache_key(&effective_model, &request);
        let now = Instant::now();
        let req_no = self.request_count.fetch_add(1, Ordering::Relaxed) + 1;

        if let Some(response) = self.lookup(&key, now, req_no) {
            return Ok(response);
        }

        match self.inner.complete(request).await {
            Ok(response) => {
                self.insert(key, response.clone(), now, req_no);
                Ok(response)
            }
            Err(error) => {
                self.log_miss_without_insert(req_no);
                Err(error)
            }
        }
    }

    async fn complete_stream(&self, request: CompletionRequest) -> Result<LlmStream<'_>, LlmError> {
        let effective_model = self.inner.effective_model_name(request.model.as_deref());
        let key = cache_key(&effective_model, &request);
        let now = Instant::now();
        let request_number = self.request_count.fetch_add(1, Ordering::Relaxed) + 1;

        if let Some(response) = self.lookup(&key, now, request_number) {
            return Ok(cached_stream(response));
        }

        match self.inner.complete_stream(request).await {
            Ok(stream) => Ok(self.cache_miss_stream(stream, key, now, request_number)),
            Err(error) => {
                self.log_miss_without_insert(request_number);
                Err(error)
            }
        }
    }

    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        // Never cache tool calls; they can trigger side effects.
        self.inner.complete_with_tools(request).await
    }

    async fn complete_with_tools_stream(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        self.inner.complete_with_tools_stream(request).await
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
        // Cache keys embed the active model name via `effective_model_name()`, so
        // requests to the new model automatically land in a separate cache slot.
        // Entries for the old model remain valid: if we switch back, they will be
        // hit again rather than wasted. Natural TTL / LRU eviction cleans them up.
        self.inner.set_model(model)
    }

    fn calculate_cost(&self, input_tokens: u32, output_tokens: u32) -> Decimal {
        self.inner.calculate_cost(input_tokens, output_tokens)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use futures::StreamExt;
    use rust_decimal::Decimal;
    use tracing_test::traced_test;

    use crate::llm::error::LlmError;
    use crate::llm::provider::{
        ChatMessage, CompletionResponse, FinishReason, LlmStreamChunk, TokenUsage,
        ToolCompletionRequest, ToolCompletionResponse,
    };
    use crate::llm::response_cache::*;
    use crate::llm::streaming_test_support::{ScriptedStreamingProvider, StreamScript};
    use crate::testing::StubLlm;

    /// Minimal provider stub that supports `set_model()` — used to test
    /// per-model cache key isolation.
    struct SwitchableStub {
        call_count: AtomicU32,
        active_model: std::sync::RwLock<String>,
    }

    impl SwitchableStub {
        fn new() -> Self {
            Self {
                call_count: AtomicU32::new(0),
                active_model: std::sync::RwLock::new("stub-model".to_string()),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for SwitchableStub {
        fn model_name(&self) -> &str {
            "stub-model"
        }

        fn active_model_name(&self) -> String {
            self.active_model.read().unwrap().clone()
        }

        fn cost_per_token(&self) -> (Decimal, Decimal) {
            (Decimal::ZERO, Decimal::ZERO)
        }

        fn set_model(&self, model: &str) -> Result<(), LlmError> {
            *self.active_model.write().unwrap() = model.to_string();
            Ok(())
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, LlmError> {
            self.call_count.fetch_add(1, Ordering::Relaxed);
            Ok(CompletionResponse {
                content: "ok".into(),
                input_tokens: 1,
                output_tokens: 1,
                finish_reason: FinishReason::Stop,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            })
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, LlmError> {
            Ok(ToolCompletionResponse {
                content: Some("ok".into()),
                tool_calls: vec![],
                input_tokens: 1,
                output_tokens: 1,
                finish_reason: FinishReason::Stop,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            })
        }
    }

    fn simple_request() -> CompletionRequest {
        CompletionRequest {
            messages: vec![ChatMessage::user("hello")],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: Default::default(),
        }
    }

    fn different_request() -> CompletionRequest {
        CompletionRequest {
            messages: vec![ChatMessage::user("goodbye")],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: Default::default(),
        }
    }

    fn native_plain_stream() -> StreamScript {
        StreamScript::Items(vec![
            Ok(LlmStreamChunk::TextDelta("hel".to_string())),
            Ok(LlmStreamChunk::TextDelta("lo".to_string())),
            Ok(LlmStreamChunk::Done {
                usage: Some(TokenUsage {
                    input_tokens: 3,
                    output_tokens: 2,
                    cache_read_input_tokens: 1,
                    cache_creation_input_tokens: 0,
                }),
                finish_reason: "stop".to_string(),
            }),
        ])
    }

    fn native_tool_stream() -> StreamScript {
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
        ])
    }

    fn tool_request() -> ToolCompletionRequest {
        ToolCompletionRequest {
            messages: vec![ChatMessage::user("use tool")],
            tools: vec![],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            tool_choice: None,
            metadata: Default::default(),
        }
    }

    #[tokio::test]
    async fn stream_miss_forwards_chunks_and_populates_cache() {
        let provider = Arc::new(ScriptedStreamingProvider::new(
            "scripted",
            vec![native_plain_stream()],
            vec![],
        ));
        let cached = CachedProvider::new(provider.clone(), ResponseCacheConfig::default());

        let chunks = cached
            .complete_stream(simple_request())
            .await
            .expect("stream should open")
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("stream should complete");

        assert_eq!(
            chunks,
            vec![
                LlmStreamChunk::TextDelta("hel".to_string()),
                LlmStreamChunk::TextDelta("lo".to_string()),
                LlmStreamChunk::Done {
                    usage: Some(TokenUsage {
                        input_tokens: 3,
                        output_tokens: 2,
                        cache_read_input_tokens: 1,
                        cache_creation_input_tokens: 0,
                    }),
                    finish_reason: "stop".to_string(),
                },
            ]
        );
        assert_eq!(provider.plain_calls(), 1);
        assert_eq!(cached.len(), 1);

        let blocking_hit = cached.complete(simple_request()).await.unwrap();
        assert_eq!(blocking_hit.content, "hello");
        assert_eq!(blocking_hit.input_tokens, 3);
        assert_eq!(blocking_hit.output_tokens, 2);
    }

    #[tokio::test]
    async fn stream_hit_emits_cached_text_and_done() {
        let provider = Arc::new(ScriptedStreamingProvider::new(
            "scripted",
            vec![native_plain_stream()],
            vec![],
        ));
        let cached = CachedProvider::new(provider.clone(), ResponseCacheConfig::default());

        cached
            .complete_stream(simple_request())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let chunks = cached
            .complete_stream(simple_request())
            .await
            .expect("cached stream should open")
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("cached stream should complete");

        assert_eq!(
            chunks,
            vec![
                LlmStreamChunk::TextDelta("hello".to_string()),
                LlmStreamChunk::Done {
                    usage: Some(TokenUsage {
                        input_tokens: 3,
                        output_tokens: 2,
                        cache_read_input_tokens: 1,
                        cache_creation_input_tokens: 0,
                    }),
                    finish_reason: "stop".to_string(),
                },
            ]
        );
        assert_eq!(provider.plain_calls(), 1);
        assert_eq!(cached.total_hits(), 1);
    }

    #[tokio::test]
    async fn stream_error_does_not_populate_cache() {
        let provider = Arc::new(ScriptedStreamingProvider::new(
            "scripted",
            vec![StreamScript::Items(vec![
                Ok(LlmStreamChunk::TextDelta("partial".to_string())),
                Err(LlmError::RequestFailed {
                    provider: "scripted".to_string(),
                    reason: "stream failed".to_string(),
                }),
            ])],
            vec![],
        ));
        let cached = CachedProvider::new(provider, ResponseCacheConfig::default());

        let chunks = cached
            .complete_stream(simple_request())
            .await
            .expect("stream should open")
            .collect::<Vec<_>>()
            .await;

        assert!(matches!(
            chunks.last(),
            Some(Err(LlmError::RequestFailed { .. }))
        ));
        assert!(cached.is_empty());
    }

    #[tokio::test]
    async fn stream_eof_without_done_does_not_populate_cache() {
        let provider = Arc::new(ScriptedStreamingProvider::new(
            "scripted",
            vec![StreamScript::Items(vec![Ok(LlmStreamChunk::TextDelta(
                "partial".to_string(),
            ))])],
            vec![],
        ));
        let cached = CachedProvider::new(provider, ResponseCacheConfig::default());

        let chunks = cached
            .complete_stream(simple_request())
            .await
            .expect("stream should open")
            .collect::<Vec<_>>()
            .await;

        assert!(matches!(
            chunks.as_slice(),
            [Ok(LlmStreamChunk::TextDelta(text))] if text == "partial"
        ));
        assert!(cached.is_empty());
    }

    #[tokio::test]
    async fn blocking_completion_and_stream_share_cache_entry() {
        let provider = Arc::new(ScriptedStreamingProvider::new(
            "scripted",
            vec![native_plain_stream()],
            vec![],
        ));
        let cached = CachedProvider::new(provider.clone(), ResponseCacheConfig::default());

        let blocking = cached.complete(simple_request()).await.unwrap();
        let stream_chunks = cached
            .complete_stream(simple_request())
            .await
            .expect("cached stream should open")
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(blocking.content, "blocking response");
        assert!(matches!(
            stream_chunks.as_slice(),
            [LlmStreamChunk::TextDelta(text), LlmStreamChunk::Done { .. }]
                if text == "blocking response"
        ));
        assert_eq!(provider.plain_calls(), 0);
        assert_eq!(cached.len(), 1);
    }

    #[tokio::test]
    async fn tool_stream_bypasses_cache() {
        let provider = Arc::new(ScriptedStreamingProvider::new(
            "scripted",
            vec![],
            vec![native_tool_stream(), native_tool_stream()],
        ));
        let cached = CachedProvider::new(provider.clone(), ResponseCacheConfig::default());

        for _ in 0..2 {
            let chunks = cached
                .complete_with_tools_stream(tool_request())
                .await
                .expect("tool stream should open")
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
                .expect("tool stream should complete");
            assert!(matches!(
                chunks.as_slice(),
                [
                    LlmStreamChunk::ToolCallDelta { .. },
                    LlmStreamChunk::Done { .. }
                ]
            ));
        }

        assert_eq!(provider.tool_calls(), 2);
        assert!(cached.is_empty());
    }

    #[test]
    fn cache_key_is_deterministic() {
        let req = simple_request();
        let k1 = cache_key("model-a", &req);
        let k2 = cache_key("model-a", &req);
        assert_eq!(k1, k2);
        assert_eq!(k1.len(), 64); // SHA-256 hex
    }

    #[test]
    fn cache_key_varies_by_model() {
        let req = simple_request();
        let k1 = cache_key("model-a", &req);
        let k2 = cache_key("model-b", &req);
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_varies_by_messages() {
        let k1 = cache_key("model-a", &simple_request());
        let k2 = cache_key("model-a", &different_request());
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_varies_by_temperature() {
        let mut req_a = simple_request();
        req_a.temperature = Some(0.0);
        let mut req_b = simple_request();
        req_b.temperature = Some(1.0);
        assert_ne!(cache_key("m", &req_a), cache_key("m", &req_b));
    }

    #[test]
    fn cache_key_varies_by_max_tokens() {
        let mut req_a = simple_request();
        req_a.max_tokens = Some(100);
        let mut req_b = simple_request();
        req_b.max_tokens = Some(500);
        assert_ne!(cache_key("m", &req_a), cache_key("m", &req_b));
    }

    #[tokio::test]
    async fn cache_hit_avoids_provider_call() {
        let stub = Arc::new(StubLlm::new("cached response"));
        let cached = CachedProvider::new(
            stub.clone(),
            ResponseCacheConfig {
                ttl: Duration::from_secs(60),
                max_entries: 100,
            },
        );

        // First call: cache miss
        let r1 = cached.complete(simple_request()).await.unwrap();
        assert_eq!(stub.calls(), 1);
        assert_eq!(r1.content, "cached response");

        // Second call: cache hit
        let r2 = cached.complete(simple_request()).await.unwrap();
        assert_eq!(stub.calls(), 1); // still 1
        assert_eq!(r2.content, "cached response");

        assert_eq!(cached.total_hits(), 1);
    }

    #[tokio::test]
    async fn different_messages_get_different_entries() {
        let stub = Arc::new(StubLlm::new("cached response"));
        let cached = CachedProvider::new(stub.clone(), ResponseCacheConfig::default());

        cached.complete(simple_request()).await.unwrap();
        cached.complete(different_request()).await.unwrap();

        assert_eq!(stub.calls(), 2);
        assert_eq!(cached.len(), 2);
    }

    #[tokio::test]
    async fn expired_entries_are_evicted() {
        let stub = Arc::new(StubLlm::new("cached response"));
        let cached = CachedProvider::new(
            stub.clone(),
            ResponseCacheConfig {
                ttl: Duration::from_millis(1),
                max_entries: 100,
            },
        );

        cached.complete(simple_request()).await.unwrap();
        assert_eq!(stub.calls(), 1);

        // Wait for TTL to expire
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Should be a cache miss now
        cached.complete(simple_request()).await.unwrap();
        assert_eq!(stub.calls(), 2);
    }

    #[tokio::test]
    async fn lru_eviction_removes_oldest() {
        let stub = Arc::new(StubLlm::new("cached response"));
        let cached = CachedProvider::new(
            stub.clone(),
            ResponseCacheConfig {
                ttl: Duration::from_secs(60),
                max_entries: 2,
            },
        );

        // Fill cache with 2 entries
        cached.complete(simple_request()).await.unwrap();
        cached.complete(different_request()).await.unwrap();
        assert_eq!(cached.len(), 2);

        // Add a third: should evict the oldest
        let third = CompletionRequest {
            messages: vec![ChatMessage::user("third")],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: Default::default(),
        };
        cached.complete(third).await.unwrap();
        assert_eq!(cached.len(), 2);
        assert_eq!(stub.calls(), 3);
    }

    #[tokio::test]
    async fn tool_calls_are_never_cached() {
        let stub = Arc::new(StubLlm::new("cached response"));
        let cached = CachedProvider::new(stub.clone(), ResponseCacheConfig::default());

        let req = ToolCompletionRequest {
            messages: vec![ChatMessage::user("use tool")],
            tools: vec![],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            tool_choice: None,
            metadata: Default::default(),
        };

        cached.complete_with_tools(req.clone()).await.unwrap();
        cached.complete_with_tools(req).await.unwrap();

        // Both should have called through
        assert_eq!(stub.calls(), 2);
        assert!(cached.is_empty());
    }

    #[tokio::test]
    async fn provider_errors_are_not_cached() {
        let stub = Arc::new(StubLlm::new("cached response"));
        let cached = CachedProvider::new(
            stub.clone(),
            ResponseCacheConfig {
                ttl: Duration::from_secs(60),
                max_entries: 100,
            },
        );

        stub.set_failing(true);
        let result = cached.complete(simple_request()).await;
        assert!(result.is_err());
        assert!(cached.is_empty());

        // After fixing the provider, should succeed and cache
        stub.set_failing(false);
        cached.complete(simple_request()).await.unwrap();
        assert_eq!(cached.len(), 1);
    }

    #[tokio::test]
    async fn clear_empties_cache() {
        let stub = Arc::new(StubLlm::new("cached response"));
        let cached = CachedProvider::new(stub.clone(), ResponseCacheConfig::default());

        cached.complete(simple_request()).await.unwrap();
        assert_eq!(cached.len(), 1);

        cached.clear();
        assert!(cached.is_empty());
    }

    #[tokio::test]
    async fn model_override_gets_distinct_cache_entries() {
        let stub = Arc::new(StubLlm::new("cached response"));
        let cached = CachedProvider::new(stub.clone(), ResponseCacheConfig::default());

        let mut req_a = simple_request();
        req_a.model = Some("model-a".to_string());
        let mut req_b = simple_request();
        req_b.model = Some("model-b".to_string());

        cached.complete(req_a).await.unwrap();
        cached.complete(req_b).await.unwrap();

        assert_eq!(stub.calls(), 2);
        assert_eq!(cached.len(), 2);
    }

    #[test]
    fn default_config_is_reasonable() {
        let cfg = ResponseCacheConfig::default();
        assert_eq!(cfg.ttl, Duration::from_secs(3600));
        assert_eq!(cfg.max_entries, 1000);
    }

    #[tokio::test]
    async fn delegates_model_name() {
        let stub = Arc::new(StubLlm::new("cached response"));
        let cached = CachedProvider::new(stub.clone(), ResponseCacheConfig::default());
        assert_eq!(cached.model_name(), "stub-model");
    }

    /// Switching models preserves existing cached entries and routes subsequent
    /// requests to a separate cache slot. Switching back replays the old slot.
    #[tokio::test]
    async fn set_model_isolates_per_model_via_key() {
        let stub = Arc::new(SwitchableStub::new());
        let cached = CachedProvider::new(stub.clone(), ResponseCacheConfig::default());

        // Populate cache under the initial model ("stub-model").
        cached.complete(simple_request()).await.unwrap();
        assert_eq!(stub.call_count.load(Ordering::Relaxed), 1);
        assert_eq!(cached.len(), 1, "one entry cached for stub-model");

        // Switch to a different model — old entries must survive.
        cached.set_model("model-b").unwrap();
        assert_eq!(cached.len(), 1, "old entries preserved after model switch");

        // Same request under model-b is a cache miss (different key).
        cached.complete(simple_request()).await.unwrap();
        assert_eq!(
            stub.call_count.load(Ordering::Relaxed),
            2,
            "cache miss for model-b"
        );
        assert_eq!(cached.len(), 2, "separate slots for stub-model and model-b");

        // Switch back — original slot is still valid (cache hit, no extra call).
        cached.set_model("stub-model").unwrap();
        cached.complete(simple_request()).await.unwrap();
        assert_eq!(
            stub.call_count.load(Ordering::Relaxed),
            2,
            "cache hit when switching back to stub-model"
        );
    }

    /// When `set_model()` fails the error is propagated and the cache is unaffected.
    #[tokio::test]
    async fn set_model_error_leaves_cache_intact() {
        // StubLlm does not override set_model() — returns an error by default.
        let stub = Arc::new(StubLlm::default());
        let cached = CachedProvider::new(stub, ResponseCacheConfig::default());

        cached.complete(simple_request()).await.unwrap();
        assert_eq!(cached.len(), 1);

        let result = cached.set_model("new-model");
        assert!(result.is_err());
        assert_eq!(cached.len(), 1, "cache unaffected by failed set_model");
    }

    /// `hit_rate_pct` stays accurate even after entries are evicted.
    /// The `total_hit_count` atomic is never decremented on eviction.
    #[tokio::test]
    async fn total_hits_survives_eviction() {
        let stub = Arc::new(StubLlm::new("response"));
        // max_entries = 1 so the first entry is LRU-evicted when a second arrives.
        let cached = CachedProvider::new(
            stub.clone(),
            ResponseCacheConfig {
                ttl: Duration::from_secs(60),
                max_entries: 1,
            },
        );

        // Populate the cache and score a hit.
        cached.complete(simple_request()).await.unwrap();
        cached.complete(simple_request()).await.unwrap();
        assert_eq!(cached.total_hits(), 1);

        // Add a different request — LRU evicts the first entry.
        cached.complete(different_request()).await.unwrap();
        assert_eq!(cached.len(), 1, "first entry was evicted");

        // The hit from the evicted entry must still be counted.
        assert_eq!(cached.total_hits(), 1, "hit count survives eviction");
    }

    /// A stats line is emitted exactly at the 100th request.
    #[tokio::test]
    #[traced_test]
    async fn stats_logged_at_request_100() {
        let stub = Arc::new(StubLlm::new("response"));
        let cached = CachedProvider::new(
            stub.clone(),
            ResponseCacheConfig {
                ttl: Duration::from_secs(60),
                max_entries: 2000,
            },
        );

        // 99 distinct requests — no stats line yet.
        for i in 0..99u32 {
            let req = CompletionRequest {
                messages: vec![ChatMessage::user(format!("request {i}"))],
                model: None,
                max_tokens: None,
                temperature: None,
                stop_sequences: None,
                metadata: Default::default(),
            };
            cached.complete(req).await.unwrap();
        }
        assert!(
            !logs_contain("LLM response cache statistics"),
            "no stats before request 100"
        );

        // 100th request triggers the first stats line.
        let req = CompletionRequest {
            messages: vec![ChatMessage::user("request 99")],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: Default::default(),
        };
        cached.complete(req).await.unwrap();
        assert!(
            logs_contain("LLM response cache statistics"),
            "stats emitted at request 100"
        );
    }

    /// Stats are emitted even when the inner provider returns an error.
    #[tokio::test]
    #[traced_test]
    async fn stats_logged_on_provider_error_at_interval() {
        let stub = Arc::new(StubLlm::new("response"));
        let cached = CachedProvider::new(
            stub.clone(),
            ResponseCacheConfig {
                ttl: Duration::from_secs(60),
                max_entries: 2000,
            },
        );

        // 99 successful requests.
        for i in 0..99u32 {
            let req = CompletionRequest {
                messages: vec![ChatMessage::user(format!("req {i}"))],
                model: None,
                max_tokens: None,
                temperature: None,
                stop_sequences: None,
                metadata: Default::default(),
            };
            cached.complete(req).await.unwrap();
        }

        // 100th request fails — stats must still be logged.
        stub.set_failing(true);
        let req = CompletionRequest {
            messages: vec![ChatMessage::user("req 99")],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: Default::default(),
        };
        let result = cached.complete(req).await;
        assert!(result.is_err());
        assert!(
            logs_contain("LLM response cache statistics"),
            "stats emitted even when provider errors on request 100"
        );
    }
}
