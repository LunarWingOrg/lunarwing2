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
//!   server with bounded bodies and timeouts.
//!
//! Security invariants:
//! - Admin endpoints require a constant-time-compared bearer token.
//! - Client-supplied `X-LunarWing-*` routing headers never reach upstreams.
//! - Upstreams must be HTTPS (loopback HTTP allowed for development).
//! - Request and response bodies are capped; upstream calls time out.

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
/// Maximum proxied request or upstream response body (4 MiB).
pub const MAX_PROXY_BODY_BYTES: usize = 4 * 1024 * 1024;
/// Default upstream request timeout.
const DEFAULT_UPSTREAM_TIMEOUT: Duration = Duration::from_secs(30);

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
    request_client: reqwest::Client,
    stream_client: reqwest::Client,
}

/// The bundled gateway control plane. Build a [`Router`] with [`router`] for
/// embedding, or run a standalone listener with [`serve`].
pub struct GatewayControlPlane;

/// Build the control-plane + data-plane router (embedding-friendly).
pub fn router(config: GatewayControlConfig) -> Result<Router, GatewayControlError> {
    let store = RouteStore::load(config.routes_path)?;
    let request_client = reqwest::Client::builder()
        .timeout(DEFAULT_UPSTREAM_TIMEOUT)
        .build()
        .map_err(|error| GatewayControlError::InvalidData {
            reason: format!("cannot build upstream HTTP client: {error}"),
        })?;
    let stream_client = reqwest::Client::builder()
        .connect_timeout(DEFAULT_UPSTREAM_TIMEOUT)
        .build()
        .map_err(|error| GatewayControlError::InvalidData {
            reason: format!("cannot build upstream streaming HTTP client: {error}"),
        })?;
    let state = Arc::new(GatewayState {
        store,
        admin_token: config.admin_token,
        request_client,
        stream_client,
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

    // Forward only the MCP-relevant client headers; routing headers and any
    // client-supplied Authorization never cross the gateway boundary.
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
            Err(_) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, "invalid route token").into_response();
            }
        }
    }

    let body = match axum::body::to_bytes(request.into_body(), MAX_PROXY_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "request body too large").into_response(),
    };
    let client = if method == axum::http::Method::GET {
        &state.stream_client
    } else {
        &state.request_client
    };
    let upstream = tokio::time::timeout(
        DEFAULT_UPSTREAM_TIMEOUT,
        client
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
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PROXY_BODY_BYTES as u64)
    {
        return (StatusCode::BAD_GATEWAY, "upstream response too large").into_response();
    }
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let headers = response.headers().clone();
    let mut reply = Response::new(bounded_response_body(response));
    *reply.status_mut() = status;
    for name in FORWARDED_UPSTREAM_HEADERS {
        if let Some(value) = headers.get(*name) {
            reply.headers_mut().insert(*name, value.clone());
        }
    }
    reply
}

fn bounded_response_body(response: reqwest::Response) -> axum::body::Body {
    use futures::StreamExt;

    let stream = futures::stream::try_unfold(
        (response.bytes_stream(), 0_usize),
        |(mut stream, total)| async move {
            let Some(chunk) = stream.next().await else {
                return Ok::<_, std::io::Error>(None);
            };
            let chunk = chunk.map_err(std::io::Error::other)?;
            let total = total
                .checked_add(chunk.len())
                .ok_or_else(|| std::io::Error::other("upstream response too large"))?;
            if total > MAX_PROXY_BODY_BYTES {
                return Err(std::io::Error::other("upstream response too large"));
            }
            Ok(Some((chunk, (stream, total))))
        },
    );
    axum::body::Body::from_stream(stream)
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

    async fn spawn_control_plane(routes_path: PathBuf) -> String {
        let router = router(GatewayControlConfig {
            bind: "127.0.0.1:0".parse().expect("bind addr"),
            admin_token: SecretString::from("admin-token"),
            routes_path,
        })
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
}
