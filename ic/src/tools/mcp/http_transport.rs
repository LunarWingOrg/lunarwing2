//! HTTP transport for MCP servers.
//!
//! Implements the Streamable HTTP transport, communicating with MCP servers
//! over HTTP POST with JSON and SSE response support.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use tokio::sync::Notify;

use crate::tools::mcp::protocol::{McpRequest, McpResponse};
use crate::tools::mcp::session::McpSessionManager;
use crate::tools::mcp::transport::McpTransport;
use crate::tools::tool::ToolError;

/// MCP transport that communicates with a server over HTTP.
///
/// Sends JSON-RPC requests as HTTP POST with `Content-Type: application/json`
/// and accepts either JSON or SSE (`text/event-stream`) responses. Optionally
/// manages session IDs via [`McpSessionManager`] and supports custom headers.
pub struct HttpMcpTransport {
    server_url: String,
    server_name: String,
    http_client: reqwest::Client,
    session_manager: Option<Arc<McpSessionManager>>,
    custom_headers: HashMap<String, String>,
    closed: AtomicBool,
    in_flight: AtomicUsize,
    drained: Notify,
    shutdown_result: tokio::sync::OnceCell<Result<(), ToolError>>,
}

impl HttpMcpTransport {
    /// Create a new HTTP transport for the given server URL.
    pub fn new(server_url: impl Into<String>, server_name: impl Into<String>) -> Self {
        Self {
            server_url: server_url.into(),
            server_name: server_name.into(),
            // reqwest::Client::builder().build() only fails if the TLS backend
            // cannot initialize, which does not happen with the default rustls
            // feature set. Panic is acceptable here (same as reqwest's own
            // `Client::new()`).
            http_client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Failed to create HTTP client"), // safety: TLS init with default rustls cannot fail
            session_manager: None,
            custom_headers: HashMap::new(),
            closed: AtomicBool::new(false),
            in_flight: AtomicUsize::new(0),
            drained: Notify::new(),
            shutdown_result: tokio::sync::OnceCell::new(),
        }
    }

    /// Attach a session manager for Mcp-Session-Id tracking.
    pub fn with_session_manager(mut self, session_manager: Arc<McpSessionManager>) -> Self {
        self.session_manager = Some(session_manager);
        self
    }

    /// Set custom headers that will be sent with every request.
    #[cfg(test)]
    pub fn with_custom_headers(mut self, headers: HashMap<String, String>) -> Self {
        self.custom_headers = headers;
        self
    }

    /// Get the server URL.
    #[cfg(test)]
    pub(crate) fn server_url(&self) -> &str {
        &self.server_url
    }

    /// Get the session manager, if one is configured.
    #[cfg(test)]
    pub(crate) fn session_manager(&self) -> Option<&Arc<McpSessionManager>> {
        self.session_manager.as_ref()
    }
}

#[async_trait]
impl McpTransport for HttpMcpTransport {
    async fn send(
        &self,
        request: &McpRequest,
        headers: &HashMap<String, String>,
    ) -> Result<McpResponse, ToolError> {
        let _request_guard = self.begin_request()?;
        // Build the HTTP request.
        let mut req_builder = self
            .http_client
            .post(&self.server_url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .json(request);

        // Apply custom headers configured on the transport.
        for (key, value) in &self.custom_headers {
            req_builder = req_builder.header(key.as_str(), value.as_str());
        }

        // Apply per-request headers (e.g. Authorization, Mcp-Session-Id).
        for (key, value) in headers {
            req_builder = req_builder.header(key.as_str(), value.as_str());
        }

        // Send the request.
        let response = req_builder.send().await.map_err(|e| {
            let mut chain = format!("[{}] MCP HTTP request failed: {}", self.server_name, e);
            let mut source = std::error::Error::source(&e);
            while let Some(cause) = source {
                chain.push_str(&format!(" -> {}", cause));
                source = cause.source();
            }
            ToolError::ExternalService(chain)
        })?;

        // Extract session ID from response headers before consuming the body.
        if let Some(ref session_manager) = self.session_manager
            && let Some(session_id) = response
                .headers()
                .get("Mcp-Session-Id")
                .and_then(|v| v.to_str().ok())
        {
            session_manager
                .update_session_id(&self.server_name, Some(session_id.to_string()))
                .await;
        }

        // Handle error status codes.
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let sanitized = sanitize_error_body(&body);
            return Err(ToolError::ExternalService(format!(
                "[{}] MCP server returned status: {} - {}",
                self.server_name, status, sanitized
            )));
        }

        // MCP notifications commonly acknowledge with 202 Accepted and no body.
        if response.status() == reqwest::StatusCode::ACCEPTED {
            return Ok(McpResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: None,
                error: None,
            });
        }

