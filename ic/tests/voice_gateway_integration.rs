//! End-to-end HTTP tests for desktop-facing voice gateway endpoints.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use base64::Engine as _;
use secrecy::SecretString;
use serde_json::{Value, json};

use lunarwing::channels::web::VoiceGatewayService;
use lunarwing::channels::web::test_helpers::TestGatewayBuilder;

const GATEWAY_TOKEN: &str = "gateway-it-token";
const SIDECAR_TOKEN: &str = "it-sidecar-token";

async fn start_mock_sidecar(
    transcribe_status: StatusCode,
    synthesize_status: StatusCode,
) -> (u16, tokio::task::JoinHandle<()>) {
    let transcribe = axum::routing::post(move |headers: axum::http::HeaderMap| async move {
        if !sidecar_authorized(&headers) {
            return (StatusCode::UNAUTHORIZED, "unauthorized".to_string()).into_response();
        }
        if transcribe_status != StatusCode::OK {
            return (transcribe_status, "sidecar failure".to_string()).into_response();
        }
        axum::Json(json!({
            "text": "hello from sidecar",
            "language": "en",
            "model": "whisper-tiny",
            "confidence": 0.9,
            "segments": []
        }))
        .into_response()
    });
    let synthesize = axum::routing::post(move |headers: axum::http::HeaderMap| async move {
        if !sidecar_authorized(&headers) {
            return (StatusCode::UNAUTHORIZED, "unauthorized".to_string()).into_response();
        }
        if synthesize_status != StatusCode::OK {
            return (synthesize_status, "sidecar failure".to_string()).into_response();
        }
        axum::Json(json!({
            "audio": base64::engine::general_purpose::STANDARD.encode([0u8; 24]),
            "format": "wav",
            "sample_rate": 16000,
            "duration_ms": 14,
            "voice": "af_heart"
        }))
        .into_response()
    });
    let app = Router::new()
        .route("/v1/voice/transcribe", transcribe)
        .route("/v1/voice/synthesize", synthesize);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock sidecar");
    let port = listener.local_addr().expect("sidecar address").port();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve mock sidecar");
    });
    (port, handle)
}

fn sidecar_authorized(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == format!("Bearer {SIDECAR_TOKEN}"))
}

async fn start_gateway(sidecar_url: Option<&str>) -> SocketAddr {
    let builder = match sidecar_url {
        Some(url) => {
            let service = VoiceGatewayService::new(url, SecretString::from(SIDECAR_TOKEN))
                .expect("valid sidecar URL");
            TestGatewayBuilder::new().voice_gateway(Arc::new(service))
        }
        None => TestGatewayBuilder::new(),
    };
    builder.start(GATEWAY_TOKEN).await.expect("start gateway").0
}

async fn post(addr: SocketAddr, path: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{addr}{path}"))
        .bearer_auth(GATEWAY_TOKEN)
        .json(&body)
        .send()
        .await
        .expect("gateway response")
}

async fn reserve_dead_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind dead port");
    let port = listener.local_addr().expect("dead port address").port();
    drop(listener);
    port
}

