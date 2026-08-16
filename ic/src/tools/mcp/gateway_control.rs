//! Bundled MCP gateway control-plane service.
//!
//! The MCP gateway client contract (dedicated/shared placement, host-owned
//! routing headers) previously assumed an operator-deployed external gateway.
//! This module bundles that gateway into LunarWing as an optional service:
//!
//! - **Control plane** (`/admin/routes`): bearer-authenticated CRUD over a
//!   route table mapping shared-gateway route names to upstream MCP servers.
//!   The table persists to a JSON file with atomic writes.
//! - **Data plane** (`/mcp`): speaks the exact header contract the client
//!   sends — `GATEWAY_MODE_HEADER` and [`GATEWAY_ROUTE_HEADER`] — looks the
//!   route up, strips the routing headers, optionally injects the route's
//!   upstream bearer token, and proxies the request to the upstream MCP
//!   server. `text/event-stream` responses stream chunk-by-chunk without a
//!   cumulative byte cap; all other responses are buffered under
//!   [`MAX_PROXY_BODY_BYTES`] so upstream read failures still map to a clean
//!   `502` before any status line is sent.
//!
//! Security invariants:
//! - Admin endpoints require a constant-time-compared bearer token.
//! - Client-supplied `X-LunarWing-*` routing headers never reach upstreams.
//! - Upstreams must be HTTPS (loopback HTTP allowed for development).
//! - Request bodies and buffered upstream responses are capped at
//!   [`MAX_PROXY_BODY_BYTES`]; connect, time-to-headers, and per-read stalls
//!   time out so a silent upstream cannot pin a stream forever.
//! - Concurrent proxied `/mcp` connections are bounded by
//!   [`MAX_CONCURRENT_PROXY_CONNECTIONS`]; saturation answers `503` and every
//!   permit is released when its stream completes or the client disconnects.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, delete, get, put};
use axum::{Json, Router};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use crate::tools::mcp::config::is_localhost_url;
use crate::tools::mcp::gateway_transport::GATEWAY_ROUTE_HEADER;

/// Maximum routes in one gateway route table.
pub const MAX_GATEWAY_ROUTES: usize = 256;
/// Maximum proxied request body or buffered upstream response body (4 MiB).
///
/// `text/event-stream` responses are forwarded chunk-by-chunk and are *not*
/// subject to this cumulative cap — buffering never accumulates for streams,
/// so a lifetime byte total would only sever healthy long-lived SSE.
pub const MAX_PROXY_BODY_BYTES: usize = 4 * 1024 * 1024;
/// Upstream connect / time-to-headers / buffered-body timeout, and the
/// per-read stall timeout for streaming responses (resets on every chunk).
const DEFAULT_UPSTREAM_TIMEOUT: Duration = Duration::from_secs(30);

/// Deadlines bounding upstream proxy work.
///
/// Tests inject short values so stall/timeout behavior is exercisable without
/// waiting out the 30 s production default.
#[derive(Debug, Clone, Copy)]
struct UpstreamTimeouts {
    /// Connect, time-to-headers, and buffered-body overall deadline.
    overall: Duration,
    /// Per-read stall deadline for streaming bodies (resets on every chunk).
    read: Duration,
}

impl Default for UpstreamTimeouts {
    fn default() -> Self {
        Self {
            overall: DEFAULT_UPSTREAM_TIMEOUT,
            read: DEFAULT_UPSTREAM_TIMEOUT,
        }
    }
}
/// Maximum concurrent proxied `/mcp` connections. Long-lived SSE streams hold
/// two sockets each, so ownership must be bounded independently of body size;
/// excess requests fail fast with `503` instead of queueing unboundedly.
const MAX_CONCURRENT_PROXY_CONNECTIONS: usize = 64;

/// Error returned by the bundled gateway control plane.
#[derive(Debug, thiserror::Error)]
pub enum GatewayControlError {
    #[error("gateway control-plane IO error at '{path}': {reason}")]
    Io { path: String, reason: String },
    #[error("invalid gateway control-plane data: {reason}")]
    InvalidData { reason: String },
    #[error("gateway control-plane bind failed on {addr}: {reason}")]
    Bind { addr: String, reason: String },
}

/// One upstream MCP server behind the shared gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayRoute {
    /// Upstream MCP endpoint URL (HTTPS; loopback HTTP allowed).
    pub upstream: String,
    /// Bearer token injected as the upstream `Authorization` header.
    /// Never returned by the admin API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_token: Option<String>,
    /// Human-readable description (advisory).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl GatewayRoute {
    pub fn validate(&self) -> Result<(), GatewayControlError> {
        let invalid = |reason: &str| GatewayControlError::InvalidData {
            reason: reason.to_string(),
        };
        let parsed = url::Url::parse(&self.upstream)
            .map_err(|error| invalid(&format!("invalid upstream URL: {error}")))?;
        let secure = parsed.scheme() == "https";
        let local = parsed.scheme() == "http" && is_localhost_url(&self.upstream);
        if !secure && !local {
            return Err(invalid("upstreams must use HTTPS (loopback HTTP allowed)"));
        }
        if let Some(token) = &self.bearer_token
            && (token.is_empty() || token.bytes().any(|b| b < 0x21 || b == 0x7f))
        {
            return Err(invalid("bearer token cannot be empty or contain controls"));
        }
        if let Some(description) = &self.description
            && (description.len() > 512 || description.chars().any(char::is_control))
        {
            return Err(invalid("description must be <= 512 chars without controls"));
        }
        Ok(())
    }
}

/// Validate a shared-gateway route name (same contract as the client config).
fn validate_route_name(name: &str) -> Result<(), GatewayControlError> {
    let valid = !name.is_empty()
        && name == name.trim()
        && name.len() <= 128
        && HeaderValue::from_str(name).is_ok();
    if !valid {
        return Err(GatewayControlError::InvalidData {
            reason: format!("invalid route name '{name}'"),
        });
    }
    Ok(())
}

/// Serializable route table file.
#[derive(Debug, Default, Serialize, Deserialize)]
struct RouteTableFile {
    #[serde(default)]
    routes: BTreeMap<String, GatewayRoute>,
}

/// Disk-backed route table with atomic persistence.
struct RouteStore {
    path: PathBuf,
    routes: RwLock<BTreeMap<String, GatewayRoute>>,
}

