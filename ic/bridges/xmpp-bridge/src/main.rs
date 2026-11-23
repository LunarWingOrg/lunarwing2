use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

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
    BridgeMessage, BridgeStatusResponse, ConfigureRequest, ConfigureResponse, DrainRequest,
    DrainResponse, MessagesQuery, MessagesResponse, OutboundRateLimitRequest,
    OutboundRateLimitResponse, SendRequest,
};
use secrecy::SecretString;
use tokio::sync::RwLock;

const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8787";
const DEFAULT_MAX_MESSAGES: usize = 2048;

/// Number of messages a `/v1/messages` page returns when the caller does not
/// ask for a size.
///
/// Must stay comfortably below the daemon's WASM host per-callback emit limit
/// (`MAX_EMITS_PER_EXECUTION`, 100): a page a channel can emit inside a single
/// `on_poll` never trips the host limit, so the guest can persist its cursor
/// and drain the rest of the backlog on the next tick. Before paging, any
/// backlog above the limit re-fetched the same set every poll, was rejected
/// all-or-nothing, and never advanced the cursor — inbound XMPP stayed dead
/// until a restart (WI#850).
const DEFAULT_MESSAGES_PAGE_LIMIT: usize = 64;

/// Hard ceiling for any single page, even when a caller asks for more. A page
/// large enough to overflow a channel callback would recreate the WI#850
/// livelock, so the ceiling stays within the host's per-callback emit budget.
const MAX_MESSAGES_PAGE_LIMIT: usize = 100;
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
/// (`docs/ops/history/XMPP_MESSAGE_BODY_LIMIT_SECURITY_REVIEW.md`).
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
    /// When this process queued the message. Used for the queue-age report
    /// and the opt-in stale-queue policy (WI#854).
    queued_at: Instant,
    /// Included in a poll response, even if the poller hasn't acknowledged it.
    served: bool,
    payload: BridgeMessage,
}

impl QueuedMessage {
    fn new(cursor: u64, payload: BridgeMessage, now: Instant) -> Self {
        Self {
            cursor,
            queued_at: now,
            served: false,
            payload,
        }
    }

    fn age_secs(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.queued_at).as_secs()
    }
}

#[derive(Default)]
struct BridgeInner {
    configured: Option<NormalizedConfig>,
    jid: Option<String>,
    channel: Option<Arc<XmppChannel>>,
    current_cursor: u64,
    messages: VecDeque<QueuedMessage>,
    /// Highest cursor acknowledged by the poller or skipped by an explicit
    /// full drain. A poller sends back the cursor of the last page it
    /// dispatched and persisted to acknowledge delivery up to it (WI#850).
    acknowledged_cursor: u64,
    /// Cursor of the most recent `/v1/messages` response. A poll must echo it
    /// to advance `acknowledged_cursor`.
    last_response_cursor: u64,
    last_poll_at: Option<Instant>,
}

#[derive(Clone)]
struct AppState {
    inner: Arc<RwLock<BridgeInner>>,
    expected_token: Option<String>,
    max_messages: usize,
    /// Minimum poll gap and unserved message age for opt-in expiry; 0 disables it.
    stale_queue_secs: u64,
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
    let stale_queue_secs = stale_queue_policy(std::env::var("XMPP_BRIDGE_STALE_QUEUE_SECS"));
    tracing::info!(
        stale_queue_secs,
        "Effective XMPP stale-queue threshold (0 disables expiry)"
    );

