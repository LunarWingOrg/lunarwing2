use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::Json;
use axum::Router;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use dashmap::DashMap;
use governor::{Quota, RateLimiter};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::audio::{AudioFormat, decode_audio, encode_wav};
use crate::config::{Config, normalize_stt_language};
use crate::engine::{EngineError, SpeechSegment, TranscriptSegment, VoiceEngine};

type DirectRateLimiter = governor::RateLimiter<
    governor::state::NotKeyed,
    governor::state::InMemoryState,
    governor::clock::QuantaClock,
>;

#[derive(Clone)]
pub struct AppState {
    config: Arc<Config>,
    engine: Arc<dyn VoiceEngine>,
    limiter: Arc<DirectRateLimiter>,
    cache: Arc<DashMap<String, CachedTranscription>>,
    metrics: Arc<Metrics>,
    started_at: Instant,
}

impl AppState {
    pub fn new(config: Arc<Config>, engine: Arc<dyn VoiceEngine>) -> Self {
        let quota = Quota::per_second(
            NonZeroU32::new(config.rate_limit_per_second).unwrap_or(NonZeroU32::MIN),
        );
        Self {
            config,
            engine,
            limiter: Arc::new(RateLimiter::direct(quota)),
            cache: Arc::new(DashMap::new()),
            metrics: Arc::new(Metrics::default()),
            started_at: Instant::now(),
        }
    }
}

#[derive(Default)]
struct Metrics {
    total_requests: AtomicU64,
    transcription_requests: AtomicU64,
    synthesis_requests: AtomicU64,
    vad_requests: AtomicU64,
    cache_hits: AtomicU64,
    rate_limited: AtomicU64,
    errors: AtomicU64,
}

#[derive(Clone)]
struct CachedTranscription {
    response: TranscribeResponse,
    created_at: Instant,
}

#[derive(Debug, Deserialize)]
pub struct TranscribeRequest {
    pub audio: String,
    #[serde(default)]
    pub format: AudioFormat,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default)]
    pub model: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscribeResponse {
    pub text: String,
    pub language: String,
    pub confidence: Option<f32>,
    pub segments: Vec<TranscriptSegment>,
    pub model: String,
}

#[derive(Debug, Deserialize)]
pub struct SynthesizeRequest {
    pub text: String,
    #[serde(default)]
    pub voice: String,
    #[serde(default = "default_language")]
    #[serde(rename = "language")]
    pub _language: String,
    #[serde(default = "default_speed")]
    pub speed: f32,
}

#[derive(Debug, Serialize)]
pub struct SynthesizeResponse {
    pub audio: String,
    pub format: &'static str,
    pub sample_rate: u32,
    pub duration_ms: u64,
    pub voice: String,
}

#[derive(Debug, Deserialize)]
pub struct DetectRequest {
    pub audio: String,
    #[serde(default)]
    pub format: AudioFormat,
}

#[derive(Debug, Serialize)]
pub struct DetectResponse {
    pub segments: Vec<SpeechSegment>,
    pub has_speech: bool,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    uptime_secs: u64,
    models_loaded: Vec<String>,
}

#[derive(Debug, Serialize)]
struct MetricsResponse {
    total_requests: u64,
    transcription_requests: u64,
    synthesis_requests: u64,
    vad_requests: u64,
    cache_hits: u64,
    rate_limited: u64,
    errors: u64,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: &'static str,
    detail: String,
}

#[derive(Debug, thiserror::Error)]
enum ApiError {
    #[error("authorization required")]
    Unauthorized,
    #[error("rate limit exceeded")]
    RateLimited,
    #[error("invalid base64 audio")]
    InvalidBase64,
    #[error("{0}")]
    InvalidRequest(String),
    #[error("voice component unavailable")]
    Unavailable,
    #[error("voice processing failed")]
    Processing,
    #[error("voice processing timed out")]
    Timeout,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error) = match self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
            Self::InvalidBase64 | Self::InvalidRequest(_) => {
                (StatusCode::BAD_REQUEST, "bad_request")
            }
            Self::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            Self::Processing => (StatusCode::UNPROCESSABLE_ENTITY, "processing_failed"),
            Self::Timeout => (StatusCode::GATEWAY_TIMEOUT, "timeout"),
        };
        let body = ErrorResponse {
            error,
            detail: self.to_string(),
        };
        (status, Json(body)).into_response()
    }
}

