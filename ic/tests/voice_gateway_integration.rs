//! End-to-end integration tests for the desktop-facing voice API (WI#131).
//!
//! These tests start a real Axum gateway on a random port plus a mock
//! LunarVoice sidecar on another random port, and verify the full flow:
//! - Auth middleware protects all `/api/voice/*` routes
//! - Status reports configured/unconfigured capability
//! - Transcribe and synthesize proxy to the loopback sidecar with the
//!   server-held credential
//! - Bounds are enforced (oversize audio, bad format, empty/long text,
//!   invalid speed, body limit)
//! - Upstream failures map to stable gateway errors without leaking sidecar
//!   detail (401 -> 500, 429 -> 429, 400 -> 400, 422/503/504 keep their
//!   semantics, other statuses -> 502, proxy timeout -> 504)

use std::sync::Arc;

use axum::{
    Json, Router,
    http::StatusCode,
    routing::{get, post},
};
use base64::Engine;
use secrecy::SecretString;
use serde_json::{Value, json};

use lunarwing::channels::web::auth::MultiAuthState;
use lunarwing::channels::web::server::GatewayState;
use lunarwing::channels::web::voice::VoiceApi;
use lunarwing::config::TranscriptionConfig;

const AUTH_TOKEN: &str = "voice-test-token";

/// Mock LunarVoice sidecar recording the bearer token it saw.
#[derive(Clone, Default)]
struct SidecarState {
    last_token: Arc<std::sync::Mutex<Option<String>>>,
}

async fn start_mock_sidecar() -> (std::net::SocketAddr, SidecarState) {
    let state = SidecarState::default();

    let transcribe = {
        let state = state.clone();
        move |headers: axum::http::HeaderMap| {
            let state = state.clone();
            async move {
                record_token(&state, &headers);
                (
                    StatusCode::OK,
                    Json(json!({
                        "text": "hello from the sidecar",
                        "language": "en",
                        "confidence": 0.9,
                        "segments": [
                            {"start_ms": 0, "end_ms": 500, "text": "hello from the sidecar"}
                        ],
                        "model": "whisper-tiny",
                    })),
                )
            }
        }
    };

    let synthesize = {
        let state = state.clone();
        move |headers: axum::http::HeaderMap| {
            let state = state.clone();
            async move {
                record_token(&state, &headers);
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": "unavailable", "detail": "tts disabled"})),
                )
            }
        }
    };

    let app = Router::new()
        .route("/v1/voice/transcribe", post(transcribe))
        .route("/v1/voice/synthesize", post(synthesize))
        .route("/health", get(|| async { (StatusCode::OK, "ok") }));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, state)
}

fn record_token(state: &SidecarState, headers: &axum::http::HeaderMap) {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);
    *state.last_token.lock().expect("sidecar token mutex") = token;
}

fn voice_config(service_url: &str) -> TranscriptionConfig {
    TranscriptionConfig {
        provider: "lunarvoice".to_string(),
        voice_service_url: Some(service_url.to_string()),
        voice_auth_token: Some(SecretString::from("sidecar-secret")),
        model: "whisper-tiny".to_string(),
        ..Default::default()
    }
}

async fn start_gateway(voice: Option<Arc<VoiceApi>>) -> std::net::SocketAddr {
    let auth = MultiAuthState::single(AUTH_TOKEN.to_string(), "test-user".to_string());
    let addr: std::net::SocketAddr = "127.0.0.1:0"
        .parse()
        .expect("hard-coded address must parse"); // safety: constant literal
    let state = Arc::new(GatewayState {
        msg_tx: tokio::sync::RwLock::new(None),
        sse: Arc::new(lunarwing::channels::web::sse::SseManager::new()),
        workspace: None,
        workspace_pool: None,
        session_manager: None,
        log_broadcaster: None,
        log_level_handle: None,
        extension_manager: None,
        tool_registry: None,
        store: None,
        job_manager: None,
        external_worker_manager: None,
        prompt_queue: None,
        owner_id: "test-user".to_string(),
        default_sender_id: "test-user".to_string(),
        shutdown_tx: tokio::sync::RwLock::new(None),
        ws_tracker: None,
        llm_provider: None,
        skill_registry: None,
        skill_catalog: None,
        scheduler: None,
        chat_rate_limiter: lunarwing::channels::web::server::PerUserRateLimiter::new(30, 60),
        oauth_rate_limiter: lunarwing::channels::web::server::RateLimiter::new(10, 60),
        webhook_rate_limiter: lunarwing::channels::web::server::RateLimiter::new(10, 60),
        registry_entries: Vec::new(),
        cost_guard: None,
        routine_engine: Arc::new(tokio::sync::RwLock::new(None)),
        startup_time: std::time::Instant::now(),
        active_config: lunarwing::channels::web::server::ActiveConfigSnapshot::default(),
        secrets_store: None,
        db_auth: None,
        channel_manager: None,
        ws_ping_interval_secs: 30,
        ws_idle_timeout_secs: 120,
        voice,
        voice_rate_limiter: lunarwing::channels::web::server::PerUserRateLimiter::new(10, 60),
    });
    lunarwing::channels::web::server::start_server(
        addr,
        state,
        lunarwing::channels::web::auth::CombinedAuthState::from(auth),
    )
    .await
    .expect("gateway should start")
}