    let state = AppState {
        inner: Arc::new(RwLock::new(BridgeInner::default())),
        expected_token,
        max_messages,
        stale_queue_secs,
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

fn stale_queue_policy(value: Result<String, std::env::VarError>) -> u64 {
    // Four nominal 30-second guest poll intervals.
    const MIN_IDLE_SECS: u64 = 120;
    match value {
        Ok(value) => match value.parse::<u64>() {
            Ok(0) => 0,
            Ok(secs) if secs >= MIN_IDLE_SECS => secs,
            _ => {
                tracing::warn!(
                    MIN_IDLE_SECS,
                    "Invalid XMPP_BRIDGE_STALE_QUEUE_SECS; expected 0 or an integer at or above the minimum"
                );
                0
            }
        },
        Err(std::env::VarError::NotPresent) => 0,
        Err(_) => {
            tracing::warn!("Unreadable XMPP_BRIDGE_STALE_QUEUE_SECS; expiry disabled");
            0
        }
    }
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
        .route("/v1/messages/drain", post(drain_handler))
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
/// the pre-auth memory-amplification window from `docs/ops/history/XMPP_MESSAGE_BODY_LIMIT_SECURITY_REVIEW.md`.
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
        pending_messages,
        oldest_queued_age_secs,
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
            pending_messages(&inner).count(),
            oldest_queued_age_secs(&inner, Instant::now()),
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
        pending_messages,
        oldest_queued_age_secs,
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
    let limit = query
        .limit
        .unwrap_or(DEFAULT_MESSAGES_PAGE_LIMIT)
        .clamp(1, MAX_MESSAGES_PAGE_LIMIT);
    let mut inner = state.inner.write().await;
    let now = Instant::now();
    let start = poll_start(&mut inner, cursor);
    let previous_poll = inner.last_poll_at.replace(now);
    if state.stale_queue_secs > 0
        && previous_poll.is_some_and(|last| {
            now.saturating_duration_since(last).as_secs() >= state.stale_queue_secs
        })
    {
        let dropped = drop_queued_messages(&mut inner, Some(state.stale_queue_secs), now);
        if dropped > 0 {
            tracing::warn!(
                dropped,
                stale_queue_secs = state.stale_queue_secs,
                current_cursor = inner.current_cursor,
                "Dropped stale never-served inbound messages after a poll gap (WI#854)"
            );
        }
    }
    let mut messages = Vec::new();
    let mut next_cursor = start;
    for message in inner
        .messages
        .iter_mut()
        .filter(|message| message.cursor > start)
        .take(limit)
    {
        // Remember every served entry, not just the last response cursor:
        // a smaller retry or an empty resync can change that cursor.
        message.served = true;
        next_cursor = message.cursor;
        messages.push(message.payload.clone());
    }
    // Advance only past the messages actually returned. Reporting the bridge's
    // newest cursor here would skip the rest of the backlog as soon as a page
    // is capped (WI#850).
    let response = MessagesResponse {
        cursor: resync_cursor(cursor, inner.current_cursor, next_cursor),
        messages,
    };
    inner.last_response_cursor = response.cursor;

    Ok(Json(response))
}

/// Discard queued inbound messages on operator request (WI#854).
async fn drain_handler(
    State(state): State<AppState>,
    Json(request): Json<DrainRequest>,
) -> Result<Json<DrainResponse>, BridgeError> {
    if request.all == request.older_than_secs.is_some() {
        return Err(BridgeError::bad_request(
            "specify exactly one of all=true or older_than_secs",
        ));
    }
    let mut inner = state.inner.write().await;
    let now = Instant::now();
    let drained = drop_queued_messages(&mut inner, request.older_than_secs, now);
    let remaining = pending_messages(&inner).count();
    if drained > 0 {
        tracing::warn!(
            drained,
            older_than_secs = ?request.older_than_secs,
            remaining,
            current_cursor = inner.current_cursor,
            "Operator drained queued inbound XMPP messages (WI#854)"
        );
    }
    Ok(Json(DrainResponse {
        drained,
        remaining,
        oldest_queued_age_secs: oldest_queued_age_secs(&inner, now),
    }))
}

fn pending_messages(inner: &BridgeInner) -> impl Iterator<Item = &QueuedMessage> {
    inner
        .messages
        .iter()
        .filter(|message| message.cursor > inner.acknowledged_cursor)
}

/// Age of the oldest unacknowledged message still retained for the poller.
fn oldest_queued_age_secs(inner: &BridgeInner, now: Instant) -> Option<u64> {
    pending_messages(inner)
        .next()
        .map(|message| message.age_secs(now))
}

/// Drop pending entries, keeping served entries for retry unless explicitly
/// discarding everything. Acknowledged history is always retained.
fn drop_queued_messages(
    inner: &mut BridgeInner,
    older_than_secs: Option<u64>,
    now: Instant,
) -> usize {
    let before = inner.messages.len();
    let acknowledged = inner.acknowledged_cursor;
    inner.messages.retain(|message| {
        message.cursor <= acknowledged
            || older_than_secs.is_some_and(|limit| message.served || message.age_secs(now) < limit)
    });
    let dropped = before - inner.messages.len();
    // Age-based discard leaves gaps that pagination can skip without
    // acknowledging a retained retry. Only a full drain can move this cursor.
    if dropped > 0 && older_than_secs.is_none() {
        inner.acknowledged_cursor = inner.current_cursor;
    }
    dropped
}

/// The cursor a poll reads after, given what the poller has acknowledged.
///
/// The daemon keeps its cursor in memory, so a daemon-only restart or a
/// re-activated channel polls again from zero while this bridge keeps its
/// ring of already-delivered messages. Serving from the request cursor would
/// replay up to the whole ring as new inbound turns (WI#850). A request
/// cursor below the acknowledged one can only come from such a reset, so the
/// poll resumes after the last acknowledged message instead: nothing
/// delivered is replayed, and messages queued while the daemon was down
/// (cursors above the acknowledged one) are still served. A page that was
/// handed out but not yet acknowledged is served again if still retained,
/// so delivery stays at-least-once. A cursor ahead of this process is the
/// bridge-restart case that `resync_cursor` handles; its page is empty.
fn poll_start(inner: &mut BridgeInner, request_cursor: u64) -> u64 {
    if request_cursor > inner.current_cursor {
        return request_cursor;
    }
    if request_cursor == inner.last_response_cursor {
        inner.acknowledged_cursor = inner.acknowledged_cursor.max(request_cursor);
    }
    if request_cursor < inner.acknowledged_cursor {
        tracing::info!(
            request_cursor,
            acknowledged_cursor = inner.acknowledged_cursor,
            "Poll cursor is behind the acknowledged cursor (daemon-side reset); resuming after the last delivered message instead of replaying"
        );
        return inner.acknowledged_cursor;
    }
    request_cursor
}

/// Keep a poller's cursor usable after the bridge process restarts.
///
/// The bridge's cursor sequence is per-process and starts over at zero. A
/// poller that persisted a cursor from the previous process would otherwise
/// sit permanently ahead of every future message and never receive anything
/// again. A request cursor above the bridge's current cursor can only mean such
/// a restart, so hand back the live cursor to resynchronize; that poll returns
/// no messages, and the next poll starts from the bridge's current position.
fn resync_cursor(request_cursor: u64, current_cursor: u64, page_cursor: u64) -> u64 {
    if request_cursor > current_cursor {
        tracing::info!(
            request_cursor,
            current_cursor,
            "Poll cursor is ahead of this bridge process (bridge restart); resynchronizing poller"
        );
        return current_cursor;
    }
    page_cursor
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
    let queued = QueuedMessage::new(cursor, payload, Instant::now());
    inner.messages.push_back(queued);
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
            stale_queue_secs: 0,
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
        // docs/ops/history/XMPP_MESSAGE_BODY_LIMIT_SECURITY_REVIEW.md: the raised limit must be scoped to
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
        // docs/ops/history/XMPP_MESSAGE_BODY_LIMIT_SECURITY_REVIEW.md: authorize runs as middleware before the Json
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

    /// Seed the bridge ring with `count` inbound messages and return the state.
    fn state_with_backlog(count: usize) -> AppState {
        let state = test_state();
        let mut inner = state.inner.try_write().expect("bridge state lock");
        for i in 0..count {
            inner.current_cursor += 1;
            let cursor = inner.current_cursor;
            let payload = BridgeMessage {
                message_id: format!("m{i}"),
                user_id: "user".to_string(),
                sender_id: None,
                user_name: None,
                content: format!("message {i}"),
                thread_id: None,
                metadata_json: "{}".to_string(),
                attachments: Vec::new(),
            };
            let queued = QueuedMessage::new(cursor, payload, Instant::now());
            inner.messages.push_back(queued);
        }
        drop(inner);
        state
    }

    async fn get_messages(router: Router, uri: &str) -> MessagesResponse {
        let body = get_body(router, uri).await;
        serde_json::from_slice(&body).expect("parse messages response")
    }

    async fn get_body(router: Router, uri: &str) -> axum::body::Bytes {
        let mut request = Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .expect("build request");
        request.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:43210"
                .parse::<SocketAddr>()
                .expect("parse peer address"),
        ));
        let response = router
            .oneshot(request)
            .await
            .expect("router should answer request");
        assert_eq!(response.status(), StatusCode::OK);
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read GET response body")
    }