pub fn api_router(state: AppState) -> Router {
    let audio_body_limit = state.config.max_body_bytes();
    let text_body_limit = state.config.max_text_body_bytes();
    Router::new()
        .route(
            "/v1/voice/transcribe",
            post(transcribe).layer(DefaultBodyLimit::max(audio_body_limit)),
        )
        .route(
            "/v1/voice/synthesize",
            post(synthesize).layer(DefaultBodyLimit::max(text_body_limit)),
        )
        .route(
            "/v1/voice/detect",
            post(detect).layer(DefaultBodyLimit::max(audio_body_limit)),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            authorize_api_request,
        ))
        .with_state(state)
}

pub fn health_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/metrics", get(metrics))
        .with_state(state)
}

async fn transcribe(
    State(state): State<AppState>,
    Json(mut request): Json<TranscribeRequest>,
) -> Result<Json<TranscribeResponse>, ApiError> {
    state
        .metrics
        .transcription_requests
        .fetch_add(1, Ordering::Relaxed);
    request.language = normalize_stt_language(&request.language)
        .map_err(|error| ApiError::InvalidRequest(error.to_string()))?;
    let key = transcription_cache_key(&request);
    if let Some(response) = cached_transcription(&state, &key) {
        state.metrics.cache_hits.fetch_add(1, Ordering::Relaxed);
        return Ok(Json(response));
    }

    let audio = decode_request_audio(&state, request.audio, request.format).await?;
    let model = if request.model.is_empty() {
        state.config.model_name.clone()
    } else {
        request.model
    };
    let result = tokio::time::timeout(
        Duration::from_secs(state.config.request_timeout_secs),
        state.engine.transcribe(audio, &request.language, &model),
    )
    .await
    .map_err(|_| ApiError::Timeout)?
    .map_err(map_engine_error)?;
    let response = TranscribeResponse {
        text: result.text,
        language: result.language,
        confidence: result.confidence,
        segments: result.segments,
        model,
    };
    store_transcription(&state, key, response.clone());
    Ok(Json(response))
}

async fn synthesize(
    State(state): State<AppState>,
    Json(request): Json<SynthesizeRequest>,
) -> Result<Json<SynthesizeResponse>, ApiError> {
    state
        .metrics
        .synthesis_requests
        .fetch_add(1, Ordering::Relaxed);
    if request.text.trim().is_empty() || request.text.chars().count() > state.config.max_text_chars
    {
        return Err(ApiError::InvalidRequest(format!(
            "text must contain 1..={} characters",
            state.config.max_text_chars
        )));
    }
    if !request.speed.is_finite() || !(0.5..=2.0).contains(&request.speed) {
        return Err(ApiError::InvalidRequest(
            "speed must be between 0.5 and 2.0".into(),
        ));
    }
    let voice = if request.voice.is_empty() {
        state.config.tts_default_voice.clone()
    } else {
        request.voice
    };
    let audio = tokio::time::timeout(
        Duration::from_secs(state.config.request_timeout_secs),
        state
            .engine
            .synthesize(&request.text, &voice, request.speed),
    )
    .await
    .map_err(|_| ApiError::Timeout)?
    .map_err(map_engine_error)?;
    if audio.sample_rate == 0
        || audio.samples.len() as u64
            > u64::from(audio.sample_rate).saturating_mul(state.config.max_tts_audio_secs)
    {
        return Err(ApiError::Processing);
    }
    let wav = encode_wav(&audio.samples, audio.sample_rate);
    Ok(Json(SynthesizeResponse {
        audio: base64::engine::general_purpose::STANDARD.encode(wav),
        format: "wav",
        sample_rate: audio.sample_rate,
        duration_ms: audio.samples.len() as u64 * 1_000 / u64::from(audio.sample_rate),
        voice,
    }))
}

async fn detect(
    State(state): State<AppState>,
    Json(request): Json<DetectRequest>,
) -> Result<Json<DetectResponse>, ApiError> {
    state.metrics.vad_requests.fetch_add(1, Ordering::Relaxed);
    let audio = decode_request_audio(&state, request.audio, request.format).await?;
    let segments = tokio::time::timeout(
        Duration::from_secs(state.config.request_timeout_secs),
        state.engine.detect(audio),
    )
    .await
    .map_err(|_| ApiError::Timeout)?
    .map_err(map_engine_error)?;
    Ok(Json(DetectResponse {
        has_speech: !segments.is_empty(),
        segments,
    }))
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let readiness = state.engine.readiness();
    Json(HealthResponse {
        status: "ok",
        uptime_secs: state.started_at.elapsed().as_secs(),
        models_loaded: readiness.models_loaded,
    })
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let readiness = state.engine.readiness();
    let status = if readiness.ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(readiness))
}

