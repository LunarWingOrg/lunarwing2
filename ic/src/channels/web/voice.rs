//! Desktop-facing voice API (`/api/voice/*`): authenticated, bounded proxy to
//! the loopback LunarVoice sidecar (WI#131).
//!
//! The gateway holds the sidecar credential (`VOICE_AUTH_TOKEN`); clients
//! authenticate with their normal gateway bearer token and never see the
//! sidecar secret. All requests are bounded (base64 audio length, text
//! length, voice/speed/format validation) and responses are read with a hard
//! byte cap before being re-serialized into gateway-owned response types.

use std::sync::Arc;
use std::time::Duration;

use axum::{Json, extract::State, http::StatusCode};
use base64::Engine;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::channels::web::auth::AuthenticatedUser;
use crate::channels::web::server::GatewayState;
use crate::llm::transcription::voice_endpoint;

/// Maximum accepted base64 audio payload (~18 MiB decoded).
pub const MAX_AUDIO_B64_LEN: usize = 24 * 1024 * 1024;
/// Maximum synthesis text length in chars (mirrors the sidecar default).
pub const MAX_SYNTH_TEXT_CHARS: usize = 10_000;
/// Maximum transcription response body (text-only JSON).
const MAX_TRANSCRIBE_RESPONSE_BYTES: usize = 1024 * 1024;
/// Maximum synthesis response body (base64 WAV of a bounded clip).
const MAX_SYNTHESIZE_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
/// Request body limits applied per route (override the global gateway limit).
pub const TRANSCRIBE_BODY_LIMIT: usize = 32 * 1024 * 1024;
pub const SYNTHESIZE_BODY_LIMIT: usize = 1024 * 1024;

/// Audio container formats the sidecar accepts (`lowercase` wire form).
const AUDIO_FORMATS: [&str; 9] = [
    "wav", "ogg", "opus", "mp3", "mp4", "webm", "flac", "m4a", "aac",
];

#[derive(Debug, thiserror::Error)]
enum VoiceProxyError {
    #[error("voice request failed: {0}")]
    Request(String),
    #[error("voice service returned an invalid response")]
    InvalidResponse,
    #[error("voice service error (HTTP {status}): {detail}")]
    Upstream { status: u16, detail: String },
}

impl VoiceProxyError {
    fn gateway_status(&self) -> StatusCode {
        match self {
            Self::Request(_) | Self::InvalidResponse => StatusCode::BAD_GATEWAY,
            Self::Upstream { status, .. } => {
                StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
            }
        }
    }
}

/// HTTP client for the loopback LunarVoice sidecar.
///
/// Endpoint construction reuses the provider's invariants: plain HTTP,
/// loopback host only, no credentials/query/fragment/path in the base URL.
pub struct VoiceProxyClient {
    client: reqwest::Client,
    transcribe: Url,
    synthesize: Url,
    auth_token: SecretString,
    model: String,
}

