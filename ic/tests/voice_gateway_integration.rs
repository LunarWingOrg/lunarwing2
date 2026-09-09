//! HTTP integration tests for the desktop-facing voice gateway proxy.
//!
//! Starts a real gateway (auth middleware plus `/api/voice/*` routes) and mock
//! LunarVoice sidecars on loopback, then exercises the endpoints end-to-end:
//! missing auth, per-route body limits, sidecar unavailability mapping, and
//! successful transcribe + synthesize round trips. Sidecar URL/token stay
//! server-side throughout.

use std::sync::Arc;

use axum::body::Bytes;
use axum::http::{Request, StatusCode};
use axum::{Json, Router};
use base64::Engine as _;
use http_body_util::BodyExt;
use lunarwing::channels::web::auth::CombinedAuthState;
use lunarwing::channels::web::auth::MultiAuthState;
use lunarwing::channels::web::handlers::voice::VoiceGatewayService;
use lunarwing::channels::web::test_helpers::TestGatewayBuilder;
use secrecy::SecretString;
use serde_json::json;
use tower::ServiceExt;

const GATEWAY_TOKEN: &str = "gateway-it-token";
const SIDECAR_TOKEN: &str = "it-sidecar-token";

/// Start a mock LunarVoice sidecar on a random loopback port.
///
/// Requires `Authorization: Bearer <SIDECAR_TOKEN>` — mirrors the real sidecar
/// auth contract so tests also cover gateway credential passing.
async fn start_mock_sidecar() -> (tokio::task::JoinHandle<()>, u16) {
    let transcribe = axum::routing::post(|headers: axum::http::HeaderMap| async move {
        let ok = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == format!("Bearer {SIDECAR_TOKEN}"));
        if ok {
            (
                axum::http::StatusCode::OK,
                axum::Json(serde_json::json!({
                    "text": "hello herd from sidecar",
                    "language": "en",
                    "model": "whisper-tiny",
                    "confidence": 0.9,
                    "segments": []
                })),
            )
                .into_response()
        } else {
            (axum::http::StatusCode::UNAUTHORIZED, "unauthorized").into_response()
        }
    });
    let synthesize = axum::routing::post(|headers: axum::http::HeaderMap| async move {
        let authed = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == format!("Bearer {SIDECAR_TOKEN}"));
        if authed {
            (
                axum::http::StatusCode::OK,
                axum::Json(serde_json::json!({
                    "audio": base64::engine::general_purpose::STANDARD.encode([0u8; 24]),
                    "format": "wav",
                    "sample_rate": 16000,
                    "duration_ms": 14,
                    "voice": "af_heart"
                })),
            )
                .into_response()
        } else {
            (axum::http::StatusCode::UNAUTHORIZED, "unauthorized").into_response()
        }
    });
    let app = axum::Router::new()
        .route("/v1/voice/transcribe", axum::routing::post(transcribe))
        .route("/v1/voice/synthesize", axum::routing::post(synthesize));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock sidecar");
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock sidecar serve");
    });
    (listener.local_addr().unwrap().port(), handle)
}

/// Start a sidecar that 503s every request — exercises the unavailable-sidecar
/// mapping in the gateway.
async fn start_broken_sidecar() -> u16 {
    let app = axum::Router::new().route(
        "/v1/voice/transcribe",
        axum::routing::post(|| async {
            (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "model load failed",
            )
        })
        .route(
            "/v1/voice/synthesize",
            axum::routing::post(|| async {
                (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "engine not ready",
                )
            }),
        );
    server_on_loopback(app).await.0
}

/// Bind a port then immediately release it so connect attempts fail with
/// connection-refused — the "sidecar truly down" path in upstream mapping.
async fn reserve_dead_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind placeholder");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

async fn server_on_loopback(app: axum::Router) -> (tokio::task::JoinHandle<()>, u16) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback bind");
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock sidecar");
    });
    (handle, port)
}