impl RouteStore {
    fn load(path: PathBuf) -> Result<Self, GatewayControlError> {
        let io_error = |reason: String| GatewayControlError::Io {
            path: path.to_string_lossy().into_owned(),
            reason,
        };
        let routes = match std::fs::read(&path) {
            Ok(bytes) => {
                if bytes.len() > MAX_PROXY_BODY_BYTES {
                    return Err(GatewayControlError::InvalidData {
                        reason: "route table file too large".to_string(),
                    });
                }
                let file: RouteTableFile =
                    serde_json::from_slice(&bytes).map_err(|error| io_error(error.to_string()))?;
                for (name, route) in &file.routes {
                    validate_route_name(name)?;
                    route.validate()?;
                }
                file.routes
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(io_error(error.to_string())),
        };
        Ok(Self {
            path,
            routes: RwLock::new(routes),
        })
    }

    fn persist(&self) -> Result<(), GatewayControlError> {
        let io_error = |reason: String| GatewayControlError::Io {
            path: self.path.to_string_lossy().into_owned(),
            reason,
        };
        let guard = self
            .routes
            .read()
            .map_err(|_| GatewayControlError::InvalidData {
                reason: "route table lock poisoned".to_string(),
            })?;
        let file = RouteTableFile {
            routes: guard.clone(),
        };
        drop(guard);
        let bytes =
            serde_json::to_vec_pretty(&file).map_err(|error| io_error(error.to_string()))?;
        let Some(parent) = self.path.parent() else {
            return Err(io_error("route table path has no parent".to_string()));
        };
        let temporary = parent.join(".gateway-routes.tmp");
        std::fs::write(&temporary, &bytes).map_err(|error| io_error(error.to_string()))?;
        std::fs::rename(&temporary, &self.path).map_err(|error| io_error(error.to_string()))
    }

    fn get(&self, name: &str) -> Option<GatewayRoute> {
        self.routes
            .read()
            .ok()
            .and_then(|guard| guard.get(name).cloned())
    }

    fn upsert(&self, name: &str, route: GatewayRoute) -> Result<(), GatewayControlError> {
        validate_route_name(name)?;
        route.validate()?;
        {
            let mut guard =
                self.routes
                    .write()
                    .map_err(|error| GatewayControlError::InvalidData {
                        reason: format!("route table lock poisoned: {error}"),
                    })?;
            if !guard.contains_key(name) && guard.len() >= MAX_GATEWAY_ROUTES {
                return Err(GatewayControlError::InvalidData {
                    reason: format!("route table is full ({MAX_GATEWAY_ROUTES} routes)"),
                });
            }
            guard.insert(name.to_string(), route);
        }
        self.persist()
    }

    fn remove(&self, name: &str) -> Result<bool, GatewayControlError> {
        let removed = {
            let mut guard =
                self.routes
                    .write()
                    .map_err(|error| GatewayControlError::InvalidData {
                        reason: format!("route table lock poisoned: {error}"),
                    })?;
            guard.remove(name).is_some()
        };
        if removed {
            self.persist()?;
        }
        Ok(removed)
    }

    /// List routes with bearer tokens redacted.
    fn list_redacted(&self) -> BTreeMap<String, RedactedRoute> {
        self.routes
            .read()
            .map(|guard| {
                guard
                    .iter()
                    .map(|(name, route)| {
                        (
                            name.clone(),
                            RedactedRoute {
                                upstream: route.upstream.clone(),
                                has_bearer_token: route.bearer_token.is_some(),
                                description: route.description.clone(),
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Route view returned by the admin API (never includes the bearer token).
#[derive(Debug, Serialize)]
struct RedactedRoute {
    upstream: String,
    has_bearer_token: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

/// Configuration for the bundled gateway control-plane service.
pub struct GatewayControlConfig {
    /// Address to bind (e.g. `127.0.0.1:9700`).
    pub bind: SocketAddr,
    /// Bearer token required on `/admin/*` endpoints.
    pub admin_token: SecretString,
    /// Path of the persisted route table JSON file.
    pub routes_path: PathBuf,
}

/// Bind address enabling the bundled MCP gateway control plane.
pub const GATEWAY_CONTROL_BIND_ENV: &str = "LUNARWING_MCP_GATEWAY_CONTROL_BIND";
/// Bearer token required on the control-plane `/admin/*` endpoints.
pub const GATEWAY_CONTROL_ADMIN_TOKEN_ENV: &str = "LUNARWING_MCP_GATEWAY_ADMIN_TOKEN";
/// Override for the persisted route table path
/// (default: `<lunarwing base dir>/mcp-gateway-routes.json`).
pub const GATEWAY_CONTROL_ROUTES_PATH_ENV: &str = "LUNARWING_MCP_GATEWAY_ROUTES_PATH";
/// Explicit opt-in to binding the control plane off-loopback.
pub const GATEWAY_CONTROL_ALLOW_NON_LOOPBACK_ENV: &str =
    "LUNARWING_MCP_GATEWAY_CONTROL_ALLOW_NON_LOOPBACK";

/// Minimum accepted admin token length; shorter tokens are rejected.
const MIN_ADMIN_TOKEN_LEN: usize = 16;

impl GatewayControlConfig {
    /// Resolve the control-plane configuration from the process environment.
    ///
    /// Returns `Ok(None)` when [`GATEWAY_CONTROL_BIND_ENV`] is unset — the
    /// service is opt-in. When set, the admin token is mandatory, the bind
    /// address must be loopback unless
    /// [`GATEWAY_CONTROL_ALLOW_NON_LOOPBACK_ENV`] is `1`/`true`, and the
    /// route table defaults to the LunarWing base directory.
    pub fn from_env() -> Result<Option<Self>, GatewayControlError> {
        Self::resolve(
            std::env::var(GATEWAY_CONTROL_BIND_ENV).ok(),
            std::env::var(GATEWAY_CONTROL_ADMIN_TOKEN_ENV).ok(),
            std::env::var(GATEWAY_CONTROL_ROUTES_PATH_ENV).ok(),
            std::env::var(GATEWAY_CONTROL_ALLOW_NON_LOOPBACK_ENV).ok(),
        )
    }

    fn resolve(
        bind: Option<String>,
        admin_token: Option<String>,
        routes_path: Option<String>,
        allow_non_loopback: Option<String>,
    ) -> Result<Option<Self>, GatewayControlError> {
        let invalid = |reason: String| GatewayControlError::InvalidData { reason };
        let Some(bind) = bind else {
            return Ok(None);
        };
        let bind: SocketAddr = bind.trim().parse().map_err(|error| {
            invalid(format!(
                "{GATEWAY_CONTROL_BIND_ENV} '{bind}' is not a valid socket address: {error}"
            ))
        })?;
        let non_loopback_allowed =
            allow_non_loopback.is_some_and(|value| matches!(value.as_str(), "1" | "true"));
        if !bind.ip().is_loopback() && !non_loopback_allowed {
            return Err(invalid(format!(
                "{GATEWAY_CONTROL_BIND_ENV} '{bind}' is not loopback; set {GATEWAY_CONTROL_ALLOW_NON_LOOPBACK_ENV}=1 to expose the control plane"
            )));
        }
        let token = admin_token.ok_or_else(|| {
            invalid(format!(
                "{GATEWAY_CONTROL_ADMIN_TOKEN_ENV} is required when {GATEWAY_CONTROL_BIND_ENV} is set"
            ))
        })?;
        if token.len() < MIN_ADMIN_TOKEN_LEN {
            return Err(invalid(format!(
                "{GATEWAY_CONTROL_ADMIN_TOKEN_ENV} must be at least {MIN_ADMIN_TOKEN_LEN} characters"
            )));
        }
        let routes_path = routes_path
            .filter(|path| !path.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                crate::bootstrap::lunarwing_base_dir().join("mcp-gateway-routes.json")
            });
        Ok(Some(Self {
            bind,
            admin_token: SecretString::from(token),
            routes_path,
        }))
    }
}

struct GatewayState {
    store: RouteStore,
    admin_token: SecretString,
    client: reqwest::Client,
    proxy_permits: Arc<tokio::sync::Semaphore>,
    timeouts: UpstreamTimeouts,
}

/// The bundled gateway control plane. Build a [`Router`] with [`router`] for
/// embedding, or run a standalone listener with [`serve`].
pub struct GatewayControlPlane;

/// Build the control-plane + data-plane router (embedding-friendly).
pub fn router(config: GatewayControlConfig) -> Result<Router, GatewayControlError> {
    router_with_timeouts(config, UpstreamTimeouts::default())
}

/// Build the router with explicit upstream deadlines. Production callers use
/// [`router`]; tests pass short timeouts to exercise stall handling.
fn router_with_timeouts(
    config: GatewayControlConfig,
    timeouts: UpstreamTimeouts,
) -> Result<Router, GatewayControlError> {
    let store = RouteStore::load(config.routes_path)?;
    // No whole-request timeout: it would sever long-lived SSE responses.
    // Connect/time-to-headers is bounded explicitly at the call site and
    // `read_timeout` resets after every chunk, so only genuinely stalled
    // upstreams are cut; buffered bodies get their own overall deadline.
    let client = reqwest::Client::builder()
        .connect_timeout(timeouts.overall)
        .read_timeout(timeouts.read)
        .build()
        .map_err(|error| GatewayControlError::InvalidData {
            reason: format!("cannot build upstream HTTP client: {error}"),
        })?;
    let state = Arc::new(GatewayState {
        store,
        admin_token: config.admin_token,
        client,
        proxy_permits: Arc::new(tokio::sync::Semaphore::new(
            MAX_CONCURRENT_PROXY_CONNECTIONS,
        )),
        timeouts,
    });
    let admin = Router::new()
        .route("/admin/routes", get(list_routes))
        .route("/admin/routes/{name}", put(upsert_route))
        .route("/admin/routes/{name}", delete(remove_route))
        .route_layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            admin_auth,
        ));
    Ok(Router::new()
        .merge(admin)
        .route("/mcp", any(proxy_mcp))
        .with_state(state))
}

/// Run the bundled gateway control plane until `shutdown` resolves.
pub async fn serve(
    config: GatewayControlConfig,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), GatewayControlError> {
    let bind = config.bind;
    let router = router(config)?;
    let listener =
        tokio::net::TcpListener::bind(bind)
            .await
            .map_err(|error| GatewayControlError::Bind {
                addr: bind.to_string(),
                reason: error.to_string(),
            })?;
    tracing::info!(%bind, "MCP gateway control plane listening");
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(|error| GatewayControlError::Bind {
            addr: bind.to_string(),
            reason: format!("serve failed: {error}"),
        })
}

async fn admin_auth(
    State(state): State<Arc<GatewayState>>,
    request: Request,
    next: axum::middleware::Next,
) -> Response {
    let authorized = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| {
            token
                .as_bytes()
                .ct_eq(state.admin_token.expose_secret().as_bytes())
                .into()
        });
    if !authorized {
        return (StatusCode::UNAUTHORIZED, "admin bearer token required").into_response();
    }
    next.run(request).await
}

async fn list_routes(State(state): State<Arc<GatewayState>>) -> impl IntoResponse {
    Json(serde_json::json!({ "routes": state.store.list_redacted() }))
}

async fn upsert_route(
    State(state): State<Arc<GatewayState>>,
    Path(name): Path<String>,
    Json(route): Json<GatewayRoute>,
) -> Response {
    match state.store.upsert(&name, route) {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({ "upserted": name })),
        )
            .into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

async fn remove_route(
    State(state): State<Arc<GatewayState>>,
    Path(name): Path<String>,
) -> Response {
    match state.store.remove(&name) {
        Ok(true) => (StatusCode::OK, Json(serde_json::json!({ "removed": name }))).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "unknown route").into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

/// Headers a client may pass through to the upstream MCP server.
const FORWARDED_CLIENT_HEADERS: &[&str] = &["content-type", "accept", "mcp-session-id"];
/// MCP response headers the gateway may return to its client.
const FORWARDED_UPSTREAM_HEADERS: &[&str] = &["content-type", "mcp-session-id"];

async fn proxy_mcp(State(state): State<Arc<GatewayState>>, request: Request) -> Response {
    let route_name = request
        .headers()
        .get(GATEWAY_ROUTE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let Some(route_name) = route_name else {
        return (
            StatusCode::BAD_REQUEST,
            "missing X-LunarWing-MCP-Route header",
        )
            .into_response();
    };
    let Some(route) = state.store.get(&route_name) else {
        return (StatusCode::NOT_FOUND, "unknown route").into_response();
    };
    let method = request.method().clone();
    if method != axum::http::Method::POST
        && method != axum::http::Method::DELETE
        && method != axum::http::Method::GET
    {
        return (StatusCode::METHOD_NOT_ALLOWED, "unsupported MCP method").into_response();
    }

    // Bound concurrent ownership: every proxied connection holds a client
    // socket, an upstream socket, and (for SSE) long-lived stream tasks.
    // Fail fast on saturation rather than queueing unboundedly.
    let Ok(permit) = Arc::clone(&state.proxy_permits).try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "gateway busy: too many concurrent proxied connections",
        )
            .into_response();
    };

    let upstream_headers = match build_upstream_headers(&request, &route) {
        Ok(headers) => headers,
        Err(status) => return (status, "invalid route token").into_response(),
    };
    let body = match axum::body::to_bytes(request.into_body(), MAX_PROXY_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "request body too large").into_response(),
    };
    // Bounds connect and time-to-headers; streaming reads afterwards are
    // bounded per chunk by the client's read timeout.
    let upstream = tokio::time::timeout(
        state.timeouts.overall,
        state
            .client
            .request(method, &route.upstream)
            .headers(upstream_headers)
            .body(body)
            .send(),
    )
    .await;
    let response = match upstream {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            return (StatusCode::BAD_GATEWAY, format!("upstream error: {error}")).into_response();
        }
        Err(_) => {
            return (StatusCode::GATEWAY_TIMEOUT, "upstream response timed out").into_response();
        }
    };
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let headers = response.headers().clone();
    if is_event_stream(&headers) {
        streaming_reply(
            response,
            &route_name,
            &route.upstream,
            status,
            &headers,
            permit,
        )
    } else {
        // Buffered replies finish inside this handler, so the permit drops here.
        buffered_reply(
            response,
            &route_name,
            &route.upstream,
            status,
            &headers,
            state.timeouts.overall,
        )
        .await
    }
}

/// Forward only the MCP-relevant client headers; routing headers and any
/// client-supplied Authorization never cross the gateway boundary.
fn build_upstream_headers(
    request: &Request,
    route: &GatewayRoute,
) -> Result<HeaderMap, StatusCode> {
    let mut upstream_headers = HeaderMap::new();
    for name in FORWARDED_CLIENT_HEADERS {
        if let Some(value) = request.headers().get(*name) {
            upstream_headers.insert(*name, value.clone());
        }
    }
    if let Some(token) = &route.bearer_token {
        match HeaderValue::from_str(&format!("Bearer {token}")) {
            Ok(value) => {
                upstream_headers.insert(axum::http::header::AUTHORIZATION, value);
            }
            Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
        }
    }
    Ok(upstream_headers)
}

/// Whether the upstream response is an SSE stream (`text/event-stream`).
fn is_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
}

/// Build the downstream reply: upstream status plus allowlisted headers.
fn reply(status: StatusCode, headers: &HeaderMap, body: axum::body::Body) -> Response {
    let mut reply = Response::new(body);
    *reply.status_mut() = status;
    for name in FORWARDED_UPSTREAM_HEADERS {
        if let Some(value) = headers.get(*name) {
            reply.headers_mut().insert(*name, value.clone());
        }
    }
    reply
}

/// Forward an SSE response chunk-by-chunk. No cumulative byte cap applies:
/// chunks are relayed as they arrive so memory stays flat, and the semaphore
/// permit moves into the body so it releases on completion or disconnect.
fn streaming_reply(
    response: reqwest::Response,
    route_name: &str,
    upstream: &str,
    status: StatusCode,
    headers: &HeaderMap,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Response {
    use futures::StreamExt;

    struct StreamState {
        chunks: futures::stream::BoxStream<'static, Result<bytes::Bytes, reqwest::Error>>,
        transferred: u64,
        route_name: String,
        upstream: String,
        _permit: tokio::sync::OwnedSemaphorePermit,
    }

    let stream = futures::stream::try_unfold(
        StreamState {
            chunks: Box::pin(response.bytes_stream()),
            transferred: 0,
            route_name: route_name.to_string(),
            upstream: upstream.to_string(),
            _permit: permit,
        },
        |mut state| async move {
            match state.chunks.next().await {
                None => Ok::<_, std::io::Error>(None),
                Some(Ok(chunk)) => {
                    state.transferred = state.transferred.saturating_add(chunk.len() as u64);
                    Ok(Some((chunk, state)))
                }
                Some(Err(error)) => {
                    tracing::warn!(
                        route = %state.route_name,
                        upstream = %state.upstream,
                        transferred_bytes = state.transferred,
                        reason = %error,
                        "upstream event stream failed mid-body"
                    );
                    Err(std::io::Error::other(error))
                }
            }
        },
    );
    reply(status, headers, axum::body::Body::from_stream(stream))
}

/// Buffer a non-streaming upstream response under the body cap so a read
/// failure or oversized body still produces a clean `502` before any status
/// line is sent, instead of aborting an already-committed `200`.
async fn buffered_reply(
    response: reqwest::Response,
    route_name: &str,
    upstream: &str,
    status: StatusCode,
    headers: &HeaderMap,
    overall_timeout: Duration,
) -> Response {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PROXY_BODY_BYTES as u64)
    {
        return (StatusCode::BAD_GATEWAY, "upstream response too large").into_response();
    }
    let collected = tokio::time::timeout(overall_timeout, collect_capped(response)).await;
    let body = match collected {
        Ok(Ok(body)) => body,
        Ok(Err(reason)) => {
            tracing::warn!(route = %route_name, %upstream, %reason, "buffered upstream response failed");
            return (StatusCode::BAD_GATEWAY, reason).into_response();
        }
        Err(_) => {
            return (StatusCode::GATEWAY_TIMEOUT, "upstream response timed out").into_response();
        }
    };
    reply(status, headers, axum::body::Body::from(body))
}

/// Collect an upstream body, refusing to buffer past [`MAX_PROXY_BODY_BYTES`].
async fn collect_capped(response: reqwest::Response) -> Result<bytes::Bytes, &'static str> {
    use futures::StreamExt;

    let mut chunks = response.bytes_stream();
    let mut body = bytes::BytesMut::new();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|_| "upstream response read failed")?;
        if body.len().saturating_add(chunk.len()) > MAX_PROXY_BODY_BYTES {
            return Err("upstream response too large");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body.freeze())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::mcp::gateway_transport::GATEWAY_MODE_HEADER;

    fn temp_routes_path() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("gateway-routes.json");
        (dir, path)
    }