impl VoiceProxyClient {
    pub fn new(
        service_url: &str,
        auth_token: SecretString,
        model: &str,
    ) -> Result<Self, crate::llm::transcription::LunarVoiceConfigError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(120))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(crate::llm::transcription::LunarVoiceConfigError::Client)?;
        Ok(Self {
            client,
            transcribe: voice_endpoint(service_url, "/v1/voice/transcribe")?,
            synthesize: voice_endpoint(service_url, "/v1/voice/synthesize")?,
            auth_token,
            model: model.to_string(),
        })
    }

    /// Model the gateway selects on the client's behalf.
    pub fn model(&self) -> &str {
        &self.model
    }

    async fn transcribe(
        &self,
        request: SidecarTranscribeRequest<'_>,
    ) -> Result<SidecarTranscribeResponse, VoiceProxyError> {
        let bytes = self
            .send_bounded(&self.transcribe, &request, MAX_TRANSCRIBE_RESPONSE_BYTES)
            .await?;
        serde_json::from_slice(&bytes).map_err(|_| VoiceProxyError::InvalidResponse)
    }

    async fn synthesize(
        &self,
        request: SidecarSynthesizeRequest<'_>,
    ) -> Result<SidecarSynthesizeResponse, VoiceProxyError> {
        let bytes = self
            .send_bounded(&self.synthesize, &request, MAX_SYNTHESIZE_RESPONSE_BYTES)
            .await?;
        serde_json::from_slice(&bytes).map_err(|_| VoiceProxyError::InvalidResponse)
    }

    /// POST JSON to the sidecar and read the body with a hard byte cap.
    async fn send_bounded(
        &self,
        endpoint: &Url,
        body: &impl Serialize,
        max_bytes: usize,
    ) -> Result<Vec<u8>, VoiceProxyError> {
        let mut response = self
            .client
            .post(endpoint.clone())
            .bearer_auth(self.auth_token.expose_secret())
            .json(body)
            .send()
            .await
            .map_err(|error| VoiceProxyError::Request(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let detail = response
                .json::<SidecarErrorResponse>()
                .await
                .map(|body| body.detail)
                .unwrap_or_else(|_| status.canonical_reason().unwrap_or("unknown").to_string());
            return Err(VoiceProxyError::Upstream {
                status: status.as_u16(),
                detail,
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > max_bytes as u64)
        {
            return Err(VoiceProxyError::InvalidResponse);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| VoiceProxyError::Request(error.to_string()))?
        {
            if bytes.len().saturating_add(chunk.len()) > max_bytes {
                return Err(VoiceProxyError::InvalidResponse);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

/// Gateway-side voice API state: present only when the LunarVoice sidecar
/// credential is configured (server-held; clients never see it).
pub struct VoiceApi {
    client: Arc<VoiceProxyClient>,
}

impl VoiceApi {
    /// Build the proxy from resolved transcription config.
    ///
    /// Returns `None` unless the provider is `lunarvoice` and the dedicated
    /// loopback token is present. Independent of `TRANSCRIPTION_ENABLED`,
    /// which only gates the agent-side attachment middleware.
    pub fn from_config(config: &crate::config::TranscriptionConfig) -> Option<Self> {
        if config.provider != "lunarvoice" {
            return None;
        }
        let token = config.voice_auth_token.clone()?;
        let service_url = config
            .voice_service_url
            .as_deref()
            .unwrap_or("http://127.0.0.1:8090");
        match VoiceProxyClient::new(service_url, token, &config.model) {
            Ok(client) => {
                tracing::info!(
                    model = %config.model,
                    "Gateway voice API enabled via loopback LunarVoice"
                );
                Some(Self {
                    client: Arc::new(client),
                })
            }
            Err(error) => {
                tracing::error!(error = %error, "Invalid LunarVoice configuration for gateway voice API");
                None
            }
        }
    }

    fn client(&self) -> &Arc<VoiceProxyClient> {
        &self.client
    }
}

// ---------------------------------------------------------------------------
// Wire types (gateway-owned; mirror the sidecar contract)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct VoiceTranscribeRequest {
    /// Base64-encoded audio bytes.
    pub audio: String,
    /// Container format (one of `AUDIO_FORMATS`). Defaults to `wav`.
    #[serde(default = "default_format")]
    pub format: String,
    /// Optional BCP-47-ish language hint; `auto` when omitted.
    #[serde(default)]
    pub language: Option<String>,
}

fn default_format() -> String {
    "wav".to_string()
}

#[derive(Debug, Serialize)]
pub struct VoiceTranscribeResponse {
    pub text: String,
    pub language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<VoiceSegment>,
    pub model: String,
}

#[derive(Debug, Serialize)]
pub struct VoiceSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct VoiceSynthesizeRequest {
    pub text: String,
    /// Voice id; the sidecar applies its default when omitted.
    #[serde(default)]
    pub voice: Option<String>,
    /// Speech rate; defaults to 1.0, clamped server-side to 0.5..=2.0.
    #[serde(default)]
    pub speed: Option<f32>,
}

#[derive(Debug, Serialize)]
pub struct VoiceSynthesizeResponse {
    /// Base64-encoded WAV bytes.
    pub audio: String,
    pub format: &'static str,
    pub sample_rate: u32,
    pub duration_ms: u64,
    pub voice: String,
}

#[derive(Debug, Serialize)]
pub struct VoiceStatusResponse {
    pub configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub max_audio_b64_len: usize,
    pub max_text_chars: usize,
}

#[derive(Debug, Serialize)]
struct SidecarTranscribeRequest<'a> {
    audio: &'a str,
    format: &'a str,
    language: &'a str,
    model: &'a str,
}

#[derive(Debug, Deserialize)]
struct SidecarTranscribeResponse {
    text: String,
    language: String,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    segments: Vec<SidecarSegment>,
    #[serde(default)]
    model: String,
}

#[derive(Debug, Deserialize)]
struct SidecarSegment {
    #[serde(default)]
    start_ms: u64,
    #[serde(default)]
    end_ms: u64,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Serialize)]
struct SidecarSynthesizeRequest<'a> {
    text: &'a str,
    voice: &'a str,
    speed: f32,
}

#[derive(Debug, Deserialize)]
struct SidecarSynthesizeResponse {
    audio: String,
    #[serde(default)]
    sample_rate: u32,
    #[serde(default)]
    duration_ms: u64,
    #[serde(default)]
    voice: String,
}

#[derive(Debug, Deserialize)]
struct SidecarErrorResponse {
    #[serde(default)]
    detail: String,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

type HandlerError = (StatusCode, Json<VoiceErrorResponse>);

#[derive(Debug, Serialize)]
pub struct VoiceErrorResponse {
    pub error: &'static str,
    pub message: String,
}

fn error_response(status: StatusCode, error: &'static str, message: String) -> HandlerError {
    (status, Json(VoiceErrorResponse { error, message }))
}

/// GET /api/voice/status — capability discovery for desktop clients.
pub async fn voice_status_handler(
    State(state): State<Arc<GatewayState>>,
    _user: AuthenticatedUser,
) -> Json<VoiceStatusResponse> {
    let (configured, model) = match state.voice.as_ref() {
        Some(api) => (true, Some(api.client().model().to_string())),
        None => (false, None),
    };
    Json(VoiceStatusResponse {
        configured,
        model,
        max_audio_b64_len: MAX_AUDIO_B64_LEN,
        max_text_chars: MAX_SYNTH_TEXT_CHARS,
    })
}

/// POST /api/voice/transcribe — bounded audio-to-text proxy.
pub async fn voice_transcribe_handler(
    State(state): State<Arc<GatewayState>>,
    user: AuthenticatedUser,
    Json(request): Json<VoiceTranscribeRequest>,
) -> Result<Json<VoiceTranscribeResponse>, HandlerError> {
    if !state.voice_rate_limiter.check(&user.0.user_id) {
        tracing::warn!(user_id = %user.0.user_id, "Voice transcribe rate limited");
        return Err(error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many voice requests".to_string(),
        ));
    }
    let api = state.voice.as_ref().ok_or_else(|| {
        error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "voice service is not configured".to_string(),
        )
    })?;

    validate_transcribe_request(&request)?;
    let language = request.language.as_deref().unwrap_or("auto");
    let sidecar = SidecarTranscribeRequest {
        audio: &request.audio,
        format: &request.format,
        language,
        model: api.client().model(),
    };
    let response = api
        .client()
        .transcribe(sidecar)
        .await
        .map_err(map_proxy_error)?;
    Ok(Json(VoiceTranscribeResponse {
        text: response.text,
        language: response.language,
        confidence: response.confidence,
        segments: response
            .segments
            .into_iter()
            .map(|segment| VoiceSegment {
                start_ms: segment.start_ms,
                end_ms: segment.end_ms,
                text: segment.text,
            })
            .collect(),
        model: response.model,
    }))
}

/// POST /api/voice/synthesize — bounded text-to-speech proxy.
pub async fn voice_synthesize_handler(
    State(state): State<Arc<GatewayState>>,
    user: AuthenticatedUser,
    Json(request): Json<VoiceSynthesizeRequest>,
) -> Result<Json<VoiceSynthesizeResponse>, HandlerError> {
    if !state.voice_rate_limiter.check(&user.0.user_id) {
        tracing::warn!(user_id = %user.0.user_id, "Voice synthesize rate limited");
        return Err(error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many voice requests".to_string(),
        ));
    }
    let api = state.voice.as_ref().ok_or_else(|| {
        error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "voice service is not configured".to_string(),
        )
    })?;

    let text = validate_synthesize_request(&request)?;
    let voice = request.voice.as_deref().unwrap_or("");
    let speed = request.speed.unwrap_or(1.0);
    let sidecar = SidecarSynthesizeRequest {
        text: &text,
        voice,
        speed,
    };
    let response = api
        .client()
        .synthesize(sidecar)
        .await
        .map_err(map_proxy_error)?;
    Ok(Json(VoiceSynthesizeResponse {
        audio: response.audio,
        format: "wav",
        sample_rate: response.sample_rate,
        duration_ms: response.duration_ms,
        voice: response.voice,
    }))
}

