//! Desktop-facing voice gateway proxy handlers (`/api/voice/*`).
//!
//! Authenticated and rate-limited proxy endpoints that forward to the local
//! loopback LunarVoice sidecar. The sidecar URL and bearer token are held only
//! on the server — they are never exposed to desktop/browser clients.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use url::{Host, Url};

use crate::channels::web::auth::AuthenticatedUser;
use crate::channels::web::server::GatewayState;

/// Maximum transcription response body accepted from the sidecar.
const MAX_TRANSCRIBE_RESPONSE_BYTES: usize = 1024 * 1024;
/// Maximum synthesis response body accepted from the sidecar.
/// Synthesized WAV for ~5 minutes at 48 kHz mono is ~57 MB base64-decoded;
/// 4 MB covers ~40 s which is well above any reasonable interactive utterance.
const MAX_SYNTHESIZE_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Maximum characters allowed in a single synthesis request text.
const MAX_SYNTHESIZE_TEXT_CHARS: usize = 2000;

/// Default request body limit for voice endpoints (may be overridden per route).
/// Transcribe endpoint: 8 MB (base64 audio + JSON framing).
pub const VOICE_TRANSCRIBE_MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// Synthesize endpoint: 4 KB (text only, no audio upload).
pub const VOICE_SYNTHESIZE_MAX_BODY_BYTES: usize = 4096;

/// HTTP client timeout for calls to the loopback LunarVoice sidecar.
const VOICE_HTTP_TIMEOUT_SECS: u64 = 120;

/// Errors that can occur when constructing a `VoiceGatewayService`.
#[derive(Debug, thiserror::Error)]
pub enum VoiceGatewayError {
    /// VOICE_SERVICE_URL is not a valid URL.
    #[error("VOICE_SERVICE_URL is not a valid URL")]
    InvalidUrl,
    /// VOICE_SERVICE_URL must use `http://` scheme.
    #[error("VOICE_SERVICE_URL must use http://")]
    InvalidScheme,
    /// Credentials, query parameters, or fragments in URL are forbidden.
    #[error("VOICE_SERVICE_URL must not contain credentials, query parameters, or a fragment")]
    InvalidComponents,
    /// Host must be localhost or a loopback IP address.
    #[error("VOICE_SERVICE_URL must point to localhost or a loopback IP address")]
    NonLoopbackHost,
    /// URL must not contain a path.
    #[error("VOICE_SERVICE_URL must not contain a path")]
    InvalidPath,
    /// Failed to build the HTTP client.
    #[error("failed to build voice gateway HTTP client: {0}")]
    Client(#[source] reqwest::Error),
}

/// Server-side proxy to the loopback LunarVoice sidecar.
///
/// Holds the sidecar bearer token server-side; desktop clients authenticate
/// via the gateway's standard bearer auth and never see the sidecar token.
pub struct VoiceGatewayService {
    client: reqwest::Client,
    transcribe_url: Url,
    synthesize_url: Url,
    auth_token: SecretString,
}

impl VoiceGatewayService {
    /// Create a proxy targeting the loopback LunarVoice sidecar at `service_url` (e.g.
    /// `http://127.0.0.1:8090`).
    ///
    /// Returns an error when `service_url` is not loopback, uses a non-HTTP scheme,
    /// contains credentials/query/fragment, or contains a path.
    pub fn new(service_url: &str, auth_token: SecretString) -> Result<Self, VoiceGatewayError> {
        Self::with_timeout(
            service_url,
            auth_token,
            Duration::from_secs(VOICE_HTTP_TIMEOUT_SECS),
        )
    }

    fn with_timeout(
        service_url: &str,
        auth_token: SecretString,
        timeout: Duration,
    ) -> Result<Self, VoiceGatewayError> {
        let mut base = Url::parse(service_url).map_err(|_| VoiceGatewayError::InvalidUrl)?;
        if base.scheme() != "http" {
            return Err(VoiceGatewayError::InvalidScheme);
        }
        if !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(VoiceGatewayError::InvalidComponents);
        }
        let loopback = match base.host() {
            Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
            Some(Host::Ipv4(ip)) => ip.is_loopback(),
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        };
        if !loopback {
            return Err(VoiceGatewayError::NonLoopbackHost);
        }
        if base.path() != "/" {
            return Err(VoiceGatewayError::InvalidPath);
        }
        let mut transcribe_url = base.clone();
        transcribe_url.set_path("/v1/voice/transcribe");
        base.set_path("/v1/voice/synthesize");

        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(timeout)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(VoiceGatewayError::Client)?;