    fn route(upstream: &str) -> GatewayRoute {
        GatewayRoute {
            upstream: upstream.to_string(),
            bearer_token: Some("upstream-secret".to_string()),
            description: Some("test route".to_string()),
        }
    }

    #[test]
    fn env_resolution_is_opt_in_and_loopback_only() {
        // Unset bind address disables the service.
        assert!(
            GatewayControlConfig::resolve(None, None, None, None)
                .expect("no bind")
                .is_none()
        );

        // Valid loopback configuration resolves with the default routes path.
        let config = GatewayControlConfig::resolve(
            Some("127.0.0.1:9700".to_string()),
            Some("a-very-long-admin-token".to_string()),
            None,
            None,
        )
        .expect("loopback config")
        .expect("enabled");
        assert_eq!(config.bind.port(), 9700);
        assert!(config.routes_path.ends_with("mcp-gateway-routes.json"));

        // Admin token is mandatory and must be long enough.
        assert!(
            GatewayControlConfig::resolve(Some("127.0.0.1:9700".to_string()), None, None, None)
                .is_err()
        );
        assert!(
            GatewayControlConfig::resolve(
                Some("127.0.0.1:9700".to_string()),
                Some("short".to_string()),
                None,
                None,
            )
            .is_err()
        );

        // Non-loopback binds require the explicit override.
        assert!(
            GatewayControlConfig::resolve(
                Some("0.0.0.0:9700".to_string()),
                Some("a-very-long-admin-token".to_string()),
                None,
                None,
            )
            .is_err()
        );
        assert!(
            GatewayControlConfig::resolve(
                Some("0.0.0.0:9700".to_string()),
                Some("a-very-long-admin-token".to_string()),
                None,
                Some("1".to_string()),
            )
            .expect("override resolves")
            .is_some()
        );

        // Garbage bind addresses are rejected.
        assert!(
            GatewayControlConfig::resolve(
                Some("not an address".to_string()),
                Some("a-very-long-admin-token".to_string()),
                None,
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn route_validation_enforces_https_and_clean_fields() {
        route("https://mcp.example.com/mcp")
            .validate()
            .expect("https upstream");
        route("http://127.0.0.1:9000/mcp")
            .validate()
            .expect("loopback http allowed");
        assert!(route("http://203.0.113.5/mcp").validate().is_err());
        assert!(route("not a url").validate().is_err());

        let mut bad = route("https://mcp.example.com/mcp");
        bad.bearer_token = Some("has space".to_string());
        assert!(bad.validate().is_err());
    }

    #[test]
    fn route_store_persists_and_redacts() {
        let (_dir, path) = temp_routes_path();
        let store = RouteStore::load(path.clone()).expect("empty load");
        store
            .upsert("project-tools", route("https://mcp.example.com/mcp"))
            .expect("upsert");
        assert!(store.get("project-tools").is_some());

        // Redacted listing never exposes the token.
        let listed = store.list_redacted();
        let redacted = listed.get("project-tools").expect("listed");
        assert!(redacted.has_bearer_token);
        let json = serde_json::to_string(&listed).expect("serialize listing");
        assert!(!json.contains("upstream-secret"));

        // Persistence roundtrip.
        let reloaded = RouteStore::load(path).expect("reload");
        let route = reloaded.get("project-tools").expect("route survives");
        assert_eq!(route.upstream, "https://mcp.example.com/mcp");
        assert_eq!(route.bearer_token.as_deref(), Some("upstream-secret"));

        assert!(reloaded.remove("project-tools").expect("remove"));
        assert!(!reloaded.remove("project-tools").expect("second remove"));
        assert!(reloaded.get("project-tools").is_none());
    }

    #[test]
    fn route_store_rejects_bad_names_and_caps_table() {
        let (_dir, path) = temp_routes_path();
        let store = RouteStore::load(path).expect("load");
        assert!(
            store
                .upsert(" bad-name", route("https://mcp.example.com/mcp"))
                .is_err()
        );
        assert!(
            store
                .upsert("bad\r\nname", route("https://mcp.example.com/mcp"))
                .is_err()
        );
        assert!(
            store
                .upsert(&"x".repeat(129), route("https://mcp.example.com/mcp"))
                .is_err()
        );
        assert!(store.upsert("ok", route("http://203.0.113.5/mcp")).is_err());
    }

    async fn spawn_upstream() -> String {
        async fn upstream_handler(request: Request) -> impl IntoResponse {
            // Echo back the headers the gateway forwarded, plus a fixed
            // JSON-RPC result so the client-visible shape is realistic.
            let headers: BTreeMap<String, String> = request
                .headers()
                .iter()
                .filter_map(|(name, value)| {
                    value
                        .to_str()
                        .ok()
                        .map(|value| (name.to_string(), value.to_string()))
                })
                .collect();
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": { "headers": headers },
            });
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                body.to_string(),
            )
        }
        let app = Router::new().route("/mcp", any(upstream_handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind upstream");
        let url = format!("http://{}/mcp", listener.local_addr().expect("addr"));
        tokio::spawn(async move { axum::serve(listener, app).await.expect("serve upstream") });
        url
    }

    #[derive(Clone)]
    struct StreamingUpstreamState {
        started: Arc<tokio::sync::Notify>,
        continue_signal: Arc<tokio::sync::Notify>,
    }

    async fn streaming_upstream_handler(State(state): State<StreamingUpstreamState>) -> Response {
        use futures::StreamExt;
        use std::convert::Infallible;

        state.started.notify_one();
        let continue_signal = Arc::clone(&state.continue_signal);
        let stream = futures::stream::once(async {
            Ok::<_, Infallible>(bytes::Bytes::from_static(b"data: first\n\n"))
        })
        .chain(futures::stream::once(async move {
            continue_signal.notified().await;
            Ok::<_, Infallible>(bytes::Bytes::from_static(b"data: second\n\n"))
        }));
        Response::builder()
            .status(StatusCode::OK)
            .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
            .header("mcp-session-id", "stream-session")
            .body(axum::body::Body::from_stream(stream))
            .expect("streaming response")
    }

    async fn spawn_streaming_upstream()
    -> (String, Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let started = Arc::new(tokio::sync::Notify::new());
        let continue_signal = Arc::new(tokio::sync::Notify::new());
        let app = Router::new()
            .route("/mcp", any(streaming_upstream_handler))
            .with_state(StreamingUpstreamState {
                started: Arc::clone(&started),
                continue_signal: Arc::clone(&continue_signal),
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind streaming upstream");
        let url = format!("http://{}/mcp", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve streaming upstream");
        });
        (url, started, continue_signal)
    }

    async fn spawn_control_plane(routes_path: PathBuf) -> String {
        spawn_control_plane_with_timeouts(routes_path, UpstreamTimeouts::default()).await
    }

    async fn spawn_control_plane_with_timeouts(
        routes_path: PathBuf,
        timeouts: UpstreamTimeouts,
    ) -> String {
        let router = router_with_timeouts(
            GatewayControlConfig {
                bind: "127.0.0.1:0".parse().expect("bind addr"),
                admin_token: SecretString::from("admin-token"),
                routes_path,
            },
            timeouts,
        )
        .expect("build router");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind control plane");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move { axum::serve(listener, router).await.expect("serve cp") });
        base
    }

    #[tokio::test]
    async fn admin_api_requires_bearer_and_crud_routes() {
        let upstream = spawn_upstream().await;
        let (_dir, path) = temp_routes_path();
        let base = spawn_control_plane(path).await;
        let client = reqwest::Client::new();

        // No token / wrong token are rejected.
        let unauthenticated = client
            .get(format!("{base}/admin/routes"))
            .send()
            .await
            .expect("request");
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
        let wrong = client
            .get(format!("{base}/admin/routes"))
            .bearer_auth("wrong")
            .send()
            .await
            .expect("request");
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

        // Upsert and list.
        let upsert = client
            .put(format!("{base}/admin/routes/project-tools"))
            .bearer_auth("admin-token")
            .json(&route(&upstream))
            .send()
            .await
            .expect("upsert");
        assert_eq!(upsert.status(), StatusCode::OK);
        let listed: serde_json::Value = client
            .get(format!("{base}/admin/routes"))
            .bearer_auth("admin-token")
            .send()
            .await
            .expect("list")
            .json()
            .await
            .expect("json");
        assert_eq!(
            listed["routes"]["project-tools"]["upstream"],
            serde_json::Value::String(upstream.clone())
        );
        assert_eq!(
            listed["routes"]["project-tools"]["has_bearer_token"],
            serde_json::Value::Bool(true)
        );
        assert!(
            listed["routes"]["project-tools"]
                .get("bearer_token")
                .is_none()
        );

        // Delete.
        let removed = client
            .delete(format!("{base}/admin/routes/project-tools"))
            .bearer_auth("admin-token")
            .send()
            .await
            .expect("delete");
        assert_eq!(removed.status(), StatusCode::OK);
        let missing = client
            .delete(format!("{base}/admin/routes/project-tools"))
            .bearer_auth("admin-token")
            .send()
            .await
            .expect("second delete");
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn data_plane_routes_and_strips_managed_headers() {
        let upstream = spawn_upstream().await;
        let (_dir, path) = temp_routes_path();
        let base = spawn_control_plane(path).await;
        let client = reqwest::Client::new();
        client
            .put(format!("{base}/admin/routes/project-tools"))
            .bearer_auth("admin-token")
            .json(&route(&upstream))
            .send()
            .await
            .expect("upsert route");

        // Missing route header -> 400; unknown route -> 404.
        let missing = client
            .post(format!("{base}/mcp"))
            .body("{}")
            .send()
            .await
            .expect("request");
        assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
        let unknown = client
            .post(format!("{base}/mcp"))
            .header(GATEWAY_ROUTE_HEADER, "nope")
            .body("{}")
            .send()
            .await
            .expect("request");
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

        // A proxied call: routing headers are stripped upstream, the route's
        // bearer token is injected, and a spoofed client Authorization dies.
        let response = client
            .post(format!("{base}/mcp"))
            .header(GATEWAY_ROUTE_HEADER, "project-tools")
            .header(GATEWAY_MODE_HEADER, "shared")
            .header("content-type", "application/json")
            .header("mcp-session-id", "session-1")
            .header("authorization", "Bearer attacker-token")
            .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
            .send()
            .await
            .expect("proxy request");
        assert_eq!(response.status(), StatusCode::OK);
        let echoed: serde_json::Value = response.json().await.expect("echo json");
        let headers = echoed["result"]["headers"].as_object().expect("headers");
        assert_eq!(
            headers.get("authorization").and_then(|v| v.as_str()),
            Some("Bearer upstream-secret")
        );
        assert_eq!(
            headers.get("mcp-session-id").and_then(|v| v.as_str()),
            Some("session-1")
        );
        assert!(!headers.contains_key("x-lunarwing-mcp-route"));
        assert!(!headers.contains_key("x-lunarwing-mcp-gateway-mode"));
    }

    #[tokio::test]
    async fn data_plane_streams_sse_and_forwards_response_headers() {
        use futures::StreamExt;

        let (upstream, started, continue_signal) = spawn_streaming_upstream().await;
        let (_dir, path) = temp_routes_path();
        let base = spawn_control_plane(path).await;
        let client = reqwest::Client::new();
        client
            .put(format!("{base}/admin/routes/streaming"))
            .bearer_auth("admin-token")
            .json(&route(&upstream))
            .send()
            .await
            .expect("upsert route");

        // The upstream waits after its first chunk. A buffered gateway would
        // therefore never return response headers to this client.
        let response = tokio::time::timeout(
            Duration::from_secs(2),
            client
                .get(format!("{base}/mcp"))
                .header(GATEWAY_ROUTE_HEADER, "streaming")
                .send(),
        )
        .await
        .expect("streaming response headers")
        .expect("streaming request");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        assert_eq!(
            response
                .headers()
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok()),
            Some("stream-session")
        );
        started.notified().await;

        let mut body = response.bytes_stream();
        let first = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .expect("first stream chunk")
            .expect("first chunk exists")
            .expect("first chunk reads");
        assert_eq!(first, bytes::Bytes::from_static(b"data: first\n\n"));

        continue_signal.notify_one();
        let second = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .expect("second stream chunk")
            .expect("second chunk exists")
            .expect("second chunk reads");
        assert_eq!(second, bytes::Bytes::from_static(b"data: second\n\n"));
    }

    /// SSE upstream that streams `chunks` events of `chunk_len` payload bytes
    /// each, crossing the buffered body cap in aggregate.
    async fn spawn_large_streaming_upstream(chunks: usize, chunk_len: usize) -> String {
        use std::convert::Infallible;

        let events: Vec<Result<bytes::Bytes, Infallible>> = (0..chunks)
            .map(|_| {
                let mut event = Vec::with_capacity(chunk_len + 8);
                event.extend_from_slice(b"data: ");
                event.extend_from_slice(&vec![b'a'; chunk_len]);
                event.extend_from_slice(b"\n\n");
                Ok(bytes::Bytes::from(event))
            })
            .collect();
        let app = Router::new().route(
            "/mcp",
            any(|| async move {
                Response::builder()
                    .status(StatusCode::OK)
                    .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
                    .body(axum::body::Body::from_stream(futures::stream::iter(events)))
                    .expect("large streaming response")
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind large streaming upstream");
        let url = format!("http://{}/mcp", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve large streaming upstream");
        });
        url
    }

    #[tokio::test]
    async fn data_plane_sse_streams_past_buffered_body_cap() {
        use futures::StreamExt;

        // Nine 512 KiB events ≈ 4.6 MiB total: beyond MAX_PROXY_BODY_BYTES.
        // A cumulative lifetime cap would sever this healthy stream.
        let upstream = spawn_large_streaming_upstream(9, 512 * 1024).await;
        let (_dir, path) = temp_routes_path();
        let base = spawn_control_plane(path).await;
        let client = reqwest::Client::new();
        client
            .put(format!("{base}/admin/routes/large-stream"))
            .bearer_auth("admin-token")
            .json(&route(&upstream))
            .send()
            .await
            .expect("upsert route");

        let response = client
            .get(format!("{base}/mcp"))
            .header(GATEWAY_ROUTE_HEADER, "large-stream")
            .send()
            .await
            .expect("streaming request");
        assert_eq!(response.status(), StatusCode::OK);

        let mut received = 0_u64;
        let mut body = response.bytes_stream();
        while let Some(chunk) = tokio::time::timeout(Duration::from_secs(10), body.next())
            .await
            .expect("stream chunk timed out")
        {
            received += chunk.expect("chunk reads").len() as u64;
        }
        assert!(received > MAX_PROXY_BODY_BYTES as u64);
    }

    #[tokio::test]
    async fn data_plane_post_sse_streams_without_method_gate() {
        use futures::StreamExt;

        let (upstream, started, continue_signal) = spawn_streaming_upstream().await;
        let (_dir, path) = temp_routes_path();
        let base = spawn_control_plane(path).await;
        let client = reqwest::Client::new();
        client
            .put(format!("{base}/admin/routes/post-stream"))
            .bearer_auth("admin-token")
            .json(&route(&upstream))
            .send()
            .await
            .expect("upsert route");

        // MCP Streamable HTTP permits text/event-stream responses to POST;
        // the upstream waits between chunks, so a buffered gateway would
        // never deliver the first event here.
        let response = tokio::time::timeout(
            Duration::from_secs(2),
            client
                .post(format!("{base}/mcp"))
                .header(GATEWAY_ROUTE_HEADER, "post-stream")
                .header("content-type", "application/json")
                .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
                .send(),
        )
        .await
        .expect("POST streaming response headers")
        .expect("POST streaming request");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        started.notified().await;

        let mut body = response.bytes_stream();
        let first = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .expect("first POST stream chunk")
            .expect("first chunk exists")
            .expect("first chunk reads");
        assert_eq!(first, bytes::Bytes::from_static(b"data: first\n\n"));
        continue_signal.notify_one();
        let second = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .expect("second POST stream chunk")
            .expect("second chunk exists")
            .expect("second chunk reads");
        assert_eq!(second, bytes::Bytes::from_static(b"data: second\n\n"));
    }

    #[tokio::test]
    async fn data_plane_buffered_oversized_json_is_clean_502() {
        // Declared length over the cap: rejected before any status is sent.
        let oversized = "x".repeat(MAX_PROXY_BODY_BYTES + 1);
        let app = Router::new().route(
            "/mcp",
            any(move || {
                let body = oversized.clone();
                async move {
                    Response::builder()
                        .status(StatusCode::OK)
                        .header(axum::http::header::CONTENT_TYPE, "application/json")
                        .body(body)
                        .expect("oversized response")
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind oversized upstream");
        let upstream = format!("http://{}/mcp", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve oversized upstream");
        });

        let (_dir, path) = temp_routes_path();
        let base = spawn_control_plane(path).await;
        let client = reqwest::Client::new();
        client
            .put(format!("{base}/admin/routes/oversized"))
            .bearer_auth("admin-token")
            .json(&route(&upstream))
            .send()
            .await
            .expect("upsert route");

        let response = client
            .post(format!("{base}/mcp"))
            .header(GATEWAY_ROUTE_HEADER, "oversized")
            .body("{}")
            .send()
            .await
            .expect("oversized request");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(
            response
                .text()
                .await
                .expect("error body")
                .contains("upstream response too large")
        );
    }

    #[tokio::test]
    async fn data_plane_buffered_chunked_overflow_is_clean_502() {
        use std::convert::Infallible;

        // No declared length (chunked): the cap must fire mid-collection.
        let chunk = bytes::Bytes::from(vec![b'x'; 512 * 1024]);
        let events: Vec<Result<bytes::Bytes, Infallible>> =
            (0..9).map(|_| Ok(chunk.clone())).collect();
        let app = Router::new().route(
            "/mcp",
            any(|| async move {
                Response::builder()
                    .status(StatusCode::OK)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from_stream(futures::stream::iter(events)))
                    .expect("chunked oversized response")
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind chunked oversized upstream");
        let upstream = format!("http://{}/mcp", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve chunked oversized upstream");
        });

        let (_dir, path) = temp_routes_path();
        let base = spawn_control_plane(path).await;
        let client = reqwest::Client::new();
        client
            .put(format!("{base}/admin/routes/chunked-oversized"))
            .bearer_auth("admin-token")
            .json(&route(&upstream))
            .send()
            .await
            .expect("upsert route");

        let response = client
            .post(format!("{base}/mcp"))
            .header(GATEWAY_ROUTE_HEADER, "chunked-oversized")
            .body("{}")
            .send()
            .await
            .expect("chunked oversized request");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(
            response
                .text()
                .await
                .expect("error body")
                .contains("upstream response too large")
        );
    }

    #[tokio::test]
    async fn data_plane_buffered_mid_body_failure_is_clean_502() {
        use futures::StreamExt;

        // Upstream sends JSON headers and one chunk, then fails the body.
        let app = Router::new().route(
            "/mcp",
            any(|| async {
                let stream = futures::stream::once(async {
                    Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"{\"partial\":"))
                })
                .chain(futures::stream::once(async {
                    Err(std::io::Error::other("upstream died mid-body"))
                }));
                Response::builder()
                    .status(StatusCode::OK)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from_stream(stream))
                    .expect("failing response")
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind failing upstream");
        let upstream = format!("http://{}/mcp", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve failing upstream");
        });

        let (_dir, path) = temp_routes_path();
        let base = spawn_control_plane(path).await;
        let client = reqwest::Client::new();
        client
            .put(format!("{base}/admin/routes/failing"))
            .bearer_auth("admin-token")
            .json(&route(&upstream))
            .send()
            .await
            .expect("upsert route");

        let response = client
            .post(format!("{base}/mcp"))
            .header(GATEWAY_ROUTE_HEADER, "failing")
            .body("{}")
            .send()
            .await
            .expect("failing request");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    /// SSE upstream that answers headers and one event, then never produces
    /// again: each connection holds a gateway permit until dropped.
    async fn spawn_hanging_upstream() -> String {
        use futures::StreamExt;
        use std::convert::Infallible;

        let app = Router::new().route(
            "/mcp",
            any(|| async {
                let stream = futures::stream::once(async {
                    Ok::<_, Infallible>(bytes::Bytes::from_static(b"data: held\n\n"))
                })
                .chain(futures::stream::pending());
                Response::builder()
                    .status(StatusCode::OK)
                    .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
                    .body(axum::body::Body::from_stream(stream))
                    .expect("hanging response")
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind hanging upstream");
        let url = format!("http://{}/mcp", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve hanging upstream");
        });
        url
    }

    #[tokio::test]
    async fn data_plane_bounds_concurrent_streams_and_reclaims_permits() {
        let hanging = spawn_hanging_upstream().await;
        let echo = spawn_upstream().await;
        let (_dir, path) = temp_routes_path();
        let base = spawn_control_plane(path).await;
        let client = reqwest::Client::new();
        for (name, upstream) in [("hang", &hanging), ("echo", &echo)] {
            client
                .put(format!("{base}/admin/routes/{name}"))
                .bearer_auth("admin-token")
                .json(&route(upstream))
                .send()
                .await
                .expect("upsert route");
        }

        // Saturate every permit with long-lived SSE connections.
        let (holding_tx, mut holding_rx) =
            tokio::sync::mpsc::channel(MAX_CONCURRENT_PROXY_CONNECTIONS);
        let release = Arc::new(tokio::sync::Notify::new());
        let mut holders = Vec::new();
        for _ in 0..MAX_CONCURRENT_PROXY_CONNECTIONS {
            let client = client.clone();
            let base = base.clone();
            let holding_tx = holding_tx.clone();
            let release = Arc::clone(&release);
            holders.push(tokio::spawn(async move {
                let response = client
                    .get(format!("{base}/mcp"))
                    .header(GATEWAY_ROUTE_HEADER, "hang")
                    .send()
                    .await
                    .expect("held stream headers");
                assert_eq!(response.status(), StatusCode::OK);
                holding_tx.send(()).await.expect("holding signal");
                release.notified().await;
                drop(response);
            }));
        }
        for _ in 0..MAX_CONCURRENT_PROXY_CONNECTIONS {
            tokio::time::timeout(Duration::from_secs(10), holding_rx.recv())
                .await
                .expect("all streams holding")
                .expect("holding channel open");
        }

        // One more proxied connection must fail fast instead of queueing.
        let saturated = client
            .post(format!("{base}/mcp"))
            .header(GATEWAY_ROUTE_HEADER, "echo")
            .body("{}")
            .send()
            .await
            .expect("saturated request");
        assert_eq!(saturated.status(), StatusCode::SERVICE_UNAVAILABLE);

        // Dropping the streams releases their permits for new connections.
        release.notify_waiters();
        for holder in holders {
            holder.await.expect("holder task");
        }
        let mut reclaimed = None;
        for _ in 0..200 {
            let attempt = client
                .post(format!("{base}/mcp"))
                .header(GATEWAY_ROUTE_HEADER, "echo")
                .header("content-type", "application/json")
                .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
                .send()
                .await
                .expect("reclamation request");
            if attempt.status() == StatusCode::OK {
                reclaimed = Some(());
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(reclaimed.is_some(), "permits were not reclaimed");
    }

    #[tokio::test]
    async fn data_plane_stalled_upstream_is_cut_by_read_timeout() {
        use futures::StreamExt;

        // Upstream answers headers and one event, then goes silent forever.
        let hanging = spawn_hanging_upstream().await;
        let (_dir, path) = temp_routes_path();
        let base = spawn_control_plane_with_timeouts(
            path,
            UpstreamTimeouts {
                overall: Duration::from_secs(5),
                read: Duration::from_millis(200),
            },
        )
        .await;
        let client = reqwest::Client::new();
        client
            .put(format!("{base}/admin/routes/hang"))
            .bearer_auth("admin-token")
            .json(&route(&hanging))
            .send()
            .await
            .expect("upsert route");

        let response = client
            .get(format!("{base}/mcp"))
            .header(GATEWAY_ROUTE_HEADER, "hang")
            .send()
            .await
            .expect("stalled stream request");
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.bytes_stream();
        let first = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .expect("first chunk")
            .expect("first chunk exists")
            .expect("first chunk reads");
        assert_eq!(first, bytes::Bytes::from_static(b"data: held\n\n"));

        // The upstream never produces again: the per-read timeout must sever
        // the stream promptly instead of pinning it for the 30 s default.
        let stalled = tokio::time::timeout(Duration::from_secs(5), body.next())
            .await
            .expect("read timeout must terminate the stalled stream");
        assert!(
            !matches!(stalled, Some(Ok(_))),
            "stalled upstream yielded more data instead of being cut"
        );
    }

    #[tokio::test]
    async fn data_plane_downstream_disconnect_cancels_upstream_stream() {
        use futures::StreamExt;
        use std::convert::Infallible;

        // Notified when the upstream response body is dropped server-side,
        // which only happens once the gateway closes the upstream connection.
        struct DropGuard(Arc<tokio::sync::Notify>);
        impl Drop for DropGuard {
            fn drop(&mut self) {
                self.0.notify_one();
            }
        }

        let dropped = Arc::new(tokio::sync::Notify::new());
        let guard_notify = Arc::clone(&dropped);
        let app = Router::new().route(
            "/mcp",
            any(move || {
                let guard_notify = Arc::clone(&guard_notify);
                async move {
                    let guard = DropGuard(guard_notify);
                    let stream = futures::stream::unfold(guard, |guard| async move {
                        tokio::time::sleep(Duration::from_millis(25)).await;
                        let heartbeat = Ok::<_, Infallible>(bytes::Bytes::from_static(b": hb\n\n"));
                        Some((heartbeat, guard))
                    });
                    Response::builder()
                        .status(StatusCode::OK)
                        .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
                        .body(axum::body::Body::from_stream(stream))
                        .expect("heartbeat response")
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind heartbeat upstream");
        let upstream = format!("http://{}/mcp", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve heartbeat upstream");
        });

        let (_dir, path) = temp_routes_path();
        let base = spawn_control_plane(path).await;
        let client = reqwest::Client::new();
        client
            .put(format!("{base}/admin/routes/heartbeat"))
            .bearer_auth("admin-token")
            .json(&route(&upstream))
            .send()
            .await
            .expect("upsert route");

        let response = client
            .get(format!("{base}/mcp"))
            .header(GATEWAY_ROUTE_HEADER, "heartbeat")
            .send()
            .await
            .expect("heartbeat stream request");
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.bytes_stream();
        tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .expect("first heartbeat")
            .expect("first heartbeat exists")
            .expect("first heartbeat reads");

        // Dropping the incomplete body stream cannot be reused by the pool,
        // so the downstream connection closes; the gateway must then drop the
        // upstream body, closing the upstream connection and the stream.
        drop(body);
        tokio::time::timeout(Duration::from_secs(5), dropped.notified())
            .await
            .expect("upstream stream was not cancelled after downstream disconnect");
    }

    #[tokio::test]
    async fn data_plane_strips_non_allowlisted_upstream_response_headers() {
        use std::convert::Infallible;

        // Buffered JSON path: allowlisted headers pass, everything else dies.
        let app = Router::new().route(
            "/mcp",
            any(|| async {
                Response::builder()
                    .status(StatusCode::OK)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .header("mcp-session-id", "allowlisted-session")
                    .header("set-cookie", "session=upstream-tracking")
                    .header("x-upstream-internal", "internal-only")
                    .header("www-authenticate", "Bearer realm=upstream")
                    .body("{}".to_string())
                    .expect("json response with junk headers")
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind junk-header upstream");
        let json_upstream = format!("http://{}/mcp", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve junk-header upstream");
        });

        // Streaming SSE path: the same allowlist applies to streamed replies.
        let sse_app = Router::new().route(
            "/mcp",
            any(|| async {
                let stream = futures::stream::once(async {
                    Ok::<_, Infallible>(bytes::Bytes::from_static(b"data: one\n\n"))
                });
                Response::builder()
                    .status(StatusCode::OK)
                    .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
                    .header("mcp-session-id", "allowlisted-stream-session")
                    .header("set-cookie", "session=stream-tracking")
                    .header("x-upstream-internal", "stream-internal-only")
                    .body(axum::body::Body::from_stream(stream))
                    .expect("sse response with junk headers")
            }),
        );
        let sse_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind junk-header sse upstream");
        let sse_upstream = format!("http://{}/mcp", sse_listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            axum::serve(sse_listener, sse_app)
                .await
                .expect("serve junk-header sse upstream");
        });

        let (_dir, path) = temp_routes_path();
        let base = spawn_control_plane(path).await;
        let client = reqwest::Client::new();
        for (name, upstream) in [("json-junk", &json_upstream), ("sse-junk", &sse_upstream)] {
            client
                .put(format!("{base}/admin/routes/{name}"))
                .bearer_auth("admin-token")
                .json(&route(upstream))
                .send()
                .await
                .expect("upsert route");
        }

        let assert_allowlist = |headers: &HeaderMap, session: &str| {
            assert_eq!(
                headers.get("mcp-session-id").and_then(|v| v.to_str().ok()),
                Some(session)
            );
            assert!(headers.get("set-cookie").is_none());
            assert!(headers.get("x-upstream-internal").is_none());
            assert!(headers.get("www-authenticate").is_none());
        };

        let buffered = client
            .post(format!("{base}/mcp"))
            .header(GATEWAY_ROUTE_HEADER, "json-junk")
            .body("{}")
            .send()
            .await
            .expect("buffered junk-header request");
        assert_eq!(buffered.status(), StatusCode::OK);
        assert_eq!(
            buffered
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        assert_allowlist(buffered.headers(), "allowlisted-session");

        let streamed = client
            .get(format!("{base}/mcp"))
            .header(GATEWAY_ROUTE_HEADER, "sse-junk")
            .send()
            .await
            .expect("streaming junk-header request");
        assert_eq!(streamed.status(), StatusCode::OK);
        assert_eq!(
            streamed
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );
        assert_allowlist(streamed.headers(), "allowlisted-stream-session");
    }
}