fn map_proxy_error(error: VoiceProxyError) -> HandlerError {
    if let VoiceProxyError::Upstream { status, detail } = &error {
        match StatusCode::from_u16(*status) {
            Ok(StatusCode::TOO_MANY_REQUESTS) => {
                return error_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    "rate_limited",
                    detail.clone(),
                );
            }
            Ok(StatusCode::BAD_REQUEST) => {
                return error_response(StatusCode::BAD_REQUEST, "bad_request", detail.clone());
            }
            _ => {}
        }
    }
    tracing::warn!(error = %error, "LunarVoice proxy request failed");
    error_response(error.gateway_status(), "upstream_error", error.to_string())
}

fn validate_transcribe_request(request: &VoiceTranscribeRequest) -> Result<(), HandlerError> {
    if request.audio.is_empty() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "audio must not be empty".to_string(),
        ));
    }
    if request.audio.len() > MAX_AUDIO_B64_LEN {
        return Err(error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            format!("audio exceeds {} base64 bytes", MAX_AUDIO_B64_LEN),
        ));
    }
    if base64::engine::general_purpose::STANDARD
        .decode(request.audio.as_bytes())
        .is_err()
    {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "audio must be valid base64".to_string(),
        ));
    }
    if !AUDIO_FORMATS.contains(&request.format.as_str()) {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "bad_request",
            format!("format must be one of: {}", AUDIO_FORMATS.join(", ")),
        ));
    }
    if request
        .language
        .as_deref()
        .is_some_and(|language| language.len() > 32)
    {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "language must be at most 32 characters".to_string(),
        ));
    }
    Ok(())
}

