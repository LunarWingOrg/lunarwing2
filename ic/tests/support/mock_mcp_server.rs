//! Mock MCP server for E2E testing of the extension lifecycle.
//!
//! Provides a minimal HTTP server with:
//! - OAuth 2.1 discovery (`.well-known/oauth-protected-resource`, `.well-known/oauth-authorization-server`)
//! - Dynamic Client Registration (`/register`)
//! - Token exchange (`/token`)
//! - MCP JSON-RPC endpoint (`/mcp`) with `initialize`, `tools/list`, `tools/call`
//!
//! Tool call responses are pre-configured via `MockToolResponse`.

#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

/// The synthetic bearer token accepted by the mock MCP server.
pub const MOCK_MCP_TOKEN: &str = "mock-access-token";

/// A pre-configured response for a specific MCP tool call.
#[derive(Clone, Debug)]
pub struct MockToolResponse {
    /// Tool name (e.g., "notion-search").
    pub name: String,
    /// JSON response content for `tools/call`.
    pub content: serde_json::Value,
}

/// Observable call counts and auth toggle shared between the server and test.
///
/// Test-only: never exported by production code.
#[derive(Debug, Default)]
pub struct MockMcpObservations {
    initialize_count: AtomicUsize,
    tools_list_count: AtomicUsize,
    discovery_count: AtomicUsize,
    registration_count: AtomicUsize,
    token_count: AtomicUsize,
    /// Per-tool-name call counter.
    tool_call_counts: std::sync::Mutex<HashMap<String, usize>>,
    calls: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
    registered_redirect_uris: std::sync::Mutex<Vec<String>>,
    last_authorization: std::sync::Mutex<Option<String>>,
    token_outside_authorization: AtomicBool,
    /// When true, the server rejects requests without a valid bearer token.
    /// Defaults to `true` (auth required), matching the original behavior.
    require_token: AtomicBool,
}

impl MockMcpObservations {
    /// Start with auth required (backward compatible).
    pub fn new() -> Self {
        Self {
            require_token: AtomicBool::new(true),
            ..Default::default()
        }
    }

    pub fn initialize_count(&self) -> usize {
        self.initialize_count.load(Ordering::SeqCst)
    }

    pub fn tools_list_count(&self) -> usize {
        self.tools_list_count.load(Ordering::SeqCst)
    }

    pub fn tool_call_count(&self, name: &str) -> usize {
        self.tool_call_counts
            .lock()
            .expect("tool_call_counts mutex")
            .get(name)
            .copied()
            .unwrap_or(0)
    }

    pub fn call_count(&self) -> usize {
        self.tool_call_counts
            .lock()
            .expect("tool_call_counts mutex")
            .values()
            .sum()
    }

    pub fn calls(&self) -> Vec<(String, serde_json::Value)> {
        self.calls.lock().expect("calls mutex").clone()
    }

    pub fn discovery_count(&self) -> usize {
        self.discovery_count.load(Ordering::SeqCst)
    }

    pub fn registration_count(&self) -> usize {
        self.registration_count.load(Ordering::SeqCst)
    }

    pub fn token_count(&self) -> usize {
        self.token_count.load(Ordering::SeqCst)
    }

    pub fn registered_redirect_uris(&self) -> Vec<String> {
        self.registered_redirect_uris
            .lock()
            .expect("registered_redirect_uris mutex")
            .clone()
    }

    pub fn last_authorization(&self) -> Option<String> {
        self.last_authorization
            .lock()
            .expect("last_authorization mutex")
            .clone()
    }

    pub fn saw_token_in_non_authorization_field(&self) -> bool {
        self.token_outside_authorization.load(Ordering::SeqCst)
    }

    /// Toggle whether the server enforces bearer-token authentication.
    pub fn set_require_token(&self, required: bool) {
        self.require_token.store(required, Ordering::SeqCst);
    }

    pub fn require_token(&self) -> bool {
        self.require_token.load(Ordering::SeqCst)
    }
}

/// A running mock MCP server.
pub struct MockMcpServer {
    /// Base URL including port (e.g., "http://127.0.0.1:12345").
    pub base_url: String,
    /// Shutdown signal sender.
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// Server task handle.
    handle: Option<tokio::task::JoinHandle<()>>,
    /// Shared observable state for test assertions.
    observations: Arc<MockMcpObservations>,
}

impl MockMcpServer {
    /// The MCP endpoint URL for use in registry entries.
    pub fn mcp_url(&self) -> String {
        format!("{}/mcp", self.base_url)
    }

    /// Number of `initialize` JSON-RPC calls received.
    pub fn initialize_count(&self) -> usize {
        self.observations.initialize_count()
    }

    /// Number of `tools/list` JSON-RPC calls received.
    pub fn tools_list_count(&self) -> usize {
        self.observations.tools_list_count()
    }

    /// Number of `tools/call` requests received for `name`.
    pub fn tool_call_count(&self, name: &str) -> usize {
        self.observations.tool_call_count(name)
    }

    pub fn call_count(&self) -> usize {
        self.observations.call_count()
    }