    #[tokio::test]
    async fn messages_page_defaults_below_emit_limit_and_advances_cursor() {
        // WI#850: a backlog larger than the daemon's per-callback emit limit
        // must come back in bounded pages so the guest can persist its cursor
        // after each page instead of tripping the host limit every tick.
        let state = state_with_backlog(150);
        let page = get_messages(build_router(state), "/v1/messages?cursor=0").await;

        assert_eq!(page.messages.len(), DEFAULT_MESSAGES_PAGE_LIMIT);
        assert_eq!(page.cursor, DEFAULT_MESSAGES_PAGE_LIMIT as u64);
        assert_eq!(page.messages[0].message_id, "m0");
    }

    #[tokio::test]
    async fn messages_page_honors_limit_and_resumes_without_skipping() {
        let state = state_with_backlog(150);
        let router = build_router(state);

        let first = get_messages(router.clone(), "/v1/messages?cursor=0&limit=10").await;
        assert_eq!(first.messages.len(), 10);
        assert_eq!(first.cursor, 10);

        let second = get_messages(
            router.clone(),
            &format!("/v1/messages?cursor={}&limit=10", first.cursor),
        )
        .await;
        assert_eq!(second.messages.len(), 10);
        assert_eq!(second.cursor, 20);
        assert_eq!(
            second.messages[0].message_id, "m10",
            "second page must resume at the first undelivered message"
        );
    }

