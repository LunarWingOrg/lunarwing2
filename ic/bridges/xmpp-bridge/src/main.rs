use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{ConnectInfo, DefaultBodyLimit, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use futures::StreamExt;
use lunarwing::bootstrap::lunarwing_base_dir;
use lunarwing::channels::{
    Channel, IncomingMessage, OutboundAttachment, OutgoingResponse, XmppChannel,
};
use lunarwing::config::XmppConfig;
use openclaw_xmpp_bridge_contract::{
    BridgeMessage, BridgeStatusResponse, ConfigureRequest, ConfigureResponse, MessagesQuery,
    MessagesResponse, OutboundRateLimitRequest, OutboundRateLimitResponse, SendRequest,
};
use secrecy::SecretString;
use tokio::sync::RwLock;

const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8787";
const DEFAULT_MAX_MESSAGES: usize = 2048;
/// Maximum JSON body accepted by `/v1/messages/send`.
///
/// The daemon's WASM host caps outbound attachments at 50 MiB of raw bytes per
/// response (`MAX_TOTAL_ATTACHMENT_BYTES` in `ic/src/channels/wasm/wrapper.rs`),
/// and the guest base64-encodes those bytes into the send request (a 4/3 size
/// inflation plus JSON framing). Without an explicit limit, axum's 2 MiB
/// `DefaultBodyLimit` rejects any attachment larger than ~1.4 MiB raw with
/// HTTP 413 before the handler runs — which is why agents could not send
/// ordinary photos (work item #42). 70 MiB covers the host cap with headroom
/// while still bounding memory use on this loopback-only service.
///
/// The raised limit is scoped to the send route only (see
/// `build_router_with_body_limit`); every other route keeps axum's 2 MiB
/// default, and authorization runs as middleware before any body is buffered
/// (`docs/ops/MEDIUM.MD`).
const MAX_SEND_BODY_BYTES: usize = 70 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct NormalizedConfig {
    jid: String,
    password: String,
    dm_policy: String,
    allow_from: Vec<String>,
    allow_rooms: Vec<String>,
    encrypted_rooms: Vec<String>,
    device_id: u32,
    omemo_store_dir: PathBuf,
    allow_plaintext_fallback: bool,
    max_messages_per_hour: u32,
}

#[derive(Debug, Clone)]
struct QueuedMessage {
    cursor: u64,
    payload: BridgeMessage,
}

#[derive(Default)]
struct BridgeInner {
    configured: Option<NormalizedConfig>,
    jid: Option<String>,
    channel: Option<Arc<XmppChannel>>,
    current_cursor: u64,
    messages: VecDeque<QueuedMessage>,
}

#[derive(Clone)]
struct AppState {
    inner: Arc<RwLock<BridgeInner>>,
    expected_token: Option<String>,
    max_messages: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("failed to install rustls crypto provider"))?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let bind_addr: SocketAddr = std::env::var("XMPP_BRIDGE_BIND")
        .unwrap_or_else(|_| DEFAULT_BIND_ADDR.to_string())
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid XMPP_BRIDGE_BIND: {}", e))?;
    let expected_token = std::env::var("XMPP_BRIDGE_TOKEN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let max_messages = std::env::var("XMPP_BRIDGE_MAX_MESSAGES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_MESSAGES);

    let state = AppState {
        inner: Arc::new(RwLock::new(BridgeInner::default())),
        expected_token,
        max_messages,
    };

    let app = build_router(state.clone());

    tracing::info!(addr = %bind_addr, "xmpp-bridge listening");
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// Build the bridge router with the production send body limit.
fn build_router(state: AppState) -> Router {
    build_router_with_body_limit(state, MAX_SEND_BODY_BYTES)
}

/// Build the bridge router with an explicit JSON body limit for the send
/// route (split out so tests can verify the bound without allocating 70 MiB
/// payloads). All other routes keep axum's 2 MiB default body limit.
fn build_router_with_body_limit(state: AppState, max_body_bytes: usize) -> Router {
    Router::new()
        .route("/v1/configure", post(configure_handler))
        .route("/v1/status", get(status_handler))
        .route("/v1/outbound-rate-limit", post(outbound_rate_limit_handler))
        .route("/v1/messages", get(messages_handler))
        .route(
            "/v1/messages/send",
            // Only the send route carries base64 attachments, so only it gets
            // the raised limit; the remaining routes keep the 2 MiB default.
            post(send_handler).layer(DefaultBodyLimit::max(max_body_bytes)),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            authorize_middleware,
        ))
        .with_state(state)
}