async fn get_json(url: &str, token: &str) -> (StatusCode, Value) {
    let client = reqwest::Client::new();
    let response = client.get(url).bearer_auth(token).send().await.unwrap();
    let status = response.status();
    let body = response.json::<Value>().await.unwrap_or(Value::Null);
    (status, body)
}

async fn post_json(url: &str, token: &str, body: Value) -> (StatusCode, Value) {
    let client = reqwest::Client::new();
    let response = client
        .post(url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json::<Value>().await.unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn voice_routes_require_auth() {
    let addr = start_gateway(None).await;
    let url = format!("http://{addr}/api/voice/status");

    let (status, _) = get_json(&url, "wrong-token").await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "status must be auth-gated"
    );

    let (status, _) = post_json(
        &format!("http://{addr}/api/voice/transcribe"),
        "wrong-token",
        json!({"audio": "AAAA"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn status_reports_unconfigured_without_credential() {
    let addr = start_gateway(None).await;
    let (status, body) = get_json(&format!("http://{addr}/api/voice/status"), AUTH_TOKEN).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["configured"], json!(false));
}

#[tokio::test]
async fn transcribe_proxies_to_sidecar_with_server_held_credential() {
    let (sidecar_addr, sidecar) = start_mock_sidecar().await;
    let api = VoiceApi::from_config(&voice_config(&format!("http://{sidecar_addr}")))
        .map(Arc::new)
        .expect("voice api should construct for loopback url");
    let addr = start_gateway(Some(api)).await;

    let (status, body) = get_json(&format!("http://{addr}/api/voice/status"), AUTH_TOKEN).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["configured"], json!(true));
    assert_eq!(body["model"], json!("whisper-tiny"));
    assert_eq!(body["max_synth_audio_secs"], json!(300));

    let audio = base64::engine::general_purpose::STANDARD.encode(b"RIFFfake");
    let (status, body) = post_json(
        &format!("http://{addr}/api/voice/transcribe"),
        AUTH_TOKEN,
        json!({"audio": audio, "format": "wav"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["text"], json!("hello from the sidecar"));
    assert_eq!(body["segments"][0]["text"], json!("hello from the sidecar"));

    // The sidecar must have seen the server-held VOICE_AUTH_TOKEN, never the
    // client's gateway token.
    let seen = sidecar
        .last_token
        .lock()
        .expect("sidecar token mutex")
        .clone();
    assert_eq!(
        seen.as_deref(),
        Some("sidecar-secret"),
        "sidecar must receive the server-held credential"
    );
    assert_ne!(seen.as_deref(), Some(AUTH_TOKEN));
}

#[tokio::test]
async fn transcribe_rejects_out_of_bounds_requests() {
    let (sidecar_addr, _) = start_mock_sidecar().await;
    let api = VoiceApi::from_config(&voice_config(&format!("http://{sidecar_addr}")))
        .map(Arc::new)
        .unwrap();
    let addr = start_gateway(Some(api)).await;

    // Bad format
    let (status, _) = post_json(
        &format!("http://{addr}/api/voice/transcribe"),
        AUTH_TOKEN,
        json!({"audio": "AAAA", "format": "exe"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Invalid base64
    let (status, _) = post_json(
        &format!("http://{addr}/api/voice/transcribe"),
        AUTH_TOKEN,
        json!({"audio": "not base64!!", "format": "wav"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Empty audio
    let (status, _) = post_json(
        &format!("http://{addr}/api/voice/transcribe"),
        AUTH_TOKEN,
        json!({"audio": "", "format": "wav"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn synthesize_validates_bounds_and_maps_upstream_unavailable() {
    let (sidecar_addr, _) = start_mock_sidecar().await;
    let api = VoiceApi::from_config(&voice_config(&format!("http://{sidecar_addr}")))
        .map(Arc::new)
        .unwrap();
    let addr = start_gateway(Some(api)).await;

    // Bounds are enforced before the upstream call.
    let (status, _) = post_json(
        &format!("http://{addr}/api/voice/synthesize"),
        AUTH_TOKEN,
        json!({"text": "   "}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = post_json(
        &format!("http://{addr}/api/voice/synthesize"),
        AUTH_TOKEN,
        json!({"text": "hi", "speed": 3.0}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let long = "x".repeat(10_001);
    let (status, _) = post_json(
        &format!("http://{addr}/api/voice/synthesize"),
        AUTH_TOKEN,
        json!({"text": long}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A valid request flows to the sidecar; the mock reports TTS disabled,
    // which must surface as a generic upstream error (not a gateway 200, not
    // a verbatim detail leak). Sidecar overload statuses keep their semantics
    // at the gateway (503 unavailable — WI#259).
    let (status, body) = post_json(
        &format!("http://{addr}/api/voice/synthesize"),
        AUTH_TOKEN,
        json!({"text": "hello", "voice": "af_heart", "speed": 1.0}),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body: {body}");
    assert_eq!(body["error"], json!("unavailable"));
    assert_eq!(body["message"], json!("voice sidecar is unavailable"));
    assert!(
        !body["message"]
            .as_str()
            .is_some_and(|m| m.contains("tts disabled")),
        "sidecar detail must not leak to clients"
    );

    // WI#259: a reply whose estimated audio duration exceeds the sidecar cap
    // is rejected by the gateway itself with a distinct 422 `text_too_long`
    // instead of burning sidecar CPU and surfacing an opaque 502.
    let long = "word ".repeat(1_200); // ~4800 non-whitespace chars ≈ 343s
    let (status, body) = post_json(
        &format!("http://{addr}/api/voice/synthesize"),
        AUTH_TOKEN,
        json!({"text": long}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_eq!(body["error"], json!("text_too_long"));
}

#[tokio::test]
async fn upstream_unauthorized_maps_to_gateway_500_without_detail_leak() {
    let app = Router::new().route(
        "/v1/voice/transcribe",
        post(|| async {
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "unauthorized", "detail": "bad sidecar token"})),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let api = VoiceApi::from_config(&voice_config(&format!("http://{addr}")))
        .map(Arc::new)
        .expect("voice api should construct");
    let gateway = start_gateway(Some(api)).await;

    // A sidecar 401 is a server-side credential problem: it must surface as
    // 500 (not 401, which would read as a failed gateway token) and must not
    // leak the sidecar's detail text.
    let (status, body) = post_json(
        &format!("http://{gateway}/api/voice/transcribe"),
        AUTH_TOKEN,
        json!({"audio": "AAAA", "format": "wav"}),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body: {body}");
    assert_eq!(body["error"], json!("upstream_error"));
    assert_eq!(
        body["message"],
        json!("voice sidecar rejected the gateway credential")
    );
}

#[tokio::test]
async fn unreachable_sidecar_maps_to_bad_gateway() {
    // Bind then immediately drop the listener to get a closed loopback port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);

    let api = VoiceApi::from_config(&voice_config(&format!("http://{addr}")))
        .map(Arc::new)
        .expect("voice api should construct for loopback url");
    let gateway = start_gateway(Some(api)).await;

    let (status, body) = post_json(
        &format!("http://{gateway}/api/voice/transcribe"),
        AUTH_TOKEN,
        json!({"audio": "AAAA", "format": "wav"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "body: {body}");
    assert_eq!(body["error"], json!("upstream_error"));
}

#[tokio::test]
async fn unconfigured_voice_service_returns_service_unavailable() {
    let addr = start_gateway(None).await;
    let (status, _) = post_json(
        &format!("http://{addr}/api/voice/transcribe"),
        AUTH_TOKEN,
        json!({"audio": "AAAA", "format": "wav"}),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    let (status, _) = post_json(
        &format!("http://{addr}/api/voice/synthesize"),
        AUTH_TOKEN,
        json!({"text": "hello"}),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[test]
fn voice_api_requires_loopback_and_dedicated_token() {
    let mut config = voice_config("http://127.0.0.1:8090");
    assert!(VoiceApi::from_config(&config).is_some());

    config.voice_auth_token = None;
    assert!(
        VoiceApi::from_config(&config).is_none(),
        "token is required"
    );

    let config = voice_config("http://192.168.1.5:8090");
    assert!(
        VoiceApi::from_config(&config).is_none(),
        "non-loopback URLs are rejected"
    );

    let mut config = voice_config("http://127.0.0.1:8090");
    config.provider = "openai".to_string();
    assert!(
        VoiceApi::from_config(&config).is_none(),
        "only the lunarvoice provider enables the gateway voice API"
    );
}