    #[tokio::test]
    async fn messages_page_clamps_oversized_limit() {
        // A caller asking for more than the host can emit in one callback gets
        // the ceiling, not its request: a page that overflows the channel
        // recreates the WI#850 livelock.
        let state = state_with_backlog(300);
        let page = get_messages(build_router(state), "/v1/messages?cursor=0&limit=5000").await;

        assert_eq!(page.messages.len(), MAX_MESSAGES_PAGE_LIMIT);
        assert_eq!(page.cursor, MAX_MESSAGES_PAGE_LIMIT as u64);
    }

    #[tokio::test]
    async fn messages_page_with_no_new_messages_keeps_cursor() {
        let state = state_with_backlog(3);
        let page = get_messages(build_router(state), "/v1/messages?cursor=99").await;

        assert!(
            page.messages.is_empty(),
            "a cursor from a previous bridge process must not see stale messages"
        );
        assert_eq!(
            page.cursor, 3,
            "a poller ahead of a restarted bridge must be resynchronized, not wedged"
        );
    }

    #[tokio::test]
    async fn messages_page_at_the_newest_message_keeps_cursor() {
        let state = state_with_backlog(3);
        let page = get_messages(build_router(state), "/v1/messages?cursor=3").await;

        assert!(page.messages.is_empty());
        assert_eq!(page.cursor, 3, "an idle poll must not move the cursor");
    }

    /// Queue `count` more inbound messages after the existing ones.
    async fn enqueue_backlog(state: &AppState, count: usize) {
        let mut inner = state.inner.write().await;
        for _ in 0..count {
            inner.current_cursor += 1;
            let cursor = inner.current_cursor;
            let payload = BridgeMessage {
                message_id: format!("m{}", cursor - 1),
                user_id: "user".to_string(),
                sender_id: None,
                user_name: None,
                content: format!("message {}", cursor - 1),
                thread_id: None,
                metadata_json: "{}".to_string(),
                attachments: Vec::new(),
            };
            let queued = QueuedMessage::new(cursor, payload, Instant::now());
            inner.messages.push_back(queued);
        }
    }

    #[tokio::test]
    async fn messages_reset_poller_resumes_after_the_acknowledged_cursor() {
        // WI#850: the daemon keeps its cursor in memory, so a daemon-only
        // restart polls again from zero while this bridge still holds the
        // delivered messages. They must not come back as new inbound turns.
        let state = state_with_backlog(150);
        let router = build_router(state);

        let first = get_messages(router.clone(), "/v1/messages?cursor=0&limit=10").await;
        assert_eq!(first.cursor, 10);
        // Echoing the cursor acknowledges the first page.
        let second = get_messages(router.clone(), "/v1/messages?cursor=10&limit=10").await;
        assert_eq!(second.cursor, 20);

        let after_reset = get_messages(router.clone(), "/v1/messages?cursor=0&limit=10").await;
        assert_eq!(
            after_reset.messages[0].message_id, "m10",
            "a reset poller resumes after the acknowledged page, not at the start of the ring"
        );
        assert_eq!(after_reset.cursor, 20);
    }