/// Authorize every request before any body extractor runs.
///
/// `authorize` needs only the peer address and headers, so as middleware it
/// rejects unauthorized clients before the `Json` extractor buffers the
/// request body (up to [`MAX_SEND_BODY_BYTES`] on the send route) — closing
/// the pre-auth memory-amplification window from `docs/ops/MEDIUM.MD`.
async fn authorize_middleware(
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, BridgeError> {
    authorize(&state, &headers, remote)?;
    Ok(next.run(request).await)
}

async fn configure_handler(
    State(state): State<AppState>,
    Json(request): Json<ConfigureRequest>,
) -> Result<Json<ConfigureResponse>, BridgeError> {
    let normalized = normalize_config(&request)?;

    {
        let inner = state.inner.read().await;
        if let Some(existing) = inner.configured.as_ref() {
            if existing == &normalized {
                return Ok(Json(ConfigureResponse {
                    configured: true,
                    running: inner.channel.is_some(),
                    jid: inner.jid.clone().unwrap_or_else(|| normalized.jid.clone()),
                }));
            }
            return Err(BridgeError::conflict(
                "xmpp-bridge is already configured; restart it to apply a different config",
            ));
        }
    }

    let native_config = to_native_config(&normalized);
    let channel = Arc::new(XmppChannel::new(native_config).await.map_err(|e| {
        BridgeError::bad_gateway(&format!("failed to initialize XMPP client: {}", e))
    })?);
    let mut stream = channel
        .start()
        .await
        .map_err(|e| BridgeError::bad_gateway(&format!("failed to start XMPP client: {}", e)))?;

    {
        let mut inner = state.inner.write().await;
        inner.configured = Some(normalized.clone());
        inner.jid = Some(normalized.jid.clone());
        inner.channel = Some(Arc::clone(&channel));
    }

    // Spawn resilient XMPP stream consumer with reconnect + exponential backoff
    let state_for_task = state.clone();
    let normalized_for_task = normalized.clone();
    tokio::spawn(async move {
        let mut backoff_secs: u64 = 1;
        const MAX_BACKOFF_SECS: u64 = 300; // 5-minute cap

        // Consume the initial stream
        while let Some(message) = stream.next().await {
            enqueue_message(&state_for_task, message).await;
        }
        tracing::warn!("xmpp-bridge input stream ended, entering reconnect loop");

        // Reconnect loop with exponential backoff + jitter
        loop {
            let jitter_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_millis() as u64 % 500)
                .unwrap_or(0);
            let wait = tokio::time::Duration::from_secs(backoff_secs)
                + tokio::time::Duration::from_millis(jitter_ms);
            tracing::info!(
                backoff_secs,
                jitter_ms,
                "xmpp-bridge reconnecting in {:?}",
                wait
            );
            tokio::time::sleep(wait).await;

            let native_config = to_native_config(&normalized_for_task);
            match XmppChannel::new(native_config).await {
                Ok(new_channel) => {
                    let new_channel = Arc::new(new_channel);
                    match new_channel.start().await {
                        Ok(mut new_stream) => {
                            tracing::info!(
                                jid = %normalized_for_task.jid,
                                "xmpp-bridge reconnected successfully"
                            );

                            // Update shared state with new channel reference
                            {
                                let mut inner = state_for_task.inner.write().await;
                                inner.channel = Some(Arc::clone(&new_channel));
                            }

                            // Reset backoff on successful reconnection
                            backoff_secs = 1;

                            // Consume the new stream
                            while let Some(message) = new_stream.next().await {
                                enqueue_message(&state_for_task, message).await;
                            }
                            tracing::warn!(
                                jid = %normalized_for_task.jid,
                                "xmpp-bridge stream ended again, will reconnect"
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                err = %e,
                                "xmpp-bridge failed to start XMPP stream, will retry"
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(
                        err = %e,
                        "xmpp-bridge failed to initialize XMPP client, will retry"
                    );
                }
            }

            // Increase backoff (capped at MAX_BACKOFF_SECS)
            backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
        }
    });

    Ok(Json(ConfigureResponse {
        configured: true,
        running: true,
        jid: normalized.jid,
    }))
}

async fn status_handler(
    State(state): State<AppState>,
) -> Result<Json<BridgeStatusResponse>, BridgeError> {
    let (
        configured,
        running,
        current_cursor,
        queued_messages,
        jid,
        configured_rooms,
        configured_limit,
        channel,
    ) = {
        let inner = state.inner.read().await;
        (
            inner.configured.is_some(),
            inner.channel.is_some(),
            inner.current_cursor,
            inner.messages.len(),
            inner.jid.clone(),
            inner
                .configured
                .as_ref()
                .map(|config| config.allow_rooms.clone())
                .unwrap_or_default(),
            inner
                .configured
                .as_ref()
                .map(|config| config.max_messages_per_hour)
                .unwrap_or(0),
            inner.channel.clone(),
        )
    };

    let diagnostics = if let Some(channel) = channel.as_ref() {
        Some(channel.omemo_diagnostics().await)
    } else {
        None
    };
    let presence_diagnostics = if let Some(channel) = channel.as_ref() {
        Some(channel.muc_presence_diagnostics().await)
    } else {
        None
    };
    let room_diagnostics = if let Some(channel) = channel.as_ref() {
        Some(channel.muc_encryption_diagnostics().await)
    } else {
        None
    };
    let outbound_rate_limit = if let Some(channel) = channel.as_ref() {
        Some(channel.outbound_rate_limit_diagnostics().await)
    } else {
        None
    };
    let active_limit = outbound_rate_limit
        .as_ref()
        .map(|value| value.max_messages_per_hour)
        .unwrap_or(configured_limit);
    let outbound_messages_last_hour = outbound_rate_limit
        .as_ref()
        .map(|value| value.messages_in_current_window)
        .unwrap_or(0);

    Ok(Json(BridgeStatusResponse {
        configured,
        running,
        current_cursor,
        queued_messages,
        jid,
        configured_rooms,
        rooms_with_presence: presence_diagnostics
            .map(|value| value.rooms_with_presence)
            .unwrap_or_default(),
        configured_max_messages_per_hour: configured_limit,
        active_max_messages_per_hour: active_limit,
        outbound_messages_last_hour,
        outbound_rate_limit_overridden: active_limit != configured_limit,
        omemo_enabled: diagnostics
            .as_ref()
            .is_some_and(|value| value.omemo_enabled),
        device_id: diagnostics.as_ref().and_then(|value| value.device_id),
        fingerprint: diagnostics
            .as_ref()
            .and_then(|value| value.fingerprint.clone()),
        bundle_published: diagnostics
            .as_ref()
            .is_some_and(|value| value.bundle_published),
        prekeys_available: diagnostics
            .as_ref()
            .map(|value| value.prekeys_available)
            .unwrap_or(0),
        migration_state: diagnostics
            .as_ref()
            .map(|value| value.migration_state.clone()),
        last_omemo_error: diagnostics.and_then(|value| value.last_omemo_error),
        encrypted_rooms_total: room_diagnostics
            .as_ref()
            .map(|value| value.encrypted_rooms_total)
            .unwrap_or(0),
        encrypted_rooms_ready: room_diagnostics
            .as_ref()
            .map(|value| value.encrypted_rooms_ready)
            .unwrap_or(0),
        last_room_error: room_diagnostics.and_then(|value| value.last_room_error),
    }))
}

async fn outbound_rate_limit_handler(
    State(state): State<AppState>,
    Json(request): Json<OutboundRateLimitRequest>,
) -> Result<Json<OutboundRateLimitResponse>, BridgeError> {
    let (configured, configured_limit, channel) = {
        let inner = state.inner.read().await;
        (
            inner.configured.is_some(),
            inner
                .configured
                .as_ref()
                .map(|config| config.max_messages_per_hour)
                .unwrap_or(0),
            inner.channel.clone(),
        )
    };

    let channel =
        channel.ok_or_else(|| BridgeError::conflict("xmpp-bridge is not configured yet"))?;
    let current = channel.outbound_rate_limit_diagnostics().await;
    let next_limit = request
        .max_messages_per_hour
        .unwrap_or(current.max_messages_per_hour);
    let updated = channel
        .set_outbound_rate_limit(next_limit, request.reset_counter)
        .await;

    tracing::info!(
        configured_max_messages_per_hour = configured_limit,
        active_max_messages_per_hour = updated.max_messages_per_hour,
        outbound_messages_last_hour = updated.messages_in_current_window,
        reset_counter = request.reset_counter,
        "Updated live XMPP outbound rate limit"
    );

    Ok(Json(OutboundRateLimitResponse {
        configured,
        running: true,
        configured_max_messages_per_hour: configured_limit,
        active_max_messages_per_hour: updated.max_messages_per_hour,
        outbound_messages_last_hour: updated.messages_in_current_window,
        reset_counter_applied: request.reset_counter,
    }))
}

async fn messages_handler(
    State(state): State<AppState>,
    Query(query): Query<MessagesQuery>,
) -> Result<Json<MessagesResponse>, BridgeError> {
    let cursor = query.cursor.unwrap_or(0);
    let inner = state.inner.read().await;
    let messages = inner
        .messages
        .iter()
        .filter(|message| message.cursor > cursor)
        .map(|message| message.payload.clone())
        .collect();

    Ok(Json(MessagesResponse {
        cursor: inner.current_cursor,
        messages,
    }))
}

async fn send_handler(
    State(state): State<AppState>,
    Json(request): Json<SendRequest>,
) -> Result<Json<serde_json::Value>, BridgeError> {
    let metadata: serde_json::Value = serde_json::from_str(&request.metadata_json)
        .map_err(|e| BridgeError::bad_request(&format!("invalid metadata_json: {}", e)))?;

    let channel = {
        let inner = state.inner.read().await;
        inner.channel.clone()
    }
    .ok_or_else(|| BridgeError::conflict("xmpp-bridge is not configured yet"))?;

    if request.attachments.is_empty() {
        let mut response = OutgoingResponse::text(request.content);
        response.metadata = metadata;
        channel
            .broadcast(&request.target, response)
            .await
            .map_err(|e| {
                BridgeError::bad_gateway(&format!("failed to send XMPP message: {}", e))
            })?;
    } else {
        let mut attachments = Vec::with_capacity(request.attachments.len());
        for attachment in request.attachments {
            let data = base64::engine::general_purpose::STANDARD
                .decode(attachment.data_base64.as_bytes())
                .map_err(|e| {
                    BridgeError::bad_request(&format!(
                        "invalid base64 for attachment '{}': {}",
                        attachment.filename, e
                    ))
                })?;
            attachments.push(OutboundAttachment {
                filename: attachment.filename,
                mime_type: attachment.mime_type,
                data,
            });
        }
        channel
            .broadcast_with_attachments(&request.target, request.content, &metadata, attachments)
            .await
            .map_err(|e| {
                BridgeError::bad_gateway(&format!("failed to send XMPP message: {}", e))
            })?;
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn enqueue_message(state: &AppState, message: IncomingMessage) {
    let metadata_json =
        serde_json::to_string(&message.metadata).unwrap_or_else(|_| "{}".to_string());
    let attachments: Vec<_> = message
        .attachments
        .iter()
        .filter(|a| !a.data.is_empty())
        .map(|a| openclaw_xmpp_bridge_contract::BridgeAttachment {
            filename: a.filename.clone().unwrap_or_else(|| "file".to_string()),
            mime_type: a.mime_type.clone(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(&a.data),
        })
        .collect();
    let payload = BridgeMessage {
        message_id: message.id.to_string(),
        user_id: message.user_id,
        sender_id: Some(message.sender_id),
        user_name: message.user_name,
        content: message.content,
        thread_id: message.thread_id,
        metadata_json,
        attachments,
    };

    let mut inner = state.inner.write().await;
    inner.current_cursor = inner.current_cursor.saturating_add(1);
    let cursor = inner.current_cursor;
    inner.messages.push_back(QueuedMessage { cursor, payload });
    while inner.messages.len() > state.max_messages {
        inner.messages.pop_front();
    }
}

fn authorize(state: &AppState, headers: &HeaderMap, remote: SocketAddr) -> Result<(), BridgeError> {
    if !remote.ip().is_loopback() {
        return Err(BridgeError::forbidden(
            "xmpp-bridge only accepts loopback clients",
        ));
    }

    let Some(expected_token) = state.expected_token.as_deref() else {
        return Ok(());
    };

    let header = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| BridgeError::unauthorized("missing Authorization header"))?;

    let provided = header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))
        .ok_or_else(|| BridgeError::unauthorized("Authorization header must use Bearer auth"))?;

    if provided != expected_token {
        return Err(BridgeError::unauthorized("invalid bridge token"));
    }

    Ok(())
}

fn normalize_config(request: &ConfigureRequest) -> Result<NormalizedConfig, BridgeError> {
    let jid = request.jid.trim();
    if jid.is_empty() {
        return Err(BridgeError::bad_request("jid is required"));
    }
    let password = request.password.trim();
    if password.is_empty() {
        return Err(BridgeError::bad_request("password is required"));
    }

    let resource = request
        .resource
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let jid = if jid.contains('/') || resource.is_none() {
        jid.to_string()
    } else {
        format!("{}/{}", jid, resource.unwrap_or_default())
    };

    let omemo_store_dir = request
        .omemo_store_dir
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| lunarwing_base_dir().join("xmpp"));

    Ok(NormalizedConfig {
        jid,
        password: password.to_string(),
        dm_policy: normalize_policy(&request.dm_policy),
        allow_from: normalize_list(&request.allow_from),
        allow_rooms: {
            let mut rooms = normalize_list(&request.allow_rooms);
            let encrypted_rooms = normalize_list(&request.encrypted_rooms);
            if !rooms.iter().any(|room| room == "*") {
                for room in &encrypted_rooms {
                    if !rooms
                        .iter()
                        .any(|value: &String| value.eq_ignore_ascii_case(room))
                    {
                        rooms.push(room.clone());
                    }
                }
            }
            rooms
        },
        encrypted_rooms: normalize_list(&request.encrypted_rooms),
        device_id: request.device_id,
        omemo_store_dir,
        allow_plaintext_fallback: request.allow_plaintext_fallback,
        max_messages_per_hour: request.max_messages_per_hour,
    })
}

fn to_native_config(config: &NormalizedConfig) -> XmppConfig {
    XmppConfig {
        jid: config.jid.clone(),
        password: SecretString::from(config.password.clone()),
        allow_from: config.allow_from.clone(),
        dm_policy: config.dm_policy.clone(),
        allow_rooms: config.allow_rooms.clone(),
        encrypted_rooms: config.encrypted_rooms.clone(),
        device_id: config.device_id,
        omemo_store_dir: config.omemo_store_dir.clone(),
        allow_plaintext_fallback: config.allow_plaintext_fallback,
        max_messages_per_hour: config.max_messages_per_hour,
    }
}

fn normalize_policy(policy: &str) -> String {
    let normalized = policy.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        "allowlist".to_string()
    } else {
        normalized
    }
}

fn normalize_list(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

struct BridgeError {
    status: StatusCode,
    message: String,
}

impl BridgeError {
    fn bad_request(message: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.to_string(),
        }
    }

    fn unauthorized(message: &str) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.to_string(),
        }
    }

    fn forbidden(message: &str) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.to_string(),
        }
    }

    fn conflict(message: &str) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.to_string(),
        }
    }

    fn bad_gateway(message: &str) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: message.to_string(),
        }
    }
}