        // Determine response format from Content-Type.
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        if content_type.contains("text/event-stream") {
            self.parse_sse_response(response, request.id).await
        } else {
            response.json().await.map_err(|e| {
                ToolError::ExternalService(format!(
                    "[{}] Failed to parse MCP response: {}",
                    self.server_name, e
                ))
            })
        }
    }

    async fn shutdown(&self) -> Result<(), ToolError> {
        self.shutdown_session(&HashMap::new()).await
    }

    async fn shutdown_with_headers(
        &self,
        headers: &HashMap<String, String>,
    ) -> Result<(), ToolError> {
        self.shutdown_session(headers).await
    }

    fn supports_http_features(&self) -> bool {
        true
    }
}

impl HttpMcpTransport {
    fn begin_request(&self) -> Result<InFlightRequest<'_>, ToolError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(self.closed_error());
        }
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        let guard = InFlightRequest { transport: self };
        if self.closed.load(Ordering::Acquire) {
            drop(guard);
            return Err(self.closed_error());
        }
        Ok(guard)
    }

    fn closed_error(&self) -> ToolError {
        ToolError::ExternalService(format!("[{}] MCP transport is shut down", self.server_name))
    }

    async fn wait_until_drained(&self) {
        loop {
            let notified = self.drained.notified();
            if self.in_flight.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }

    async fn shutdown_session(&self, headers: &HashMap<String, String>) -> Result<(), ToolError> {
        self.shutdown_result
            .get_or_init(|| self.perform_shutdown(headers))
            .await
            .clone()
    }

    async fn perform_shutdown(&self, headers: &HashMap<String, String>) -> Result<(), ToolError> {
        self.closed.store(true, Ordering::Release);
        self.wait_until_drained().await;
        let Some(session_manager) = self.session_manager.as_ref() else {
            return Ok(());
        };
        let Some(session_id) = session_manager.get_session_id(&self.server_name).await else {
            return Ok(());
        };

        let mut request = self.http_client.delete(&self.server_url);
        for (key, value) in &self.custom_headers {
            if !key.eq_ignore_ascii_case("mcp-session-id") {
                request = request.header(key.as_str(), value.as_str());
            }
        }
        for (key, value) in headers {
            if !key.eq_ignore_ascii_case("mcp-session-id") {
                request = request.header(key.as_str(), value.as_str());
            }
        }
        let response = request
            .header("Mcp-Session-Id", session_id)
            .send()
            .await
            .map_err(|error| {
                ToolError::ExternalService(format!(
                    "[{}] Failed to terminate MCP HTTP session: {}",
                    self.server_name, error
                ))
            })?;
        if response.status().is_success()
            || response.status() == reqwest::StatusCode::NOT_FOUND
            || response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED
        {
            return Ok(());
        }

        let status = response.status();
        let body = sanitize_error_body(&response.text().await.unwrap_or_default());
        Err(ToolError::ExternalService(format!(
            "[{}] MCP session termination returned status: {} - {}",
            self.server_name, status, body
        )))
    }

    /// Parse a Server-Sent Events response, returning the JSON-RPC response
    /// whose `id` matches `request_id`. Non-matching events (e.g. server
    /// notifications or progress updates) are skipped so that the caller
    /// receives the actual result for its request.
    async fn parse_sse_response(
        &self,
        response: reqwest::Response,
        request_id: Option<u64>,
    ) -> Result<McpResponse, ToolError> {
        use futures::StreamExt;

        const MAX_SSE_BUFFER: usize = 10 * 1024 * 1024; // 10 MB

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                ToolError::ExternalService(format!(
                    "[{}] Failed to read SSE chunk: {}",
                    self.server_name, e
                ))
            })?;

            buffer.push_str(&String::from_utf8_lossy(&chunk));

            if buffer.len() > MAX_SSE_BUFFER {
                return Err(ToolError::ExternalService(format!(
                    "[{}] SSE response exceeded {} byte limit",
                    self.server_name, MAX_SSE_BUFFER
                )));
            }

            // Process only complete lines (terminated by \n). The last
            // element of split('\n') may be an incomplete line; keep it
            // in the buffer for the next chunk.
            let mut remaining_start = 0;
            let bytes = buffer.as_bytes();
            for (i, &b) in bytes.iter().enumerate() {
                if b == b'\n' {
                    let line = &buffer[remaining_start..i];
                    remaining_start = i + 1;

                    if let Some(json_str) = line.strip_prefix("data: ")
                        && let Ok(resp) = serde_json::from_str::<McpResponse>(json_str)
                        && resp.id == request_id
                    {
                        return Ok(resp);
                    }
                }
            }
            // Keep only the unprocessed trailing fragment without allocating
            // a new String each iteration.
            if remaining_start > 0 {
                buffer.drain(..remaining_start);
            }
        }

        // Process any remaining data without a trailing newline.
        if let Some(json_str) = buffer.strip_prefix("data: ")
            && let Ok(resp) = serde_json::from_str::<McpResponse>(json_str.trim())
            && resp.id == request_id
        {
            return Ok(resp);
        }

        Err(ToolError::ExternalService(format!(
            "[{}] No matching response (id={:?}) in SSE stream",
            self.server_name, request_id
        )))
    }
}