fn validate_synthesize_request(request: &VoiceSynthesizeRequest) -> Result<String, HandlerError> {
    let text = request.text.trim();
    if text.is_empty() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "text must not be empty".to_string(),
        ));
    }
    if text.chars().count() > MAX_SYNTH_TEXT_CHARS {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "bad_request",
            format!("text exceeds {} characters", MAX_SYNTH_TEXT_CHARS),
        ));
    }
    if request
        .voice
        .as_deref()
        .is_some_and(|voice| voice.len() > 64)
    {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "voice must be at most 64 characters".to_string(),
        ));
    }
    if request
        .speed
        .is_some_and(|speed| !speed.is_finite() || !(0.5..=2.0).contains(&speed))
    {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "speed must be between 0.5 and 2.0".to_string(),
        ));
    }
    Ok(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_format_allowlist_matches_sidecar_wire_names() {
        // The sidecar's `AudioFormat` wire forms (lowercase) must all be here.
        for format in [
            "wav", "ogg", "opus", "mp3", "mp4", "webm", "flac", "m4a", "aac",
        ] {
            assert!(AUDIO_FORMATS.contains(&format), "missing {format}");
        }
    }

    #[test]
    fn validate_transcribe_rejects_oversize_and_bad_format() {
        let request = VoiceTranscribeRequest {
            audio: "AAAA".to_string(),
            format: "exe".to_string(),
            language: None,
        };
        let err = validate_transcribe_request(&request).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        let request = VoiceTranscribeRequest {
            audio: "A".repeat(MAX_AUDIO_B64_LEN + 8),
            format: "wav".to_string(),
            language: None,
        };
        let err = validate_transcribe_request(&request).unwrap_err();
        assert_eq!(err.0, StatusCode::PAYLOAD_TOO_LARGE);

        let request = VoiceTranscribeRequest {
            audio: "not base64!!".to_string(),
            format: "wav".to_string(),
            language: None,
        };
        let err = validate_transcribe_request(&request).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_synthesize_bounds_text_voice_speed() {
        let request = VoiceSynthesizeRequest {
            text: "  ".to_string(),
            voice: None,
            speed: None,
        };
        assert_eq!(
            validate_synthesize_request(&request).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );

        let request = VoiceSynthesizeRequest {
            text: "x".repeat(MAX_SYNTH_TEXT_CHARS + 1),
            voice: None,
            speed: None,
        };
        assert_eq!(
            validate_synthesize_request(&request).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );

        let request = VoiceSynthesizeRequest {
            text: "hello".to_string(),
            voice: Some("v".repeat(65)),
            speed: None,
        };
        assert_eq!(
            validate_synthesize_request(&request).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );

        let request = VoiceSynthesizeRequest {
            text: "hello".to_string(),
            voice: None,
            speed: Some(2.5),
        };
        assert_eq!(
            validate_synthesize_request(&request).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );

        let request = VoiceSynthesizeRequest {
            text: " hello ".to_string(),
            voice: None,
            speed: Some(1.0),
        };
        assert_eq!(validate_synthesize_request(&request).unwrap(), "hello");
    }

    #[test]
    fn voice_api_requires_lunarvoice_provider_and_token() {
        let mut config = crate::config::TranscriptionConfig::default();
        assert!(VoiceApi::from_config(&config).is_none());
        config.provider = "lunarvoice".to_string();
        assert!(VoiceApi::from_config(&config).is_none());
        config.voice_auth_token = Some(SecretString::from("secret"));
        assert!(VoiceApi::from_config(&config).is_some());
    }

    #[test]
    fn voice_api_rejects_non_loopback_url() {
        let config = crate::config::TranscriptionConfig {
            provider: "lunarvoice".to_string(),
            voice_auth_token: Some(SecretString::from("secret")),
            voice_service_url: Some("http://192.168.1.5:8090".to_string()),
            ..Default::default()
        };
        assert!(VoiceApi::from_config(&config).is_none());
    }

    #[tokio::test]
    async fn proxy_maps_upstream_errors_and_passes_responses_through() {
        use axum::routing::post;

        let sidecar = axum::Router::new()
            .route(
                "/v1/voice/transcribe",
                post(|| async {
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        Json(serde_json::json!({"error": "rate_limited", "detail": "slow down"})),
                    )
                }),
            )
            .route(
                "/v1/voice/synthesize",
                post(|| async {
                    Json(serde_json::json!({
                        "audio": "UklGRg==",
                        "format": "wav",
                        "sample_rate": 24000,
                        "duration_ms": 1000,
                        "voice": "af_heart",
                    }))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, sidecar).await.unwrap();
        });

        let client = VoiceProxyClient::new(
            &format!("http://{}", addr),
            SecretString::from("secret"),
            "whisper-tiny",
        )
        .unwrap();

        let err = client
            .transcribe(SidecarTranscribeRequest {
                audio: "AAAA",
                format: "wav",
                language: "auto",
                model: "whisper-tiny",
            })
            .await
            .unwrap_err();
        assert!(matches!(err, VoiceProxyError::Upstream { status: 429, .. }));
        let mapped = map_proxy_error(err);
        assert_eq!(mapped.0, StatusCode::TOO_MANY_REQUESTS);

        let response = client
            .synthesize(SidecarSynthesizeRequest {
                text: "hello",
                voice: "",
                speed: 1.0,
            })
            .await
            .unwrap();
        assert_eq!(response.audio, "UklGRg==");
        assert_eq!(response.sample_rate, 24000);
    }
}