    pub fn calls(&self) -> Vec<(String, serde_json::Value)> {
        self.observations.calls()
    }

    pub fn discovery_count(&self) -> usize {
        self.observations.discovery_count()
    }

    pub fn registration_count(&self) -> usize {
        self.observations.registration_count()
    }

    pub fn token_count(&self) -> usize {
        self.observations.token_count()
    }

    pub fn registered_redirect_uris(&self) -> Vec<String> {
        self.observations.registered_redirect_uris()
    }

    pub fn last_authorization(&self) -> Option<String> {
        self.observations.last_authorization()
    }

    pub fn saw_token_in_non_authorization_field(&self) -> bool {
        self.observations.saw_token_in_non_authorization_field()
    }

    /// Toggle whether the server requires a valid bearer token.
    pub fn require_token(&self, required: bool) {
        self.observations.set_require_token(required);
    }

    /// Shut down the server.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.handle.take() {
            let _ = h.await;
        }
    }
}

impl Drop for MockMcpServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
}

/// Shared state for the mock server handlers.
struct MockState {
    /// Base URL (filled after bind).
    base_url: String,
    /// Tool definitions served by tools/list.
    tools: Vec<McpToolDef>,
    /// Pre-configured tool call responses keyed by tool name.
    /// Multiple calls to the same tool return responses in order.
    tool_responses: HashMap<String, Vec<serde_json::Value>>,
    /// Counter for tool_responses consumption (per tool name).
    tool_response_idx: std::sync::Mutex<HashMap<String, usize>>,
    /// Observable state for test assertions.
    observations: Arc<MockMcpObservations>,
}

#[derive(Clone, Serialize)]
struct McpToolDef {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: serde_json::Value,
}

/// Start a mock MCP server on a random port.
///
/// `tool_responses` configures what `tools/call` returns for each tool name.
/// Multiple responses for the same tool are returned in order.
pub async fn start_mock_mcp_server(tool_responses: Vec<MockToolResponse>) -> MockMcpServer {
    // Build tool definitions and response map.
    let mut tools = Vec::new();
    let mut response_map: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    let mut seen_tools = std::collections::HashSet::new();

    for tr in &tool_responses {
        if seen_tools.insert(tr.name.clone()) {
            tools.push(McpToolDef {
                name: tr.name.clone(),
                description: format!("Mock tool: {}", tr.name),
                input_schema: serde_json::json!({"type": "object", "properties": {}}),
            });
        }
        response_map
            .entry(tr.name.clone())
            .or_default()
            .push(tr.content.clone());
    }

    // Bind to a random port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind mock MCP server");
    let addr: SocketAddr = listener.local_addr().expect("no local addr");
    let base_url = format!("http://127.0.0.1:{}", addr.port());

    let observations = Arc::new(MockMcpObservations::new());

    let state = Arc::new(MockState {
        base_url: base_url.clone(),
        tools,
        tool_responses: response_map,
        tool_response_idx: std::sync::Mutex::new(HashMap::new()),
        observations: Arc::clone(&observations),
    });

    let app = Router::new()
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(handle_protected_resource),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(handle_auth_server_metadata),
        )
        .route("/register", post(handle_register))
        .route("/authorize", get(handle_authorize))
        .route("/token", post(handle_token))
        .route("/mcp", post(handle_mcp))
        .with_state(state);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .expect("mock MCP server failed");
    });

    // Wait briefly for the server to start accepting.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    MockMcpServer {
        base_url,
        shutdown_tx: Some(shutdown_tx),
        handle: Some(handle),
        observations,
    }
}

// ── OAuth discovery endpoints ───────────────────────────────────────────

async fn handle_protected_resource(State(state): State<Arc<MockState>>) -> impl IntoResponse {
    state
        .observations
        .discovery_count
        .fetch_add(1, Ordering::SeqCst);
    Json(serde_json::json!({
        "resource": format!("{}/mcp", state.base_url),
        "authorization_servers": [state.base_url],
        "scopes_supported": ["read", "write"]
    }))
}

async fn handle_auth_server_metadata(State(state): State<Arc<MockState>>) -> impl IntoResponse {
    Json(serde_json::json!({
        "issuer": state.base_url,
        "authorization_endpoint": format!("{}/authorize", state.base_url),
        "token_endpoint": format!("{}/token", state.base_url),
        "registration_endpoint": format!("{}/register", state.base_url),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code"],
        "code_challenge_methods_supported": ["S256"],
        "scopes_supported": ["read", "write"]
    }))
}

// ── OAuth DCR ───────────────────────────────────────────────────────────