async fn metrics(State(state): State<AppState>) -> Json<MetricsResponse> {
    Json(MetricsResponse {
        total_requests: state.metrics.total_requests.load(Ordering::Relaxed),
        transcription_requests: state.metrics.transcription_requests.load(Ordering::Relaxed),
        synthesis_requests: state.metrics.synthesis_requests.load(Ordering::Relaxed),
        vad_requests: state.metrics.vad_requests.load(Ordering::Relaxed),
        cache_hits: state.metrics.cache_hits.load(Ordering::Relaxed),
        rate_limited: state.metrics.rate_limited.load(Ordering::Relaxed),
        errors: state.metrics.errors.load(Ordering::Relaxed),
    })
}

async fn authorize_api_request(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    state.metrics.total_requests.fetch_add(1, Ordering::Relaxed);
    if !authorized(request.headers(), state.config.auth_token.as_ref()) {
        state.metrics.errors.fetch_add(1, Ordering::Relaxed);
        return Err(ApiError::Unauthorized);
    }
    if state.limiter.check().is_err() {
        state.metrics.rate_limited.fetch_add(1, Ordering::Relaxed);
        state.metrics.errors.fetch_add(1, Ordering::Relaxed);
        return Err(ApiError::RateLimited);
    }
    let response = next.run(request).await;
    if response.status().is_client_error() || response.status().is_server_error() {
        state.metrics.errors.fetch_add(1, Ordering::Relaxed);
    }
    Ok(response)
}

fn authorized(headers: &HeaderMap, expected: Option<&SecretString>) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    let Some(provided) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    let expected = expected.expose_secret().as_bytes();
    provided.len() == expected.len() && bool::from(provided.as_bytes().ct_eq(expected))
}