    #[tokio::test]
    async fn messages_unacknowledged_page_is_served_again() {
        // A poller that failed to dispatch a page asks with its old cursor
        // again; that page must come back (at-least-once), not be skipped.
        let state = state_with_backlog(30);
        let router = build_router(state);

        let first = get_messages(router.clone(), "/v1/messages?cursor=0&limit=10").await;
        assert_eq!(first.cursor, 10);
        let retry = get_messages(router.clone(), "/v1/messages?cursor=0&limit=10").await;
        assert_eq!(retry.messages[0].message_id, "m0");
        assert_eq!(retry.cursor, 10);
    }

    #[tokio::test]
    async fn messages_reset_poller_still_gets_messages_queued_while_down() {
        let state = state_with_backlog(5);
        let router = build_router(state.clone());

        let first = get_messages(router.clone(), "/v1/messages?cursor=0").await;
        assert_eq!(first.cursor, 5);
        let idle = get_messages(router.clone(), "/v1/messages?cursor=5").await;
        assert!(idle.messages.is_empty());

        // Messages arrive while the daemon is down, then it polls from zero.
        enqueue_backlog(&state, 3).await;
        let after_reset = get_messages(router.clone(), "/v1/messages?cursor=0").await;
        let ids: Vec<_> = after_reset
            .messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect();
        assert_eq!(
            ids,
            ["m5", "m6", "m7"],
            "only the undelivered messages come back"
        );
        assert_eq!(after_reset.cursor, 8);
    }

    #[tokio::test]
    async fn messages_pages_cover_the_whole_backlog_in_order() {
        let state = state_with_backlog(150);
        let router = build_router(state);
        let mut cursor = 0;
        let mut delivered = Vec::new();

        loop {
            let page = get_messages(router.clone(), &format!("/v1/messages?cursor={cursor}")).await;
            if page.messages.is_empty() {
                break;
            }
            assert!(page.cursor > cursor, "a non-empty page must advance");
            cursor = page.cursor;
            delivered.extend(page.messages.into_iter().map(|m| m.message_id));
        }

        assert_eq!(delivered.len(), 150);
        for (i, id) in delivered.iter().enumerate() {
            assert_eq!(id, &format!("m{i}"), "paging must not skip or reorder");
        }
    }

    /// Backdate every queued message by `secs` so the stale-queue policy and
    /// age-bounded drain are testable without sleeping.
    async fn backdate_backlog(state: &AppState, secs: u64) {
        let mut inner = state.inner.write().await;
        let now = Instant::now();
        let aged = now
            .checked_sub(std::time::Duration::from_secs(secs))
            .unwrap_or(now);
        for message in inner.messages.iter_mut() {
            message.queued_at = aged;
        }
    }