async fn handle_register(
    State(state): State<Arc<MockState>>,
    Json(request): Json<serde_json::Value>,
) -> impl IntoResponse {
    state
        .observations
        .registration_count
        .fetch_add(1, Ordering::SeqCst);
    let redirect_uris = request
        .get("redirect_uris")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(String::from)
        .collect::<Vec<_>>();
    *state
        .observations
        .registered_redirect_uris
        .lock()
        .expect("registered_redirect_uris mutex") = redirect_uris.clone();
    Json(serde_json::json!({
        "client_id": "mock-client-id",
        "client_name": "lunarwing-test",
        "redirect_uris": redirect_uris,
        "grant_types": ["authorization_code"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none"
    }))
}

// ── OAuth authorize (auto-approve) ──────────────────────────────────────

/// In a real flow, this would show a consent screen. For testing, we just
/// need the endpoint to exist. The test will bypass OAuth by injecting
/// tokens directly.
async fn handle_authorize() -> impl IntoResponse {
    // Return a simple HTML page; in practice the test injects tokens directly.
    axum::response::Html(
        "<html><body>Mock OAuth: authorize endpoint. Tests bypass this.</body></html>",
    )
}

// ── OAuth token exchange ────────────────────────────────────────────────

async fn handle_token(
    State(state): State<Arc<MockState>>,
    Form(request): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    state
        .observations
        .token_count
        .fetch_add(1, Ordering::SeqCst);
    if request.values().any(|value| value.contains(MOCK_MCP_TOKEN)) {
        state
            .observations
            .token_outside_authorization
            .store(true, Ordering::SeqCst);
    }
    Json(serde_json::json!({
        "access_token": MOCK_MCP_TOKEN,
        "token_type": "Bearer",
        "expires_in": 3600,
        "refresh_token": "mock-refresh-token"
    }))
}

// ── MCP JSON-RPC endpoint ───────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<serde_json::Value>,
    method: String,
    #[serde(default)]
    params: Option<serde_json::Value>,
}

async fn handle_mcp(
    State(state): State<Arc<MockState>>,
    headers: HeaderMap,
    Json(req): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(String::from);
    *state
        .observations
        .last_authorization
        .lock()
        .expect("last_authorization mutex") = authorization;
    if serde_json::to_string(&req).is_ok_and(|body| body.contains(MOCK_MCP_TOKEN)) {
        state
            .observations
            .token_outside_authorization
            .store(true, Ordering::SeqCst);
    }

    // When auth is required, check for a valid bearer token.
    if state.observations.require_token() {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if !auth.starts_with("Bearer ") || &auth[7..] != MOCK_MCP_TOKEN {
            // Return 401 with WWW-Authenticate header per MCP OAuth spec.
            let www_auth = format!(
                "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\"",
                state.base_url
            );
            return (
                StatusCode::UNAUTHORIZED,
                [("www-authenticate", www_auth.as_str())],
                Json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": req.id,
                    "error": {"code": -32000, "message": "Unauthorized"}
                })),
            )
                .into_response();
        }
    }

    // Handle notifications (no id) silently.
    if req.id.is_none() {
        return StatusCode::OK.into_response();
    }

    // Track observable counts for JSON-RPC requests with IDs.
    match req.method.as_str() {
        "initialize" => {
            state
                .observations
                .initialize_count
                .fetch_add(1, Ordering::SeqCst);
        }
        "tools/list" => {
            state
                .observations
                .tools_list_count
                .fetch_add(1, Ordering::SeqCst);
        }
        _ => {}
    }

    let response = match req.method.as_str() {
        "initialize" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": req.id,
            "result": {
                "protocolVersion": "2024-11-05",
                "serverInfo": {
                    "name": "mock-mcp-server",
                    "version": "1.0.0"
                },
                "capabilities": {
                    "tools": {}
                }
            }
        }),
        "tools/list" => {
            let tools: Vec<serde_json::Value> = state
                .tools
                .iter()
                .map(|t| serde_json::to_value(t).unwrap())
                .collect();
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": req.id,
                "result": {
                    "tools": tools
                }
            })
        }
        "tools/call" => {
            let tool_name = req
                .params
                .as_ref()
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("unknown");

            // Record call count.
            {
                let mut counts = state.observations.tool_call_counts.lock().expect("counts");
                *counts.entry(tool_name.to_string()).or_insert(0) += 1;
            }
            let arguments = req
                .params
                .as_ref()
                .and_then(|params| params.get("arguments"))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            state
                .observations
                .calls
                .lock()
                .expect("calls mutex")
                .push((tool_name.to_string(), arguments));

            let content = {
                let mut idx_map = state.tool_response_idx.lock().unwrap();
                let idx = idx_map.entry(tool_name.to_string()).or_insert(0);
                let responses = state.tool_responses.get(tool_name);
                let result = responses
                    .and_then(|r| r.get(*idx))
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({"error": "no mock response configured"}));
                *idx += 1;
                result
            };

            serde_json::json!({
                "jsonrpc": "2.0",
                "id": req.id,
                "result": {
                    "content": [
                        {
                            "type": "text",
                            "text": serde_json::to_string(&content).unwrap_or_default()
                        }
                    ]
                }
            })
        }
        _ => serde_json::json!({
            "jsonrpc": "2.0",
            "id": req.id,
            "error": {"code": -32601, "message": format!("Method not found: {}", req.method)}
        }),
    };

    Json(response).into_response()
}