        Ok(Self {
            client,
            transcribe_url,
            synthesize_url: base,
            auth_token,
        })
    }

    async fn transcribe_request(
        &self,
        body: &VoiceTranscribeRequest,
    ) -> Result<VoiceTranscribeResponse, VoiceProxyError> {
        let response = self
            .client
            .post(self.transcribe_url.clone())
            .bearer_auth(self.auth_token.expose_secret())
            .json(body)
            .send()
            .await
            .map_err(VoiceProxyError::Upstream)?;
        let status = response.status();
        if !status.is_success() {
            return Err(VoiceProxyError::UpstreamStatus(status));
        }
        let bytes = read_bounded_response(response, MAX_TRANSCRIBE_RESPONSE_BYTES).await?;
        serde_json::from_slice(&bytes).map_err(VoiceProxyError::Decode)
    }

    async fn synthesize_request(
        &self,
        body: &VoiceSynthesizeRequest,
    ) -> Result<VoiceSynthesizeResponse, VoiceProxyError> {
        let response = self
            .client
            .post(self.synthesize_url.clone())
            .bearer_auth(self.auth_token.expose_secret())
            .json(body)
            .send()
            .await
            .map_err(VoiceProxyError::Upstream)?;
        let status = response.status();
        if !status.is_success() {
            return Err(VoiceProxyError::UpstreamStatus(status));
        }
        let bytes = read_bounded_response(response, MAX_SYNTHESIZE_RESPONSE_BYTES).await?;
        serde_json::from_slice(&bytes).map_err(VoiceProxyError::Decode)
    }
}

async fn read_bounded_response(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, VoiceProxyError> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(VoiceProxyError::ResponseTooLarge);
    }

    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(VoiceProxyError::Upstream)? {
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(VoiceProxyError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Errors from the sidecar proxy layer.
#[derive(Debug, thiserror::Error)]
pub enum VoiceProxyError {
    /// Network-level failure calling the sidecar.
    #[error("sidecar request failed: {0}")]
    Upstream(reqwest::Error),
    /// Sidecar returned a non-2xx status code.
    #[error("sidecar returned HTTP {0}")]
    UpstreamStatus(StatusCode),
    /// Sidecar response body exceeded the response size cap.
    #[error("sidecar response exceeded size limit")]
    ResponseTooLarge,
    /// Response body could not be decoded as the expected JSON shape.
    #[error("sidecar response decode error: {0}")]
    Decode(serde_json::Error),
}

impl VoiceProxyError {
    fn into_http_error(self) -> (StatusCode, String) {
        match &self {
            VoiceProxyError::Upstream(e) if e.is_timeout() => (
                StatusCode::GATEWAY_TIMEOUT,
                "Voice sidecar timed out".to_string(),
            ),
            VoiceProxyError::Upstream(e) if e.is_connect() || e.is_request() => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Voice sidecar unavailable".to_string(),
            ),
            VoiceProxyError::Upstream(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to contact voice sidecar".to_string(),
            ),
            VoiceProxyError::UpstreamStatus(status) => match status.as_u16() {
                401 => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Voice sidecar rejected gateway credentials".to_string(),
                ),
                429 => (
                    StatusCode::TOO_MANY_REQUESTS,
                    "Voice sidecar rate limited".to_string(),
                ),
                _ => (
                    StatusCode::BAD_GATEWAY,
                    format!("Voice sidecar error (HTTP {})", status.as_u16()),
                ),
            },
            VoiceProxyError::ResponseTooLarge => (
                StatusCode::BAD_GATEWAY,
                "Voice sidecar response exceeded size limit".to_string(),
            ),
            VoiceProxyError::Decode(_) => (
                StatusCode::BAD_GATEWAY,
                "Voice sidecar returned unexpected response".to_string(),
            ),
        }
    }
}