struct InFlightRequest<'a> {
    transport: &'a HttpMcpTransport,
}

impl Drop for InFlightRequest<'_> {
    fn drop(&mut self) {
        if self.transport.in_flight.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.transport.drained.notify_waiters();
        }
    }
}

/// Sanitize an HTTP error body for safe inclusion in error messages.
///
/// When the body looks like a full HTML document (`<html` or `<!doctype`),
/// strips all tags, collapsing whitespace.  Non-HTML bodies are left
/// intact.  In both cases the result is truncated to 200 *characters*
/// (char-boundary safe) so that large payloads don't bloat error messages.
///
/// See #263 — raw HTML error pages were propagating through the error
/// chain into the web UI, causing a white screen.
pub(crate) fn sanitize_error_body(body: &str) -> String {
    const MAX_CHARS: usize = 200;

    // Only strip tags when the body looks like a full HTML document.
    // Plain text that happens to contain `<` / `>` (e.g. log lines,
    // comparison expressions) is left untouched.
    let lower = body.to_ascii_lowercase();
    let is_html_document = lower.contains("<html") || lower.contains("<!doctype");

    let text = if is_html_document {
        let stripped = body
            .chars()
            .fold((String::new(), false), |(mut out, in_tag), c| {
                if c == '<' {
                    (out, true)
                } else if c == '>' {
                    (out, false)
                } else if !in_tag {
                    out.push(c);
                    (out, false)
                } else {
                    (out, true)
                }
            })
            .0;
        stripped.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        body.to_string()
    };

    // Truncate at a char boundary (safe for multi-byte UTF-8).
    if text.chars().count() > MAX_CHARS {
        let byte_offset = text
            .char_indices()
            .nth(MAX_CHARS)
            .map(|(i, _)| i)
            .unwrap_or(text.len());
        format!("{}... ({} bytes total)", &text[..byte_offset], body.len())
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_error_body_strips_html_tags() {
        let html =
            r#"<!DOCTYPE html><html><body><h1>422 Error</h1><p>Invalid token</p></body></html>"#;
        let result = sanitize_error_body(html);
        assert!(!result.contains('<'), "HTML tags must be stripped");
        assert!(!result.contains('>'), "HTML tags must be stripped");
        assert!(result.contains("422 Error"));
        assert!(result.contains("Invalid token"));
    }

    #[test]
    fn test_sanitize_error_body_truncates_large_html_page() {
        let html = format!(
            "<html><body><p>{}</p></body></html>",
            "error detail ".repeat(50)
        );
        let result = sanitize_error_body(&html);
        assert!(result.contains("..."));
        assert!(result.contains("bytes total)"));
        assert!(!result.contains('<'));
    }

    #[test]
    fn test_sanitize_error_body_passes_short_plain_text() {
        assert_eq!(sanitize_error_body("Not Found"), "Not Found");
    }

    #[test]
    fn test_sanitize_error_body_truncates_long_plain_text() {
        let long = "x".repeat(300);
        let result = sanitize_error_body(&long);
        assert!(result.contains("..."));
        assert!(result.contains("300 bytes total)"));
    }

    #[test]
    fn test_sanitize_error_body_multibyte_no_panic() {
        // 300 CJK characters = 900 bytes; truncation must land on a
        // char boundary, not in the middle of a multi-byte sequence.
        let cjk = "错误".repeat(150);
        let result = sanitize_error_body(&cjk);
        assert!(result.contains("..."));
        // Must be valid UTF-8 (would have panicked otherwise).
        assert!(result.is_char_boundary(result.len()));
    }

    #[test]
    fn test_sanitize_error_body_strips_uppercase_html() {
        let html = "<HTML><BODY><H1>500 Internal Server Error</H1></BODY></HTML>";
        let result = sanitize_error_body(html);
        assert!(
            !result.contains('<'),
            "uppercase HTML tags must be stripped"
        );
        assert!(result.contains("500 Internal Server Error"));
    }

    #[test]
    fn test_sanitize_error_body_preserves_angle_brackets_in_non_html() {
        let text = "value < 10 and value > 0";
        assert_eq!(sanitize_error_body(text), text);
    }

    #[test]
    fn test_sanitize_error_body_empty_string() {
        assert_eq!(sanitize_error_body(""), "");
    }

    #[test]
    fn test_new_creates_transport() {
        let transport = HttpMcpTransport::new("http://localhost:8080", "test");
        assert_eq!(transport.server_url(), "http://localhost:8080");
        assert!(transport.session_manager().is_none());
        assert!(transport.custom_headers.is_empty());
    }

    #[test]
    fn test_supports_http_features() {
        let http_transport = HttpMcpTransport::new("http://localhost:8080", "test");
        assert!(http_transport.supports_http_features());
    }

    #[test]
    fn test_with_session_manager() {
        let session_manager = Arc::new(McpSessionManager::new());
        let transport = HttpMcpTransport::new("http://localhost:8080", "test")
            .with_session_manager(session_manager.clone());
        assert!(transport.session_manager().is_some());
    }

    #[test]
    fn test_with_custom_headers() {
        let mut headers = HashMap::new();
        headers.insert("X-Custom".to_string(), "value".to_string());
        let transport =
            HttpMcpTransport::new("http://localhost:8080", "test").with_custom_headers(headers);
        assert_eq!(transport.custom_headers.get("X-Custom").unwrap(), "value");
    }

    // -- Wire-level echo server tests -----------------------------------------
    //
    // These tests spin up a real HTTP server that echoes received headers back
    // as a JSON-RPC result, verifying that custom headers and Authorization
    // handling work end-to-end through the actual HTTP transport.

    /// Spawn a lightweight echo server that returns received headers as a
    /// JSON-RPC response.  Returns `(url, join_handle)`.
    async fn spawn_echo_server() -> (String, tokio::task::JoinHandle<()>) {
        use axum::{Router, extract::Request, routing::post};
        use tokio::net::TcpListener;

        async fn echo_headers(req: Request) -> axum::response::Json<serde_json::Value> {
            let mut map = serde_json::Map::new();
            for (name, value) in req.headers() {
                if let Ok(v) = value.to_str() {
                    map.insert(name.to_string(), serde_json::Value::String(v.to_string()));
                }
            }
            axum::response::Json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": map,
            }))
        }

        let app = Router::new().route("/", post(echo_headers));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://127.0.0.1:{}", addr.port());

        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        (url, handle)
    }

    #[tokio::test]
    async fn test_wire_custom_headers_sent() {
        let (url, _handle) = spawn_echo_server().await;

        let custom = HashMap::from([
            ("X-Api-Key".to_string(), "secret-key".to_string()),
            ("X-Org-Id".to_string(), "org-123".to_string()),
        ]);
        let transport = HttpMcpTransport::new(&url, "echo-test").with_custom_headers(custom);

        let request = McpRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(1),
            method: "initialize".to_string(),
            params: Some(serde_json::json!({})),
        };
        let per_request_headers = HashMap::new();
        let response = transport
            .send(&request, &per_request_headers)
            .await
            .unwrap();

        let echoed = response.result.unwrap();
        assert_eq!(echoed["x-api-key"], "secret-key");
        assert_eq!(echoed["x-org-id"], "org-123");
    }

    #[tokio::test]
    async fn test_wire_per_request_headers_override_custom() {
        let (url, _handle) = spawn_echo_server().await;

        let custom = HashMap::from([(
            "authorization".to_string(),
            "Bearer custom-token".to_string(),
        )]);
        let transport = HttpMcpTransport::new(&url, "echo-test").with_custom_headers(custom);

        // Per-request header should override the custom header
        let per_request = HashMap::from([(
            "authorization".to_string(),
            "Bearer oauth-token".to_string(),
        )]);
        let request = McpRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(1),
            method: "initialize".to_string(),
            params: Some(serde_json::json!({})),
        };
        let response = transport.send(&request, &per_request).await.unwrap();

        let echoed = response.result.unwrap();
        // Per-request headers are inserted after custom headers via HeaderMap::insert,
        // which replaces any existing entry for the same key.
        assert_eq!(echoed["authorization"], "Bearer oauth-token");
    }

    #[tokio::test]
    async fn test_wire_custom_auth_preserved_when_no_per_request_auth() {
        let (url, _handle) = spawn_echo_server().await;

        let custom = HashMap::from([(
            "authorization".to_string(),
            "Bearer custom-token".to_string(),
        )]);
        let transport = HttpMcpTransport::new(&url, "echo-test").with_custom_headers(custom);

        let per_request = HashMap::new(); // no per-request auth
        let request = McpRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(1),
            method: "initialize".to_string(),
            params: Some(serde_json::json!({})),
        };
        let response = transport.send(&request, &per_request).await.unwrap();

        let echoed = response.result.unwrap();
        assert_eq!(echoed["authorization"], "Bearer custom-token");
    }

    async fn spawn_accepted_server() -> (String, tokio::task::JoinHandle<()>) {
        use axum::{Router, routing::post};
        use tokio::net::TcpListener;

        async fn accepted() -> axum::http::StatusCode {
            axum::http::StatusCode::ACCEPTED
        }

        let app = Router::new().route("/", post(accepted));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind to an ephemeral port");
        let addr = listener
            .local_addr()
            .expect("Failed to get listener's local address");
        let url = format!("http://127.0.0.1:{}", addr.port());

        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("Test server failed to run");
        });

        (url, handle)
    }

    fn notification_request(method: &str) -> McpRequest {
        McpRequest {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: method.to_string(),
            params: None,
        }
    }

    #[tokio::test]
    async fn test_accepted_notification_returns_empty_response() {
        let (url, _handle) = spawn_accepted_server().await;
        let transport = HttpMcpTransport::new(&url, "accepted-test");
        let request = notification_request("notifications/initialized");

        let response = transport
            .send(&request, &HashMap::new())
            .await
            .expect("202 notification response");
        assert_eq!(response.jsonrpc, "2.0");
        assert_eq!(response.id, request.id);
        assert!(response.result.is_none());
        assert!(response.error.is_none());
    }

    #[tokio::test]
    async fn test_client_shutdown_terminates_http_session_and_closes_stale_clones() {
        use axum::http::{HeaderMap, StatusCode};
        use axum::{Router, routing::delete};
        use tokio::net::TcpListener;

        async fn terminate(headers: HeaderMap) -> StatusCode {
            assert_eq!(headers.get_all("mcp-session-id").iter().count(), 1);
            assert_eq!(
                headers
                    .get("mcp-session-id")
                    .and_then(|value| value.to_str().ok()),
                Some("session-123")
            );
            assert_eq!(
                headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok()),
                Some("Bearer shutdown-token")
            );
            StatusCode::NO_CONTENT
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/", delete(terminate)))
                .await
                .unwrap();
        });
        let url = format!("http://{address}");
        let sessions = Arc::new(McpSessionManager::new());
        sessions.get_or_create("shutdown-http", &url).await;
        sessions
            .update_session_id("shutdown-http", Some("session-123".to_string()))
            .await;
        let transport = Arc::new(
            HttpMcpTransport::new(&url, "shutdown-http")
                .with_session_manager(Arc::clone(&sessions)),
        );
        let mut config = crate::tools::mcp::McpServerConfig::new("shutdown-http", &url);
        config.headers.insert(
            "Authorization".to_string(),
            "Bearer shutdown-token".to_string(),
        );
        let client = crate::tools::mcp::McpClient::new_with_transport(
            "shutdown-http",
            transport,
            Some(Arc::clone(&sessions)),
            None,
            "test",
            Some(config),
        );
        let stale_clone = client.clone();

        client.shutdown().await.expect("shutdown HTTP client");

        assert!(sessions.active_servers().await.is_empty());
        let error = stale_clone
            .test_connection()
            .await
            .expect_err("stale clone must stay closed");
        assert!(error.to_string().contains("shut down"));
        server.abort();
    }

    #[tokio::test]
    async fn test_shutdown_drains_in_flight_request_before_session_delete() {
        use axum::extract::State;
        use axum::http::StatusCode;
        use axum::{Json, Router, routing::post};
        use tokio::net::TcpListener;

        struct TestState {
            started: tokio::sync::Notify,
            release: tokio::sync::Notify,
            post_completed: AtomicBool,
            delete_count: AtomicUsize,
        }

        async fn delayed_post(State(state): State<Arc<TestState>>) -> Json<serde_json::Value> {
            state.started.notify_one();
            state.release.notified().await;
            state.post_completed.store(true, Ordering::Release);
            Json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"tools": []}
            }))
        }

        async fn terminate(State(state): State<Arc<TestState>>) -> StatusCode {
            assert!(state.post_completed.load(Ordering::Acquire));
            state.delete_count.fetch_add(1, Ordering::AcqRel);
            StatusCode::NO_CONTENT
        }

        let state = Arc::new(TestState {
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
            post_completed: AtomicBool::new(false),
            delete_count: AtomicUsize::new(0),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/", post(delayed_post).delete(terminate))
            .with_state(Arc::clone(&state));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let url = format!("http://{address}");
        let sessions = Arc::new(McpSessionManager::new());
        sessions.get_or_create("drain-http", &url).await;
        sessions
            .update_session_id("drain-http", Some("drain-session".to_string()))
            .await;
        let transport = Arc::new(
            HttpMcpTransport::new(&url, "drain-http").with_session_manager(Arc::clone(&sessions)),
        );
        let started = state.started.notified();
        let request_transport = Arc::clone(&transport);
        let request = tokio::spawn(async move {
            request_transport
                .send(&McpRequest::list_tools(1), &HashMap::new())
                .await
        });
        started.await;

        let first_shutdown_transport = Arc::clone(&transport);
        let first_shutdown = tokio::spawn(async move { first_shutdown_transport.shutdown().await });
        let second_shutdown_transport = Arc::clone(&transport);
        let second_shutdown =
            tokio::spawn(async move { second_shutdown_transport.shutdown().await });
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert_eq!(state.delete_count.load(Ordering::Acquire), 0);

        state.release.notify_one();
        request.await.unwrap().expect("in-flight request completes");
        first_shutdown.await.unwrap().expect("shutdown completes");
        second_shutdown.await.unwrap().expect("shutdown completes");
        assert_eq!(state.delete_count.load(Ordering::Acquire), 1);
        server.abort();
    }
}