    async fn post_drain(router: Router, body: &str) -> DrainResponse {
        let mut request = Request::builder()
            .method("POST")
            .uri("/v1/messages/drain")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("build request");
        request.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:43210"
                .parse::<SocketAddr>()
                .expect("parse peer address"),
        ));
        let response = router
            .oneshot(request)
            .await
            .expect("router should answer request");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read drain body");
        serde_json::from_slice(&body).expect("parse drain response")
    }

    #[tokio::test]
    async fn drain_all_discards_even_the_unacknowledged_page() {
        let state = state_with_backlog(5);
        let router = build_router(state);

        let served = get_messages(router.clone(), "/v1/messages?cursor=0&limit=2").await;
        assert_eq!(served.cursor, 2);
        let drained = post_drain(router.clone(), r#"{"all":true}"#).await;
        assert_eq!(
            drained.drained, 5,
            "all also discards the unacknowledged page"
        );
        assert_eq!(drained.remaining, 0);
        assert_eq!(drained.oldest_queued_age_secs, None);

        let page = get_messages(router.clone(), "/v1/messages?cursor=0").await;
        assert!(page.messages.is_empty(), "a drained queue serves nothing");
        assert_eq!(page.cursor, 5, "the cursor sequence must not reset");
    }

    #[tokio::test]
    async fn acknowledged_history_is_not_pending_or_drained() {
        let state = state_with_backlog(2);
        backdate_backlog(&state, 600).await;
        let router = build_router(state.clone());
        get_messages(router.clone(), "/v1/messages?cursor=0").await;
        get_messages(router.clone(), "/v1/messages?cursor=2").await;
        let body = get_body(router.clone(), "/v1/status").await;
        let status: BridgeStatusResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(status.queued_messages, 2);
        assert_eq!(status.pending_messages, 0);
        assert_eq!(status.oldest_queued_age_secs, None);
        assert_eq!(
            post_drain(router.clone(), r#"{"all":true}"#).await.drained,
            0
        );
        enqueue_backlog(&state, 1).await;
        let body = get_body(router.clone(), "/v1/status").await;
        let status: BridgeStatusResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(status.pending_messages, 1);
        assert!(status.oldest_queued_age_secs.unwrap() < 600);
        assert_eq!(post_drain(router, r#"{"all":true}"#).await.drained, 1);
        assert_eq!(state.inner.read().await.messages.len(), 2);
    }

    #[tokio::test]
    async fn invalid_or_unauthorized_drain_leaves_pending_messages() {
        let state = AppState {
            expected_token: Some("bridge-token".into()),
            ..state_with_backlog(2)
        };
        let router = build_router(state.clone());
        let path = "/v1/messages/drain";
        let peer = "127.0.0.1:43210";
        let json = br#"{"all":true}"#.to_vec();
        assert_eq!(
            post_status(router.clone(), path, json.clone(), None, peer).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            post_status(
                router.clone(),
                path,
                json,
                Some("bridge-token"),
                "192.0.2.1:43210"
            )
            .await,
            StatusCode::FORBIDDEN
        );
        for (body, expected) in [
            ("", StatusCode::BAD_REQUEST),
            ("{}", StatusCode::BAD_REQUEST),
            (
                r#"{"all":false,"older_than_secs":null}"#,
                StatusCode::BAD_REQUEST,
            ),
            (
                r#"{"all":true,"older_than_secs":10}"#,
                StatusCode::BAD_REQUEST,
            ),
            (r#"{"older_than":10}"#, StatusCode::UNPROCESSABLE_ENTITY),
            ("[true,null]", StatusCode::UNPROCESSABLE_ENTITY),
        ] {
            assert_eq!(
                post_status(
                    router.clone(),
                    path,
                    body.as_bytes().to_vec(),
                    Some("bridge-token"),
                    peer
                )
                .await,
                expected
            );
        }
        let mut request = Request::post(path)
            .header("authorization", "Bearer bridge-token")
            .body(Body::from(r#"{"older_than_secs":10}"#))
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
        assert_eq!(
            router.oneshot(request).await.unwrap().status(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        assert_eq!(pending_messages(&*state.inner.read().await).count(), 2);
    }

    #[tokio::test]
    async fn drain_age_threshold_is_inclusive() {
        let state = state_with_backlog(2);
        let mut inner = state.inner.write().await;
        let now = Instant::now();
        inner.messages[0].queued_at = now - std::time::Duration::from_secs(60);
        inner.messages[1].queued_at = now - std::time::Duration::from_secs(59);
        assert_eq!(drop_queued_messages(&mut inner, Some(60), now), 1);
        assert_eq!(inner.messages[0].cursor, 2, "the younger message remains");
    }

    #[tokio::test]
    async fn drain_with_age_bound_keeps_served_and_recent_messages() {
        let state = state_with_backlog(4);
        backdate_backlog(&state, 120).await;
        enqueue_backlog(&state, 2).await;
        let router = build_router(state.clone());

        let served = get_messages(router.clone(), "/v1/messages?cursor=0&limit=2").await;
        assert_eq!(served.cursor, 2);
        // A smaller retry must not make the rest of the previously served page
        // eligible for discard just because the latest response ends sooner.
        let shorter = get_messages(router.clone(), "/v1/messages?cursor=0&limit=1").await;
        assert_eq!(shorter.cursor, 1);
        let drained = post_drain(router.clone(), r#"{"older_than_secs":60}"#).await;
        assert_eq!(
            drained.drained, 2,
            "only aged, unserved messages are discarded"
        );
        assert_eq!(
            drained.remaining, 4,
            "the unacknowledged page is still pending"
        );
        assert!(drained.oldest_queued_age_secs.is_some_and(|age| age >= 120));
        assert_eq!(state.inner.read().await.acknowledged_cursor, 0);

        let retry = get_messages(router.clone(), "/v1/messages?cursor=0&limit=2").await;
        assert_eq!(retry, served, "the served page survives an age-based drain");
        let page = get_messages(router.clone(), "/v1/messages?cursor=2").await;
        let ids: Vec<_> = page
            .messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect();
        assert_eq!(ids, ["m4", "m5"], "fresh messages must still be served");
        assert_eq!(
            page.cursor, 6,
            "the next page crosses the discarded cursor gap"
        );
        let idle = get_messages(router.clone(), "/v1/messages?cursor=6").await;
        assert!(idle.messages.is_empty());
        let reset = get_messages(router, "/v1/messages?cursor=0").await;
        assert!(reset.messages.is_empty(), "acknowledged pages stay retired");
    }

    #[tokio::test]
    async fn drain_with_age_bound_does_not_treat_an_empty_resync_as_delivery() {
        let state = state_with_backlog(2);
        backdate_backlog(&state, 120).await;
        let router = build_router(state);

        let resync = get_messages(router.clone(), "/v1/messages?cursor=99").await;
        assert!(resync.messages.is_empty());
        assert_eq!(resync.cursor, 2);
        let drained = post_drain(router, r#"{"older_than_secs":60}"#).await;
        assert_eq!(drained.drained, 2, "neither message was served");
        assert_eq!(drained.remaining, 0);
    }

    #[tokio::test]
    async fn stale_queue_policy_expires_pending_messages_only_after_poll_gap() {
        // `state_with_backlog` and the struct update share the same inner
        // queue, so the policy sees the seeded messages.
        let state = AppState {
            stale_queue_secs: 120,
            ..state_with_backlog(4)
        };
        backdate_backlog(&state, 300).await;
        enqueue_backlog(&state, 2).await;
        let router = build_router(state.clone());

        // First contact and ordinary pagination preserve even aged messages.
        let first = get_messages(router.clone(), "/v1/messages?cursor=0&limit=1").await;
        assert_eq!(first.messages[0].message_id, "m0");
        let second = get_messages(router.clone(), "/v1/messages?cursor=1&limit=1").await;
        assert_eq!(second.messages[0].message_id, "m1");
        state.inner.write().await.last_poll_at =
            Some(Instant::now() - std::time::Duration::from_secs(120));

        let retry = get_messages(router.clone(), "/v1/messages?cursor=0&limit=1").await;
        assert_eq!(
            retry, second,
            "expiry must not discard an unacknowledged page"
        );
        {
            let inner = state.inner.read().await;
            let retained: Vec<_> = inner
                .messages
                .iter()
                .map(|message| message.cursor)
                .collect();
            assert_eq!(
                retained,
                [1, 2, 5, 6],
                "only aged, unserved entries are gone"
            );
            assert_eq!(inner.acknowledged_cursor, 1, "expiry cannot skip the retry");
        }

        let page = get_messages(router, "/v1/messages?cursor=2").await;
        let ids: Vec<_> = page
            .messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect();
        assert_eq!(ids, ["m4", "m5"], "only the fresh messages may be served");
    }

    #[test]
    fn stale_queue_policy_requires_zero_or_at_least_120_seconds() {
        for value in ["", "60s", " 120", "-1", "119", "0"] {
            assert_eq!(stale_queue_policy(Ok(value.into())), 0);
        }
        assert_eq!(stale_queue_policy(Ok("120".into())), 120);
        assert_eq!(stale_queue_policy(Err(std::env::VarError::NotPresent)), 0);
    }

    #[tokio::test]
    async fn stale_queue_policy_is_off_by_default() {
        // Preserve the pre-WI#854 behavior unless an operator opts in: an old
        // backlog is still delivered rather than silently dropped.
        let state = state_with_backlog(3);
        backdate_backlog(&state, 86_400).await;
        let page = get_messages(build_router(state), "/v1/messages?cursor=0").await;

        assert_eq!(page.messages.len(), 3);
    }
}