/// Wire-format transcription request accepted from the desktop client.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VoiceTranscribeRequest {
    /// Base64-encoded audio bytes (PCM or encoded container).
    pub audio: String,
    /// Audio format hint (must be supported by the sidecar).
    #[serde(default = "default_transcribe_format")]
    pub format: String,
    /// BCP 47 language tag or "auto".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Transcription model override (uses sidecar default when absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Wire-format transcription response returned to the desktop client.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VoiceTranscribeResponse {
    /// Full transcription text.
    pub text: String,
    /// Detected/normalized language.
    pub language: String,
    /// Model identifier used for transcription.
    pub model: String,
    /// Sidecar confidence score, when available.
    pub confidence: Option<f32>,
    /// Timestamped transcript segments returned by the sidecar.
    pub segments: Vec<VoiceTranscriptSegment>,
}

/// Timestamped portion of a transcription result.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VoiceTranscriptSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

fn default_transcribe_format() -> String {
    "wav".to_string()
}

/// Wire-format synthesis request accepted from the desktop client.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VoiceSynthesizeRequest {
    /// Text to synthesize.
    pub text: String,
    /// Voice identifier (uses sidecar default when absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// Language hint (uses sidecar default when absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Speech speed multiplier in `[0.5, 2.0]`.
    #[serde(default = "default_synthesize_speed")]
    pub speed: f32,
}

/// Wire-format synthesis response returned to the desktop client.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VoiceSynthesizeResponse {
    /// Base64-encoded WAV bytes.
    pub audio: String,
    /// Audio media format returned by the sidecar.
    pub format: String,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Duration in milliseconds.
    pub duration_ms: u64,
    /// Voice identifier used for synthesis.
    pub voice: String,
}

fn default_synthesize_speed() -> f32 {
    1.0
}

/// `POST /api/voice/transcribe` — transcribe an audio clip via the loopback LunarVoice sidecar.
pub async fn voice_transcribe_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(identity): AuthenticatedUser,
    Json(body): Json<VoiceTranscribeRequest>,
) -> Result<Json<VoiceTranscribeResponse>, (StatusCode, String)> {
    if !state.voice_rate_limiter.check(&identity.user_id) {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "Voice rate limit exceeded. Try again shortly.".to_string(),
        ));
    }

    let voice = state.voice_gateway.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Voice service not configured".to_string(),
    ))?;

    let req = body;
    if req.audio.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "audio must not be empty".to_string(),
        ));
    }
    if req.audio.len() > VOICE_TRANSCRIBE_MAX_BODY_BYTES.saturating_sub(256) {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "audio payload too large".to_string(),
        ));
    }

    let result = voice.transcribe_request(&req).await;
    let response = match result {
        Ok(r) => r,
        Err(err) => return Err(err.into_http_error()),
    };

    let log_text_len = response.text.chars().count();
    tracing::debug!(
        user = %identity.user_id,
        language = %response.language,
        model = %response.model,
        text_len = log_text_len,
        "Voice transcription complete"
    );

    Ok(Json(VoiceTranscribeResponse {
        text: response.text,
        language: response.language,
        model: response.model,
        confidence: response.confidence,
        segments: response.segments,
    }))
}