async fn start_cancellation_sidecar() -> (u16, Arc<tokio::sync::Notify>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let route_calls = Arc::clone(&calls);
    let route_entered = Arc::clone(&entered);
    let app = Router::new().route(
        "/v1/voice/transcribe",
        axum::routing::post(move || {
            let calls = Arc::clone(&route_calls);
            let entered = Arc::clone(&route_entered);
            async move {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    entered.notify_one();
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                }
                axum::Json(json!({
                    "text": "still healthy",
                    "language": "en",
                    "model": "mock",
                    "confidence": null,
                    "segments": []
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind cancellation sidecar");
    let port = listener.local_addr().expect("sidecar address").port();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve cancellation sidecar");
    });
    (port, entered)
}

#[tokio::test]
async fn missing_auth_returns_401() {
    let addr = start_gateway(Some("http://127.0.0.1:1")).await;
    let response = reqwest::Client::new()
        .post(format!("http://{addr}/api/voice/transcribe"))
        .json(&json!({"audio": "aGk="}))
        .send()
        .await
        .expect("gateway response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn transcribe_round_trip_uses_server_side_token() {
    let (port, _sidecar) = start_mock_sidecar(StatusCode::OK, StatusCode::OK).await;
    let sidecar_url = format!("http://127.0.0.1:{port}");
    let addr = start_gateway(Some(&sidecar_url)).await;
    let response = post(
        addr,
        "/api/voice/transcribe",
        json!({"audio": "aGVsbG8=", "format": "wav"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.expect("response body");
    let value: Value = serde_json::from_str(&body).expect("response JSON");
    assert_eq!(value["text"], "hello from sidecar");
    assert_eq!(value["confidence"], 0.9);
    assert_eq!(value["segments"], json!([]));
    assert!(!body.contains(SIDECAR_TOKEN));
    assert!(!body.contains(&sidecar_url));
}

#[tokio::test]
async fn synthesize_round_trip_uses_server_side_token() {
    let (port, _sidecar) = start_mock_sidecar(StatusCode::OK, StatusCode::OK).await;
    let addr = start_gateway(Some(&format!("http://127.0.0.1:{port}"))).await;
    let response = post(
        addr,
        "/api/voice/synthesize",
        json!({"text": "hello there", "speed": 1.0}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value: Value = response.json().await.expect("response JSON");
    assert_eq!(value["format"], "wav");
    assert_eq!(value["voice"], "af_heart");
    assert_eq!(value["sample_rate"], 16000);
}

#[tokio::test]
async fn oversized_requests_return_413() {
    let (port, _sidecar) = start_mock_sidecar(StatusCode::OK, StatusCode::OK).await;
    let addr = start_gateway(Some(&format!("http://127.0.0.1:{port}"))).await;
    let transcribe = post(
        addr,
        "/api/voice/transcribe",
        json!({"audio": "A".repeat(9 * 1024 * 1024)}),
    )
    .await;
    assert_eq!(transcribe.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let synthesize = post(
        addr,
        "/api/voice/synthesize",
        json!({"text": "x".repeat(8192)}),
    )
    .await;
    assert_eq!(synthesize.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn unconfigured_or_down_sidecar_returns_503() {
    let unconfigured = start_gateway(None).await;
    let response = post(
        unconfigured,
        "/api/voice/transcribe",
        json!({"audio": "aGk="}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let dead_port = reserve_dead_port().await;
    let down = start_gateway(Some(&format!("http://127.0.0.1:{dead_port}"))).await;
    let response = post(down, "/api/voice/transcribe", json!({"audio": "aGk="})).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn sidecar_error_returns_502() {
    let (port, _sidecar) =
        start_mock_sidecar(StatusCode::INTERNAL_SERVER_ERROR, StatusCode::OK).await;
    let addr = start_gateway(Some(&format!("http://127.0.0.1:{port}"))).await;
    let response = post(addr, "/api/voice/transcribe", json!({"audio": "aGk="})).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn client_cancellation_does_not_block_follow_up_request() {
    let (port, entered) = start_cancellation_sidecar().await;
    let addr = start_gateway(Some(&format!("http://127.0.0.1:{port}"))).await;
    let first = tokio::spawn(post(
        addr,
        "/api/voice/transcribe",
        json!({"audio": "aGk="}),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .expect("first sidecar request started");
    first.abort();

    let response = post(addr, "/api/voice/transcribe", json!({"audio": "aGk="})).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn invalid_synthesis_speed_returns_400() {
    let (port, _sidecar) = start_mock_sidecar(StatusCode::OK, StatusCode::OK).await;
    let addr = start_gateway(Some(&format!("http://127.0.0.1:{port}"))).await;
    let response = post(
        addr,
        "/api/voice/synthesize",
        json!({"text": "hello", "speed": 9.9}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