impl IntoResponse for BridgeError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(serde_json::json!({
                "error": self.message,
            })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use openclaw_xmpp_bridge_contract::BridgeAttachment;
    use tower::ServiceExt;

    use super::*;

    fn test_state() -> AppState {
        AppState {
            inner: Arc::new(RwLock::new(BridgeInner::default())),
            expected_token: None,
            max_messages: DEFAULT_MAX_MESSAGES,
        }
    }

    fn test_state_with_token() -> AppState {
        AppState {
            expected_token: Some("bridge-token".to_string()),
            ..test_state()
        }
    }

    /// POST `body` to `path` from the given peer and return the response
    /// status. The bridge is intentionally left unconfigured, so a send body
    /// that survives extraction reaches the handler and surfaces as
    /// `409 CONFLICT`; a body rejected by the body limit never gets that far
    /// and surfaces as `413 PAYLOAD TOO LARGE`; a request rejected by the
    /// auth middleware (before any body is buffered) surfaces as
    /// `401 UNAUTHORIZED` / `403 FORBIDDEN`.
    async fn post_status(
        router: Router,
        path: &str,
        body: Vec<u8>,
        auth: Option<&str>,
        peer: &str,
    ) -> StatusCode {
        let mut builder = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json");
        if let Some(token) = auth {
            builder = builder.header("authorization", format!("Bearer {}", token));
        }
        let mut request = builder.body(Body::from(body)).expect("build request");
        request.extensions_mut().insert(ConnectInfo(
            peer.parse::<SocketAddr>().expect("parse peer address"),
        ));
        router
            .oneshot(request)
            .await
            .expect("router should answer request")
            .status()
    }

    async fn send_status_for_body(router: Router, body: Vec<u8>) -> StatusCode {
        post_status(router, "/v1/messages/send", body, None, "127.0.0.1:43210").await
    }

    #[tokio::test]
    async fn send_with_large_attachment_body_reaches_handler() {
        // Work item #42 regression: a photo-sized attachment (~2.25 MiB raw)
        // base64-encodes to ~3 MiB, which exceeded axum's 2 MiB default body
        // limit and failed with 413 before the handler ever ran.
        let raw = vec![0xAB_u8; 2_400_000];
        let request = SendRequest {
            target: "user@example.com".to_string(),
            content: String::new(),
            metadata_json: "{}".to_string(),
            attachments: vec![BridgeAttachment {
                filename: "photo.jpg".to_string(),
                mime_type: "image/jpeg".to_string(),
                data_base64: base64::engine::general_purpose::STANDARD.encode(raw),
            }],
        };
        let body = serde_json::to_vec(&request).expect("serialize send request");
        assert!(
            body.len() > 2 * 1024 * 1024,
            "regression test body must exceed the old 2 MiB default limit"
        );

        let status = send_status_for_body(build_router(test_state()), body).await;

        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "large attachment body must reach the handler (unconfigured bridge), not be rejected by the body limit"
        );
    }

    #[tokio::test]
    async fn send_body_stays_bounded_above_configured_limit() {
        let status = send_status_for_body(
            build_router_with_body_limit(test_state(), 1024),
            vec![0_u8; 2048],
        )
        .await;

        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn non_send_routes_keep_default_body_limit() {
        // docs/ops/MEDIUM.MD: the raised limit must be scoped to
        // /v1/messages/send; every other route keeps axum's 2 MiB default so
        // a local sprayer cannot make the bridge buffer huge bodies on cheap
        // control routes.
        let status = post_status(
            build_router(test_state()),
            "/v1/configure",
            vec![0_u8; 3 * 1024 * 1024],
            None,
            "127.0.0.1:43210",
        )
        .await;

        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn unauthorized_over_limit_body_rejected_before_buffering() {
        // docs/ops/MEDIUM.MD: authorize runs as middleware before the Json
        // extractor, so an unauthenticated over-limit body gets 401 (not
        // 413) and the bridge never buffers it.
        let status = post_status(
            build_router_with_body_limit(test_state_with_token(), 1024),
            "/v1/messages/send",
            vec![0_u8; 2048],
            None,
            "127.0.0.1:43210",
        )
        .await;

        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn non_loopback_peer_forbidden_before_buffering() {
        let status = post_status(
            build_router_with_body_limit(test_state(), 1024),
            "/v1/messages/send",
            vec![0_u8; 2048],
            None,
            "10.0.0.9:43210",
        )
        .await;

        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn authorized_send_passes_middleware_and_reaches_handler() {
        let status = post_status(
            build_router(test_state_with_token()),
            "/v1/messages/send",
            br#"{"target":"u@example.com","content":"","metadata_json":"{}","attachments":[]}"#
                .to_vec(),
            Some("bridge-token"),
            "127.0.0.1:43210",
        )
        .await;

        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "authorized request must pass the middleware and reach the handler (unconfigured bridge)"
        );
    }
}