/// `POST /api/voice/synthesize` — synthesize text to audio via the loopback LunarVoice sidecar.
pub async fn voice_synthesize_handler(
    State(state): State<Arc<GatewayState>>,
    AuthenticatedUser(identity): AuthenticatedUser,
    Json(body): Json<VoiceSynthesizeRequest>,
) -> Result<Json<VoiceSynthesizeResponse>, (StatusCode, String)> {
    if !state.voice_rate_limiter.check(&identity.user_id) {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "Voice rate limit exceeded. Try again shortly.".to_string(),
        ));
    }

    let voice = state.voice_gateway.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Voice service not configured".to_string(),
    ))?;

    let req = body;
    let text_chars = req.text.chars().count();
    if text_chars == 0 {
        return Err((
            StatusCode::BAD_REQUEST,
            "text must not be empty".to_string(),
        ));
    }
    if text_chars > MAX_SYNTHESIZE_TEXT_CHARS {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("text too long ({text_chars} chars; max {MAX_SYNTHESIZE_TEXT_CHARS})"),
        ));
    }
    if !req.speed.is_finite() || !(0.5..=2.0).contains(&req.speed) {
        return Err((
            StatusCode::BAD_REQUEST,
            "speed must be between 0.5 and 2.0".to_string(),
        ));
    }

    let result = voice.synthesize_request(&req).await;
    let response = match result {
        Ok(r) => r,
        Err(err) => return Err(err.into_http_error()),
    };

    tracing::debug!(
        user = %identity.user_id,
        voice = %response.voice,
        duration_ms = response.duration_ms,
        text_len = text_chars,
        "Voice synthesis complete"
    );

    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use axum::Router;

    use super::*;
    use crate::channels::web::server::GatewayState;
    use crate::channels::web::test_helpers::TestGatewayBuilder;

    fn mock_voice_service(auth_token: &str) -> Result<VoiceGatewayService, VoiceGatewayError> {
        VoiceGatewayService::new("http://127.0.0.1:8090", SecretString::from(auth_token))
    }

    #[test]
    fn voice_service_accepts_loopback_urls() {
        assert!(VoiceGatewayService::new("http://127.0.0.1:8090", SecretString::from("t")).is_ok());
        assert!(VoiceGatewayService::new("http://localhost:8090", SecretString::from("t")).is_ok());
        assert!(VoiceGatewayService::new("http://[::1]:8090", SecretString::from("t")).is_ok());
    }

    #[test]
    fn voice_service_rejects_non_loopback() {
        assert!(
            VoiceGatewayService::new("http://192.168.1.5:8090", SecretString::from("t")).is_err()
        );
        assert!(
            VoiceGatewayService::new("https://127.0.0.1:8090", SecretString::from("t")).is_err()
        );
        assert!(
            VoiceGatewayService::new("http://user@127.0.0.1:8090", SecretString::from("t"))
                .is_err()
        );
        assert!(
            VoiceGatewayService::new("http://127.0.0.1:8090/path", SecretString::from("t"))
                .is_err()
        );
        assert!(
            VoiceGatewayService::new("http://127.0.0.1:8090?x=1", SecretString::from("t")).is_err()
        );
    }

    #[test]
    fn voice_service_endpoint_paths() {
        let svc = mock_voice_service("tok").expect("valid service");
        assert_eq!(
            svc.transcribe_url.as_str(),
            "http://127.0.0.1:8090/v1/voice/transcribe"
        );
        assert_eq!(
            svc.synthesize_url.as_str(),
            "http://127.0.0.1:8090/v1/voice/synthesize"
        );
    }

    #[test]
    fn absent_optional_fields_are_omitted_for_sidecar_defaults() {
        let transcribe = VoiceTranscribeRequest {
            audio: "aGk=".to_string(),
            format: "wav".to_string(),
            language: None,
            model: None,
        };
        let transcribe = serde_json::to_value(transcribe).expect("serialize transcription");
        assert!(transcribe.get("language").is_none());
        assert!(transcribe.get("model").is_none());

        let synthesize = VoiceSynthesizeRequest {
            text: "hello".to_string(),
            voice: None,
            language: None,
            speed: 1.0,
        };
        let synthesize = serde_json::to_value(synthesize).expect("serialize synthesis");
        assert!(synthesize.get("voice").is_none());
        assert!(synthesize.get("language").is_none());
    }

    #[test]
    fn test_builder_can_inject_voice_gateway() {
        let service = mock_voice_service("hello").expect("valid service");
        let state: Arc<GatewayState> = TestGatewayBuilder::new()
            .voice_gateway(Arc::new(service))
            .build();
        assert!(state.voice_gateway.is_some());
    }

    #[tokio::test]
    async fn sidecar_timeout_maps_to_gateway_timeout() {
        let app = Router::new().route(
            "/v1/voice/transcribe",
            axum::routing::post(|| async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Json(serde_json::json!({
                    "text": "late",
                    "language": "en",
                    "model": "mock",
                    "confidence": null,
                    "segments": []
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock sidecar");
        let port = listener.local_addr().expect("sidecar address").port();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve mock sidecar");
        });
        let service = VoiceGatewayService::with_timeout(
            &format!("http://127.0.0.1:{port}"),
            SecretString::from("token"),
            Duration::from_millis(20),
        )
        .expect("voice service");
        let request = VoiceTranscribeRequest {
            audio: "aGk=".to_string(),
            format: "wav".to_string(),
            language: None,
            model: None,
        };

        let error = service
            .transcribe_request(&request)
            .await
            .expect_err("request should time out");
        assert_eq!(error.into_http_error().0, StatusCode::GATEWAY_TIMEOUT);
        server.abort();
    }
}