async fn decode_request_audio(
    state: &AppState,
    encoded: String,
    format: AudioFormat,
) -> Result<crate::audio::AudioData, ApiError> {
    if encoded.len() > state.config.max_body_bytes() {
        return Err(ApiError::InvalidRequest(
            "encoded audio is too large".into(),
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| ApiError::InvalidBase64)?;
    decode_audio(
        bytes,
        format,
        state.config.max_audio_bytes,
        state.config.max_audio_secs,
        &state.config.ffmpeg_path,
        Duration::from_secs(state.config.request_timeout_secs),
    )
    .await
    .map_err(|error| ApiError::InvalidRequest(error.to_string()))
}

fn map_engine_error(error: EngineError) -> ApiError {
    match error {
        EngineError::ModelUnavailable(_) | EngineError::Busy => ApiError::Unavailable,
        EngineError::ModelMismatch | EngineError::LanguageMismatch | EngineError::UnknownVoice => {
            ApiError::InvalidRequest(error.to_string())
        }
        _ => ApiError::Processing,
    }
}

fn transcription_cache_key(request: &TranscribeRequest) -> String {
    let mut hasher = Sha256::new();
    hasher.update(request.audio.as_bytes());
    hasher.update(serde_json::to_vec(&request.format).unwrap_or_default());
    hasher.update(request.language.as_bytes());
    hasher.update(request.model.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn cached_transcription(state: &AppState, key: &str) -> Option<TranscribeResponse> {
    let entry = state.cache.get(key)?;
    if entry.created_at.elapsed().as_secs() < state.config.cache_ttl_secs {
        Some(entry.response.clone())
    } else {
        drop(entry);
        state.cache.remove(key);
        None
    }
}

fn store_transcription(state: &AppState, key: String, response: TranscribeResponse) {
    if state.cache.len() >= state.config.cache_max_entries {
        state
            .cache
            .retain(|_, entry| entry.created_at.elapsed().as_secs() < state.config.cache_ttl_secs);
    }
    if state.cache.len() >= state.config.cache_max_entries {
        let oldest = state
            .cache
            .iter()
            .min_by_key(|entry| entry.created_at)
            .map(|entry| entry.key().clone());
        if let Some(oldest) = oldest {
            state.cache.remove(&oldest);
        }
    }
    state.cache.insert(
        key,
        CachedTranscription {
            response,
            created_at: Instant::now(),
        },
    );
}

fn default_language() -> String {
    "auto".to_string()
}

fn default_speed() -> f32 {
    1.0
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};
    use std::path::PathBuf;

    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use secrecy::SecretString;
    use tower::ServiceExt;

    use super::*;
    use crate::audio::{AudioData, encode_wav};
    use crate::engine::{EngineReadiness, SynthesizedAudio, Transcription};

    struct MockEngine;

    #[async_trait]
    impl VoiceEngine for MockEngine {
        async fn transcribe(
            &self,
            _audio: AudioData,
            _language: &str,
            _model: &str,
        ) -> Result<Transcription, EngineError> {
            Ok(Transcription {
                text: "hello herd".into(),
                language: "en".into(),
                confidence: None,
                segments: vec![],
            })
        }

        async fn synthesize(
            &self,
            _text: &str,
            _voice: &str,
            _speed: f32,
        ) -> Result<SynthesizedAudio, EngineError> {
            Ok(SynthesizedAudio {
                samples: vec![0.0; 160],
                sample_rate: 16_000,
            })
        }

        async fn detect(&self, _audio: AudioData) -> Result<Vec<SpeechSegment>, EngineError> {
            Ok(vec![SpeechSegment {
                start_ms: 0,
                end_ms: 10,
            }])
        }

        fn readiness(&self) -> EngineReadiness {
            EngineReadiness {
                ready: true,
                stt_engine: Some("mock".into()),
                tts_engine: Some("mock".into()),
                vad_engine: Some("mock".into()),
                models_loaded: vec!["stt".into(), "tts".into(), "vad".into()],
            }
        }
    }

    fn test_config() -> Config {
        Config {
            bind_host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 8090,
            health_port: 8091,
            auth_token: Some(SecretString::from("test-token".to_string())),
            max_audio_bytes: 1024 * 1024,
            max_audio_secs: 10,
            max_text_chars: 100,
            max_tts_audio_secs: 10,
            request_timeout_secs: 2,
            max_concurrent_inference: 2,
            rate_limit_per_second: 100,
            cache_ttl_secs: 60,
            cache_max_entries: 10,
            ffmpeg_path: PathBuf::from("ffmpeg"),
            model_name: "mock".into(),
            stt_engine: crate::config::SttEngineKind::Whisper,
            stt_encoder: PathBuf::new(),
            stt_decoder: PathBuf::new(),
            stt_joiner: PathBuf::new(),
            stt_tokens: PathBuf::new(),
            stt_language: "auto".into(),
            vad_model: PathBuf::new(),
            tts_model: PathBuf::new(),
            tts_voices: PathBuf::new(),
            tts_tokens: PathBuf::new(),
            tts_data_dir: PathBuf::new(),
            tts_dict_dir: PathBuf::new(),
            tts_lexicon: String::new(),
            tts_default_voice: "af_heart".into(),
            tts_voice_ids: HashMap::from([("af_heart".into(), 0)]),
            enable_vad: true,
            enable_tts: true,
            require_stt: true,
            num_threads: 1,
        }
    }

    fn test_state() -> AppState {
        AppState::new(Arc::new(test_config()), Arc::new(MockEngine))
    }

    fn authorized_json_request(path: &str, body: impl Into<Body>) -> Request<Body> {
        Request::post(path)
            .header("content-type", "application/json")
            .header("authorization", "Bearer test-token")
            .body(body.into())
            .unwrap_or_else(|error| panic!("failed to construct test request: {error}"))
    }

    async fn response_json(response: Response) -> serde_json::Value {
        let body = response
            .into_body()
            .collect()
            .await
            .unwrap_or_else(|error| panic!("failed to collect response body: {error}"))
            .to_bytes();
        serde_json::from_slice(&body)
            .unwrap_or_else(|error| panic!("response was not JSON: {error}"))
    }

    #[tokio::test]
    async fn api_requires_bearer_token() {
        let response = api_router(test_state())
            .oneshot(
                Request::post("/v1/voice/transcribe")
                    .header("content-type", "application/json")
                    .body(Body::from("not-json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn transcribe_returns_mock_result() {
        let wav = encode_wav(&vec![0.0; 160], 16_000);
        let body = serde_json::json!({
            "audio": base64::engine::general_purpose::STANDARD.encode(wav),
            "format": "wav"
        });
        let response = api_router(test_state())
            .oneshot(
                Request::post("/v1/voice/transcribe")
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer test-token")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["text"], "hello herd");
        assert_eq!(body["model"], "mock");
    }

    #[tokio::test]
    async fn synthesize_and_detect_return_mock_results() {
        let state = test_state();
        let synthesis = api_router(state.clone())
            .oneshot(authorized_json_request(
                "/v1/voice/synthesize",
                serde_json::json!({"text": "hello herd"}).to_string(),
            ))
            .await
            .unwrap_or_else(|error| panic!("synthesis request failed: {error}"));
        assert_eq!(synthesis.status(), StatusCode::OK);
        let synthesis = response_json(synthesis).await;
        assert_eq!(synthesis["format"], "wav");
        assert_eq!(synthesis["voice"], "af_heart");
        assert_eq!(synthesis["duration_ms"], 10);

        let wav = encode_wav(&vec![0.0; 160], 16_000);
        let detection = api_router(state)
            .oneshot(authorized_json_request(
                "/v1/voice/detect",
                serde_json::json!({
                    "audio": base64::engine::general_purpose::STANDARD.encode(wav),
                    "format": "wav"
                })
                .to_string(),
            ))
            .await
            .unwrap_or_else(|error| panic!("detection request failed: {error}"));
        assert_eq!(detection.status(), StatusCode::OK);
        let detection = response_json(detection).await;
        assert_eq!(detection["has_speech"], true);
        assert_eq!(detection["segments"][0]["end_ms"], 10);
    }

    #[tokio::test]
    async fn api_metrics_count_auth_parse_and_cache_outcomes() {
        let state = test_state();
        let router = api_router(state.clone());
        let unauthorized = router
            .clone()
            .oneshot(
                Request::post("/v1/voice/transcribe")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap_or_else(|error| panic!("failed to construct test request: {error}")),
            )
            .await
            .unwrap_or_else(|error| panic!("unauthorized request failed: {error}"));
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let malformed = router
            .clone()
            .oneshot(authorized_json_request(
                "/v1/voice/transcribe",
                Body::from("{"),
            ))
            .await
            .unwrap_or_else(|error| panic!("malformed request failed: {error}"));
        assert!(malformed.status().is_client_error());

        let wav = encode_wav(&vec![0.0; 160], 16_000);
        let body = serde_json::json!({
            "audio": base64::engine::general_purpose::STANDARD.encode(wav),
            "format": "wav"
        })
        .to_string();
        for _ in 0..2 {
            let response = router
                .clone()
                .oneshot(authorized_json_request(
                    "/v1/voice/transcribe",
                    body.clone(),
                ))
                .await
                .unwrap_or_else(|error| panic!("transcription request failed: {error}"));
            assert_eq!(response.status(), StatusCode::OK);
        }

        assert_eq!(state.metrics.total_requests.load(Ordering::Relaxed), 4);
        assert_eq!(state.metrics.errors.load(Ordering::Relaxed), 2);
        assert_eq!(
            state.metrics.transcription_requests.load(Ordering::Relaxed),
            2
        );
        assert_eq!(state.metrics.cache_hits.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn synthesis_body_limit_is_route_specific_and_observable() {
        let state = test_state();
        let text = "x".repeat(state.config.max_text_body_bytes());
        let body = serde_json::json!({"text": text}).to_string();
        assert!(body.len() > state.config.max_text_body_bytes());
        assert!(body.len() < state.config.max_body_bytes());

        let response = api_router(state.clone())
            .oneshot(authorized_json_request(
                "/v1/voice/synthesize",
                Body::from(body),
            ))
            .await
            .unwrap_or_else(|error| panic!("oversized synthesis request failed: {error}"));
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(state.metrics.total_requests.load(Ordering::Relaxed), 1);
        assert_eq!(state.metrics.errors.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn rate_limit_rejections_update_operational_metrics() {
        let mut config = test_config();
        config.rate_limit_per_second = 1;
        let state = AppState::new(Arc::new(config), Arc::new(MockEngine));
        let router = api_router(state.clone());
        let body = serde_json::json!({"text": "hello"}).to_string();
        let first = router
            .clone()
            .oneshot(authorized_json_request(
                "/v1/voice/synthesize",
                body.clone(),
            ))
            .await
            .unwrap_or_else(|error| panic!("first synthesis request failed: {error}"));
        assert_eq!(first.status(), StatusCode::OK);
        let second = router
            .oneshot(authorized_json_request("/v1/voice/synthesize", body))
            .await
            .unwrap_or_else(|error| panic!("second synthesis request failed: {error}"));
        assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(state.metrics.total_requests.load(Ordering::Relaxed), 2);
        assert_eq!(state.metrics.rate_limited.load(Ordering::Relaxed), 1);
        assert_eq!(state.metrics.errors.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn health_and_ready_need_no_token() {
        let health = health_router(test_state())
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);

        let ready = health_router(test_state())
            .oneshot(Request::get("/ready").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(ready.status(), StatusCode::OK);
    }
}
