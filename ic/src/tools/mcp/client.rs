//! MCP client for connecting to MCP servers.
//!
//! Supports both local (unauthenticated) and hosted (OAuth-authenticated) servers.
//! Uses pluggable transports (HTTP, stdio, Unix) via the `McpTransport` trait.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::context::JobContext;
use crate::secrets::SecretsStore;
use crate::tools::mcp::auth::refresh_access_token;
use crate::tools::mcp::config::McpServerConfig;
use crate::tools::mcp::http_transport::HttpMcpTransport;
use crate::tools::mcp::protocol::{
    CallToolResult, InitializeResult, ListToolsResult, McpRequest, McpResponse, McpTool,
    SUPPORTED_PROTOCOL_VERSIONS,
};
use crate::tools::mcp::session::McpSessionManager;
use crate::tools::mcp::transport::McpTransport;
use crate::tools::tool::{ApprovalRequirement, Tool, ToolError, ToolOutput};

pub(crate) const MAX_WORKER_MCP_TOOLS: usize = 256;
pub(crate) const MAX_WORKER_MCP_TOOL_BYTES: usize = 2 * 1024 * 1024;

/// Discovery bounds prevent malicious servers from extending pagination indefinitely.
const MAX_TOOLS_LIST_PAGES: usize = 100;
const MAX_TOOLS_LIST_COUNT: usize = 4_096;
const MAX_TOOLS_LIST_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOOLS_LIST_DURATION: Duration = Duration::from_secs(30);

/// MCP client for communicating with MCP servers.
///
/// Supports multiple transport types:
/// - HTTP: For remote MCP servers (created via `new`, `new_with_name`, `new_authenticated`)
/// - Stdio/Unix: Via `new_with_transport` with a custom `McpTransport` implementation
pub struct McpClient {
    /// Transport for sending requests.
    transport: Arc<dyn McpTransport>,

    /// Server URL (kept for accessor compatibility).
    server_url: String,

    /// Server name (for logging and session management).
    server_name: String,

    /// Request ID counter.
    next_id: AtomicU64,

    /// Cached tools.
    tools_cache: RwLock<Option<(u64, Vec<McpTool>)>>,

    /// Session manager (shared across clients).
    session_manager: Option<Arc<McpSessionManager>>,

    /// Secrets store for retrieving access tokens.
    secrets: Option<Arc<dyn SecretsStore + Send + Sync>>,

    /// User ID for secrets lookup.
    user_id: String,

    /// Server configuration (for token secret name lookup).
    server_config: Option<McpServerConfig>,

    /// Custom headers to include in every request.
    custom_headers: HashMap<String, String>,

    /// Ensures the MCP initialize handshake runs exactly once.
    /// Uses `OnceCell` to serialize concurrent callers so only one
    /// actually sends the request; subsequent calls return immediately.
    initialized: tokio::sync::OnceCell<InitializeResult>,
}

impl McpClient {
    /// Create a new simple MCP client (no authentication).
    ///
    /// Use this for local development servers or servers that don't require auth.
    pub fn new(server_url: impl Into<String>) -> Self {
        let url: String = server_url.into();
        let name = extract_server_name(&url);
        let transport = Arc::new(HttpMcpTransport::new(url.clone(), name.clone()));

        Self {
            transport,
            server_url: url,
            server_name: name,
            next_id: AtomicU64::new(1),
            tools_cache: RwLock::new(None),
            session_manager: None,
            secrets: None,
            user_id: "default".to_string(),
            server_config: None,
            custom_headers: HashMap::new(),
            initialized: tokio::sync::OnceCell::new(),
        }
    }

    /// Create a new simple MCP client with a specific name.
    ///
    /// Use this when you have a configured server name but no authentication.
    pub fn new_with_name(server_name: impl Into<String>, server_url: impl Into<String>) -> Self {
        let name: String = server_name.into();
        let url: String = server_url.into();
        let transport = Arc::new(HttpMcpTransport::new(url.clone(), name.clone()));

        Self {
            transport,
            server_url: url,
            server_name: name,
            next_id: AtomicU64::new(1),
            tools_cache: RwLock::new(None),
            session_manager: None,
            secrets: None,
            user_id: "default".to_string(),
            server_config: None,
            custom_headers: HashMap::new(),
            initialized: tokio::sync::OnceCell::new(),
        }
    }

    /// Create a new simple MCP client from an HTTP server configuration (no authentication).
    ///
    /// Use this when you have an `McpServerConfig` with custom headers but no OAuth.
    /// The config must use HTTP transport (the default); for stdio/UDS use `new_with_transport`.
    ///
    /// Returns an error if the config uses a non-HTTP transport.
    pub fn new_with_config(config: McpServerConfig) -> Result<Self, ToolError> {
        if !matches!(
            config.effective_transport(),
            crate::tools::mcp::config::EffectiveTransport::Http
        ) {
            return Err(ToolError::InvalidParameters(
                "new_with_config only supports HTTP transport; use new_with_transport for stdio/UDS"
                    .to_string(),
            ));
        }
        let transport = Arc::new(HttpMcpTransport::with_request_timeout(
            config.url.clone(),
            config.name.clone(),
            Duration::from_secs(config.request_timeout_seconds()),
        ));

        Ok(Self {
            transport,
            server_url: config.url.clone(),
            server_name: config.name.clone(),
            next_id: AtomicU64::new(1),
            tools_cache: RwLock::new(None),
            session_manager: None,
            secrets: None,
            user_id: "default".to_string(),
            custom_headers: config.headers.clone(),
            initialized: tokio::sync::OnceCell::new(),
            server_config: Some(config),
        })
    }

    /// Create an unauthenticated HTTP client with shared session tracking.
    pub(crate) fn new_with_config_and_session_manager(
        config: McpServerConfig,
        session_manager: Arc<McpSessionManager>,
        user_id: impl Into<String>,
    ) -> Result<Self, ToolError> {
        let transport = Arc::new(
            HttpMcpTransport::with_request_timeout(
                config.url.clone(),
                config.name.clone(),
                Duration::from_secs(config.request_timeout_seconds()),
            )
            .with_session_manager(Arc::clone(&session_manager)),
        );
        let mut client = Self::new_with_config(config)?;
        client.transport = transport;
        client.session_manager = Some(session_manager);
        client.user_id = user_id.into();
        Ok(client)
    }

    /// Create a new authenticated MCP client.
    ///
    /// Use this for hosted MCP servers that require OAuth authentication.
    pub fn new_authenticated(
        config: McpServerConfig,
        session_manager: Arc<McpSessionManager>,
        secrets: Arc<dyn SecretsStore + Send + Sync>,
        user_id: impl Into<String>,
    ) -> Self {
        let transport = Arc::new(
            HttpMcpTransport::with_request_timeout(
                config.url.clone(),
                config.name.clone(),
                Duration::from_secs(config.request_timeout_seconds()),
            )
            .with_session_manager(session_manager.clone()),
        );

        let custom_headers = config.headers.clone();

        Self {
            transport,
            server_url: config.url.clone(),
            server_name: config.name.clone(),
            next_id: AtomicU64::new(1),
            tools_cache: RwLock::new(None),
            session_manager: Some(session_manager),
            secrets: Some(secrets),
            user_id: user_id.into(),
            server_config: Some(config),
            custom_headers,
            initialized: tokio::sync::OnceCell::new(),
        }
    }

    /// Create a new MCP client with a custom transport.
    ///
    /// Use this for stdio, UDS, or other non-HTTP transports.
    pub fn new_with_transport(
        server_name: impl Into<String>,
        transport: Arc<dyn McpTransport>,
        session_manager: Option<Arc<McpSessionManager>>,
        secrets: Option<Arc<dyn SecretsStore + Send + Sync>>,
        user_id: impl Into<String>,
        server_config: Option<McpServerConfig>,
    ) -> Self {
        let name: String = server_name.into();
        let url = server_config
            .as_ref()
            .map(|c| c.url.clone())
            .unwrap_or_default();
        let custom_headers = server_config
            .as_ref()
            .map(|c| c.headers.clone())
            .unwrap_or_default();

        Self {
            transport,
            server_url: url,
            server_name: name,
            next_id: AtomicU64::new(1),
            tools_cache: RwLock::new(None),
            session_manager,
            secrets,
            user_id: user_id.into(),
            server_config,
            custom_headers,
            initialized: tokio::sync::OnceCell::new(),
        }
    }

    /// Attach a session manager for Streamable HTTP session tracking.
    pub fn with_session_manager(mut self, session_manager: Arc<McpSessionManager>) -> Self {
        self.session_manager = Some(session_manager);
        self
    }

    /// Get the server name.
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Get the server URL.
    pub fn server_url(&self) -> &str {
        &self.server_url
    }

    /// Whether this client has a session manager attached.
    pub fn has_session_manager(&self) -> bool {
        self.session_manager.is_some()
    }

    /// Whether the underlying persistent runtime can still accept requests.
    pub(crate) fn runtime_is_healthy(&self) -> bool {
        self.transport.is_healthy()
    }

    /// Whether this client owns an in-process MCP-over-WASM actor.
    pub(crate) fn runtime_is_wasm(&self) -> bool {
        self.server_config.as_ref().is_some_and(|config| {
            matches!(
                config.effective_transport(),
                crate::tools::mcp::config::EffectiveTransport::Wasm { .. }
            )
        })
    }

    #[cfg(test)]
    pub(crate) fn owner_user_id(&self) -> &str {
        &self.user_id
    }

    /// Shut down the transport and discard local session/cache state.
    pub async fn shutdown(&self) -> Result<(), ToolError> {
        let headers = match self.build_request_headers().await {
            Ok(headers) => headers,
            Err(error) => {
                tracing::warn!(
                    server = %self.server_name,
                    error = %error,
                    "Failed to resolve MCP shutdown headers; closing transport without them"
                );
                HashMap::new()
            }
        };
        let result = self.transport.shutdown_with_headers(&headers).await;
        if let Some(ref session_manager) = self.session_manager {
            session_manager.terminate(&self.server_name).await;
        }
        self.clear_cache().await;
        result
    }

    /// Get the next request ID.
    fn next_request_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Get the access token for this server (if authenticated).
    async fn get_access_token(&self) -> Result<Option<String>, ToolError> {
        let Some(ref secrets) = self.secrets else {
            return Ok(None);
        };
        let Some(ref config) = self.server_config else {
            return Ok(None);
        };
        match secrets
            .get_decrypted(&self.user_id, &config.token_secret_name())
            .await
        {
            Ok(token) => Ok(Some(token.expose().to_string())),
            Err(crate::secrets::SecretError::NotFound(_)) => Ok(None),
            Err(e) => Err(ToolError::ExternalService(format!(
                "Failed to get access token: {}",
                e
            ))),
        }
    }

    /// Build the headers map for a request (auth, session-id, custom headers).
    ///
    /// Custom headers are applied first. OAuth token injection is skipped if the
    /// user has explicitly configured an Authorization header, so user-provided
    /// credentials are never silently overwritten.
    async fn build_request_headers(&self) -> Result<HashMap<String, String>, ToolError> {
        let mut headers = self.custom_headers.clone();

        // Only inject OAuth token if the user hasn't set a custom Authorization header.
        let has_custom_auth = self
            .custom_headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("authorization"));
        if !has_custom_auth && let Some(token) = self.get_access_token().await? {
            let trimmed = token.trim();
            if !trimmed.is_empty() {
                headers.insert("Authorization".to_string(), format!("Bearer {}", trimmed));
            }
        }
        if let Some(ref session_manager) = self.session_manager
            && let Some(session_id) = session_manager.get_session_id(&self.server_name).await
        {
            headers.retain(|key, _| !key.eq_ignore_ascii_case("mcp-session-id"));
            headers.insert("Mcp-Session-Id".to_string(), session_id);
        }
        Ok(headers)
    }

    /// Re-run the MCP initialize handshake outside the OnceCell cache.
    ///
    /// This is used for recoverable session-expiry failures when an MCP server
    /// reports that the current session ID is no longer valid.
    async fn reinitialize_session(&self) -> Result<InitializeResult, ToolError> {
        if let Some(ref session_manager) = self.session_manager {
            session_manager.terminate(&self.server_name).await;
            session_manager
                .get_or_create(&self.server_name, &self.server_url)
                .await;
        }

        let request = McpRequest::initialize(self.next_request_id());
        let headers = match self.build_request_headers().await {
            Ok(headers) => headers,
            Err(error) => {
                self.cleanup_failed_initialization().await;
                return Err(error);
            }
        };
        let response = match self.transport.send(&request, &headers).await {
            Ok(response) => response,
            Err(error) => {
                self.cleanup_failed_initialization().await;
                return Err(error);
            }
        };

        if let Some(error) = response.error {
            self.cleanup_failed_initialization().await;
            return Err(ToolError::ExternalService(format!(
                "MCP initialization error: {} (code {})",
                error.message, error.code
            )));
        }

        let init_result: InitializeResult = match response
            .result
            .ok_or_else(|| {
                ToolError::ExternalService("No result in initialize response".to_string())
            })
            .and_then(|r| {
                serde_json::from_value(r).map_err(|e| {
                    ToolError::ExternalService(format!("Invalid initialize result: {}", e))
                })
            }) {
            Ok(result) => result,
            Err(error) => {
                self.cleanup_failed_initialization().await;
                return Err(error);
            }
        };

        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&init_result.protocol_version.as_str()) {
            let protocol_version = init_result.protocol_version.clone();
            self.cleanup_failed_initialization().await;
            return Err(ToolError::ExternalService(format!(
                "Unsupported MCP protocol version '{protocol_version}'"
            )));
        }

        let list_changed = init_result
            .capabilities
            .tools
            .as_ref()
            .is_some_and(|tools| tools.list_changed);
        self.transport.set_tools_list_changed(list_changed);

        let headers = match self.build_request_headers().await {
            Ok(headers) => headers,
            Err(error) => {
                self.cleanup_failed_initialization().await;
                return Err(error);
            }
        };

        // Streamable HTTP permits opening GET after initialize establishes its
        // session. Wait only for response headers: some servers defer them until
        // initialized, so bounded readiness must never block that notification.
        if list_changed && let Err(error) = self.transport.start_notifications(&headers).await {
            tracing::debug!(
                server = %self.server_name,
                %error,
                "Failed to start MCP notification stream"
            );
        }

        let notification = McpRequest::initialized_notification();
        if let Err(e) = self.transport.send(&notification, &headers).await {
            tracing::debug!(
                "Failed to send initialized notification to '{}': {}",
                self.server_name,
                e
            );
        }

        if let Some(ref session_manager) = self.session_manager {
            session_manager.mark_initialized(&self.server_name).await;
        }

        Ok(init_result)
    }

    async fn cleanup_failed_initialization(&self) {
        let headers = self.build_request_headers().await.unwrap_or_default();
        if let Err(error) = self.transport.shutdown_with_headers(&headers).await {
            tracing::debug!(
                server = %self.server_name,
                %error,
                "Failed to close transport after MCP initialization failure"
            );
        }
        if let Some(ref session_manager) = self.session_manager {
            session_manager.terminate(&self.server_name).await;
        }
        self.clear_cache().await;
    }

    /// Return true when the error looks like a recoverable MCP session expiry.
    fn is_session_expiry_error(message: &str) -> bool {
        let lower = message.to_ascii_lowercase();
        lower.contains("session")
            && (lower.contains("400")
                || lower.contains("missing session id")
                || lower.contains("no valid session id"))
    }

    /// Send a request to the MCP server with auth and session headers.
    /// Automatically attempts token refresh on 401 errors (HTTP transports only).
    async fn send_request(&self, request: McpRequest) -> Result<McpResponse, ToolError> {
        // For non-HTTP transports, just send directly without retry logic
        if !self.transport.supports_http_features() {
            let headers = self.build_request_headers().await?;
            return self.transport.send(&request, &headers).await;
        }

        // HTTP transport: try up to 2 times (first attempt, then retry after token refresh
        // or recoverable session reinitialization).
        for attempt in 0..2 {
            let headers = self.build_request_headers().await?;
            let result = self.transport.send(&request, &headers).await;

            match result {
                Ok(response) => return Ok(response),
                Err(ToolError::ExternalService(ref msg))
                    if attempt == 0
                        && self.session_manager.is_some()
                        && Self::is_session_expiry_error(msg) =>
                {
                    tracing::debug!(
                        "MCP session expired, attempting reinitialize for '{}'",
                        self.server_name
                    );
                    self.reinitialize_session().await?;
                    continue;
                }
                Err(ToolError::ExternalService(ref msg))
                    if msg.contains("401")
                        || msg.contains("Unauthorized")
                        || (msg.contains("400") && {
                            let lower = msg.to_ascii_lowercase();
                            lower.contains("authorization") || lower.contains("authenticate")
                        }) =>
                {
                    if attempt == 0
                        && let Some(ref secrets) = self.secrets
                        && let Some(ref config) = self.server_config
                    {
                        tracing::debug!(
                            "MCP token expired, attempting refresh for '{}'",
                            self.server_name
                        );
                        match refresh_access_token(config, secrets, &self.user_id).await {
                            Ok(_) => {
                                tracing::info!("MCP token refreshed for '{}'", self.server_name);
                                continue;
                            }
                            Err(e) => {
                                tracing::debug!(
                                    "Token refresh failed for '{}': {}",
                                    self.server_name,
                                    e
                                );
                            }
                        }
                    }
                    return Err(ToolError::ExternalService(format!(
                        "MCP server '{}' requires authentication. Run: lunarwing mcp auth {}",
                        self.server_name, self.server_name
                    )));
                }
                Err(e) => return Err(e),
            }
        }

        Err(ToolError::ExternalService(
            "MCP request failed after retry".to_string(),
        ))
    }

    /// Initialize the connection to the MCP server.
    ///
    /// Uses `OnceCell` to guarantee that exactly one caller performs the
    /// handshake, even under concurrent access. Subsequent calls return
    /// immediately.
    pub async fn initialize(&self) -> Result<InitializeResult, ToolError> {
        let startup_timeout = Duration::from_secs(
            self.server_config
                .as_ref()
                .map(McpServerConfig::startup_timeout_seconds)
                .unwrap_or(crate::tools::mcp::config::DEFAULT_MCP_STARTUP_TIMEOUT_SECONDS),
        );
        let result = self
            .initialized
            .get_or_try_init(|| async {
                tokio::time::timeout(startup_timeout, self.reinitialize_session())
                    .await
                    .map_err(|_| {
                        ToolError::ExternalService(format!(
                            "MCP initialization for '{}' exceeded {} second startup limit",
                            self.server_name,
                            startup_timeout.as_secs()
                        ))
                    })?
            })
            .await?;

        Ok(result.clone())
    }

    /// List available tools from the MCP server.
    pub async fn list_tools(&self) -> Result<Vec<McpTool>, ToolError> {
        self.list_tools_with_generation(MAX_TOOLS_LIST_DURATION)
            .await
            .map(|(tools, _)| tools)
    }

    async fn list_tools_with_generation(
        &self,
        operation_timeout: Duration,
    ) -> Result<(Vec<McpTool>, u64), ToolError> {
        let tools = tokio::time::timeout(operation_timeout, async {
            loop {
                let generation = self.tools_generation();
                if let Some((cached_generation, tools)) = self.tools_cache.read().await.as_ref()
                    && *cached_generation == generation
                {
                    return Ok::<(Vec<McpTool>, u64), ToolError>((tools.clone(), generation));
                }
                let tools = self.list_all_tools().await?;
                if self.tools_generation() == generation {
                    *self.tools_cache.write().await = Some((generation, tools.clone()));
                    return Ok::<(Vec<McpTool>, u64), ToolError>((tools, generation));
                }
            }
        })
        .await
        .map_err(|_| {
            ToolError::ExternalService(format!(
                "MCP tools/list exceeded {} second operation limit",
                operation_timeout.as_secs_f64()
            ))
        })??;
        Ok(tools)
    }

    #[cfg(test)]
    async fn list_tools_with_timeout(
        &self,
        operation_timeout: Duration,
    ) -> Result<Vec<McpTool>, ToolError> {
        self.list_tools_with_generation(operation_timeout)
            .await
            .map(|(tools, _)| tools)
    }

    async fn list_all_tools(&self) -> Result<Vec<McpTool>, ToolError> {
        self.initialize().await?;
        let mut tools = Vec::new();
        let mut cursor = None;
        let mut seen_cursors = HashSet::new();
        let mut serialized_bytes = 0usize;

        for page in 1..=MAX_TOOLS_LIST_PAGES {
            let request = McpRequest::list_tools_page(self.next_request_id(), cursor.as_deref());
            let response = self.send_request(request).await?;
            if let Some(error) = response.error {
                return Err(ToolError::ExternalService(format!(
                    "MCP error: {} (code {})",
                    error.message, error.code
                )));
            }

            let value = response.result.ok_or_else(|| {
                ToolError::ExternalService("No result in MCP response".to_string())
            })?;
            let page_bytes = serde_json::to_vec(&value).map_err(|error| {
                ToolError::ExternalService(format!("Invalid tools list: {error}"))
            })?;
            serialized_bytes = serialized_bytes
                .checked_add(page_bytes.len())
                .ok_or_else(|| {
                    ToolError::ExternalService("MCP tools/list byte count overflow".to_string())
                })?;
            if serialized_bytes > MAX_TOOLS_LIST_BYTES {
                return Err(ToolError::ExternalService(format!(
                    "MCP tools/list exceeded {MAX_TOOLS_LIST_BYTES} serialized bytes"
                )));
            }

            let result: ListToolsResult = serde_json::from_value(value).map_err(|error| {
                ToolError::ExternalService(format!("Invalid tools list: {error}"))
            })?;
            if tools.len().saturating_add(result.tools.len()) > MAX_TOOLS_LIST_COUNT {
                return Err(ToolError::ExternalService(format!(
                    "MCP tools/list exceeded {MAX_TOOLS_LIST_COUNT} tools"
                )));
            }
            tools.extend(result.tools);

            let Some(next_cursor) = result.next_cursor else {
                return Ok(tools);
            };
            if !seen_cursors.insert(next_cursor.clone()) {
                return Err(ToolError::ExternalService(format!(
                    "MCP tools/list repeated cursor '{next_cursor}'"
                )));
            }
            cursor = Some(next_cursor);

            if page == MAX_TOOLS_LIST_PAGES {
                return Err(ToolError::ExternalService(format!(
                    "MCP tools/list exceeded {MAX_TOOLS_LIST_PAGES} pages"
                )));
            }
        }

        unreachable!("tools/list page loop always returns")
    }

    /// Call a tool on the MCP server.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<CallToolResult, ToolError> {
        self.initialize().await?;

        let request = McpRequest::call_tool(self.next_request_id(), name, arguments);
        let response = self.send_request(request).await?;

        if let Some(error) = response.error {
            return Err(ToolError::ExecutionFailed(format!(
                "MCP tool error: {} (code {})",
                error.message, error.code
            )));
        }

        response
            .result
            .ok_or_else(|| ToolError::ExternalService("No result in MCP response".to_string()))
            .and_then(|r| {
                serde_json::from_value(r)
                    .map_err(|e| ToolError::ExternalService(format!("Invalid tool result: {}", e)))
            })
    }

    /// Clear the tools cache.
    pub async fn clear_cache(&self) {
        *self.tools_cache.write().await = None;
    }

    /// Monotonic generation advanced by negotiated tools/list change notifications.
    pub(crate) fn tools_generation(&self) -> u64 {
        self.transport.tools_generation()
    }

    /// Create Tool implementations for all MCP tools.
    pub async fn create_tools(&self) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        let mcp_tools = self.list_tools().await?;
        Ok(self.wrap_tools(mcp_tools))
    }

    /// Create tools for an untrusted worker, retaining exact discovery generation.
    pub(crate) async fn create_worker_tools_with_generation(
        &self,
    ) -> Result<(Vec<Arc<dyn Tool>>, usize, u64), ToolError> {
        let (mcp_tools, generation) = self
            .list_tools_with_generation(MAX_TOOLS_LIST_DURATION)
            .await?;
        let descriptor_bytes = validate_worker_tools(&mcp_tools)?;
        Ok((self.wrap_tools(mcp_tools), descriptor_bytes, generation))
    }

    fn wrap_tools(&self, mcp_tools: Vec<McpTool>) -> Vec<Arc<dyn Tool>> {
        let client = Arc::new(self.clone());
        mcp_tools
            .into_iter()
            .map(|t| {
                let prefixed_name = format!("{}_{}", self.server_name, t.name);
                Arc::new(McpToolWrapper {
                    tool: t,
                    prefixed_name,
                    client: client.clone(),
                }) as Arc<dyn Tool>
            })
            .collect()
    }

    /// Test the connection to the MCP server.
    pub async fn test_connection(&self) -> Result<(), ToolError> {
        self.initialize().await?;
        self.list_tools().await?;
        Ok(())
    }
}

fn validate_worker_tools(tools: &[McpTool]) -> Result<usize, ToolError> {
    if tools.len() > MAX_WORKER_MCP_TOOLS {
        return Err(ToolError::ExternalService(format!(
            "worker MCP tool limit exceeded ({} discovered, max {MAX_WORKER_MCP_TOOLS})",
            tools.len()
        )));
    }
    let encoded = serde_json::to_vec(tools).map_err(|error| {
        ToolError::ExternalService(format!("failed to measure worker MCP tools: {error}"))
    })?;
    if encoded.len() > MAX_WORKER_MCP_TOOL_BYTES {
        return Err(ToolError::ExternalService(format!(
            "worker MCP tool descriptors exceed {MAX_WORKER_MCP_TOOL_BYTES} bytes"
        )));
    }
    Ok(encoded.len())
}

/// Clone the client, resetting the tools cache and preserving completed initialization state.
/// The `next_id` counter is copied so cloned clients continue with monotonically increasing IDs.
impl Clone for McpClient {
    fn clone(&self) -> Self {
        let initialized = tokio::sync::OnceCell::new();
        if let Some(result) = self.initialized.get() {
            let _ = initialized.set(result.clone());
        }
        Self {
            transport: self.transport.clone(),
            server_url: self.server_url.clone(),
            server_name: self.server_name.clone(),
            next_id: AtomicU64::new(self.next_id.load(Ordering::SeqCst)),
            tools_cache: RwLock::new(None),
            session_manager: self.session_manager.clone(),
            secrets: self.secrets.clone(),
            user_id: self.user_id.clone(),
            server_config: self.server_config.clone(),
            custom_headers: self.custom_headers.clone(),
            initialized,
        }
    }
}

/// Extract a server name from a URL for logging/display purposes.
fn extract_server_name(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
        .unwrap_or_else(|| "unknown".to_string())
        .replace('.', "_")
}

/// Wrapper that implements Tool for an MCP tool.
struct McpToolWrapper {
    tool: McpTool,
    prefixed_name: String,
    client: Arc<McpClient>,
}

#[async_trait]
impl Tool for McpToolWrapper {
    fn name(&self) -> &str {
        &self.prefixed_name
    }
    fn description(&self) -> &str {
        &self.tool.description
    }
    fn parameters_schema(&self) -> serde_json::Value {
        self.tool.input_schema.clone()
    }

    fn owner_user_id(&self) -> Option<&str> {
        Some(&self.client.user_id)
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        if ctx.user_id != self.client.user_id {
            return Err(ToolError::NotAuthorized(
                "MCP tool is owned by a different user".to_string(),
            ));
        }
        let start = std::time::Instant::now();

        // Strip top-level null values before forwarding — LLMs often emit
        // `"field": null` for optional params, but many MCP servers reject
        // explicit nulls for fields that should simply be absent.
        let params = strip_top_level_nulls(params);

        let result = self.client.call_tool(&self.tool.name, params).await?;
        let content: String = result
            .content
            .iter()
            .filter_map(|b| b.as_text())
            .collect::<Vec<_>>()
            .join("\n");
        if result.is_error {
            return Err(ToolError::ExecutionFailed(content));
        }
        Ok(ToolOutput::text(content, start.elapsed()))
    }

    fn requires_sanitization(&self) -> bool {
        true
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        if self.client.server_config.as_ref().is_some_and(|config| {
            config
                .always_require_approval
                .iter()
                .any(|name| name == &self.tool.name)
        }) {
            ApprovalRequirement::Always
        } else if self.tool.requires_approval() {
            ApprovalRequirement::UnlessAutoApproved
        } else {
            ApprovalRequirement::Never
        }
    }
}

/// Remove top-level keys whose value is JSON null from an object.
///
/// LLMs frequently emit `"field": null` for optional parameters.  Many MCP
/// servers (e.g. Notion) treat an explicit `null` as an invalid value for
/// optional fields that should simply be absent.  Stripping these before
/// forwarding avoids 400-class rejections from strict servers.
fn strip_top_level_nulls(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let filtered = map.into_iter().filter(|(_, v)| !v.is_null()).collect();
            serde_json::Value::Object(filtered)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker_tool(name: &str, schema: serde_json::Value) -> McpTool {
        McpTool {
            name: name.to_string(),
            description: String::new(),
            input_schema: schema,
            annotations: None,
        }
    }

    #[test]
    fn worker_tool_discovery_enforces_count_and_size_limits() {
        let too_many = (0..=MAX_WORKER_MCP_TOOLS)
            .map(|index| worker_tool(&format!("tool-{index}"), serde_json::json!({})))
            .collect::<Vec<_>>();
        assert!(validate_worker_tools(&too_many).is_err());

        let oversized = vec![worker_tool(
            "large",
            serde_json::json!({"description": "x".repeat(MAX_WORKER_MCP_TOOL_BYTES)}),
        )];
        assert!(validate_worker_tools(&oversized).is_err());
        assert!(validate_worker_tools(&[worker_tool("small", serde_json::json!({}))]).is_ok());
    }

    #[test]
    fn test_mcp_request_list_tools() {
        let req = McpRequest::list_tools(1);
        assert_eq!(req.method, "tools/list");
        assert_eq!(req.id, Some(1));
    }

    #[test]
    fn test_mcp_request_call_tool() {
        let req = McpRequest::call_tool(2, "test", serde_json::json!({"key": "value"}));
        assert_eq!(req.method, "tools/call");
        assert!(req.params.is_some());
    }

    #[test]
    fn test_extract_server_name() {
        assert_eq!(
            extract_server_name("https://mcp.notion.com/v1"),
            "mcp_notion_com"
        );
        assert_eq!(extract_server_name("http://localhost:8080"), "localhost");
        assert_eq!(extract_server_name("invalid"), "unknown");
    }

    #[test]
    fn test_simple_client_creation() {
        let client = McpClient::new("http://localhost:8080");
        assert_eq!(client.server_url(), "http://localhost:8080");
        assert!(client.session_manager.is_none());
        assert!(client.secrets.is_none());
    }

    #[test]
    fn test_extract_server_name_with_port() {
        assert_eq!(
            extract_server_name("http://example.com:3000"),
            "example_com"
        );
    }

    #[test]
    fn test_extract_server_name_with_path() {
        assert_eq!(
            extract_server_name("http://api.server.io/v2/mcp"),
            "api_server_io"
        );
    }

    #[test]
    fn test_extract_server_name_with_query_params() {
        assert_eq!(
            extract_server_name("http://mcp.example.com/endpoint?token=abc&v=1"),
            "mcp_example_com"
        );
    }

    #[test]
    fn test_extract_server_name_https() {
        assert_eq!(
            extract_server_name("https://secure.mcp.dev"),
            "secure_mcp_dev"
        );
    }

    #[test]
    fn test_extract_server_name_ip_address() {
        assert_eq!(
            extract_server_name("http://192.168.1.100:9090/mcp"),
            "192_168_1_100"
        );
    }

    #[test]
    fn test_new_defaults() {
        let client = McpClient::new("http://localhost:9999");
        assert_eq!(client.server_url(), "http://localhost:9999");
        assert_eq!(client.server_name(), "localhost");
        assert!(client.session_manager.is_none());
        assert!(client.secrets.is_none());
        assert_eq!(client.user_id, "default");
    }

    #[test]
    fn test_new_with_name_uses_custom_name() {
        let client = McpClient::new_with_name("my-server", "http://localhost:8080");
        assert_eq!(client.server_name(), "my-server");
        assert_eq!(client.server_url(), "http://localhost:8080");
        assert_eq!(client.user_id, "default");
        assert!(client.session_manager.is_none());
        assert!(client.secrets.is_none());
    }

    #[test]
    fn test_server_name_accessor() {
        let client = McpClient::new("https://tools.example.org/mcp");
        assert_eq!(client.server_name(), "tools_example_org");
    }

    #[test]
    fn test_server_url_accessor() {
        let url = "https://tools.example.org/mcp?v=2";
        let client = McpClient::new(url);
        assert_eq!(client.server_url(), url);
    }

    #[test]
    fn test_clone_preserves_fields() {
        let client = McpClient::new_with_name("cloned-server", "http://localhost:5555");
        client.next_request_id();
        client.next_request_id();
        let cloned = client.clone();
        assert_eq!(cloned.server_url(), "http://localhost:5555");
        assert_eq!(cloned.server_name(), "cloned-server");
        assert_eq!(cloned.user_id, "default");
        assert_eq!(cloned.next_id.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_clone_resets_tools_cache() {
        let client = McpClient::new("http://localhost:5555");
        let cloned = client.clone();
        let cache = cloned.tools_cache.read().await;
        assert!(cache.is_none());
    }

    #[test]
    fn test_new_with_config_carries_custom_headers() {
        let mut headers = HashMap::new();
        headers.insert("X-API-Key".to_string(), "secret".to_string());
        headers.insert("X-Custom".to_string(), "value".to_string());

        let config = McpServerConfig::new("test", "http://localhost:8080").with_headers(headers);
        let client = McpClient::new_with_config(config.clone()).expect("HTTP config should work");

        assert_eq!(client.server_name(), "test");
        assert_eq!(client.server_url(), "http://localhost:8080");
        assert_eq!(client.custom_headers.len(), 2);
        assert_eq!(client.custom_headers.get("X-API-Key").unwrap(), "secret");
        assert!(client.server_config.is_some());
    }

    #[test]
    fn test_new_with_config_no_headers() {
        let config = McpServerConfig::new("bare", "http://localhost:9090");
        let client = McpClient::new_with_config(config).expect("HTTP config should work");

        assert_eq!(client.server_name(), "bare");
        assert!(client.custom_headers.is_empty());
        assert!(client.secrets.is_none());
        assert!(client.session_manager.is_none());
    }

    #[test]
    fn test_with_session_manager() {
        let client = McpClient::new("http://localhost:8080");
        assert!(!client.has_session_manager());

        let session_manager = Arc::new(McpSessionManager::new());
        let client = client.with_session_manager(session_manager);

        assert!(client.has_session_manager());
    }

    #[test]
    fn test_next_request_id_monotonically_increasing() {
        let client = McpClient::new("http://localhost:1234");
        assert_eq!(client.next_request_id(), 1);
        assert_eq!(client.next_request_id(), 2);
        assert_eq!(client.next_request_id(), 3);
    }

    #[test]
    fn test_mcp_tool_requires_approval_destructive() {
        use crate::tools::mcp::protocol::{McpTool, McpToolAnnotations};
        let tool = McpTool {
            name: "delete_all".to_string(),
            description: "Deletes everything".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            annotations: Some(McpToolAnnotations {
                destructive_hint: true,
                side_effects_hint: false,
                read_only_hint: false,
                execution_time_hint: None,
                ..Default::default()
            }),
        };
        assert!(tool.requires_approval());
    }

    #[test]
    fn test_mcp_tool_no_approval_when_not_destructive() {
        use crate::tools::mcp::protocol::{McpTool, McpToolAnnotations};
        let tool = McpTool {
            name: "read_data".to_string(),
            description: "Reads data".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            annotations: Some(McpToolAnnotations {
                destructive_hint: false,
                side_effects_hint: true,
                read_only_hint: false,
                execution_time_hint: None,
                ..Default::default()
            }),
        };
        assert!(!tool.requires_approval());
    }

    #[test]
    fn test_mcp_tool_no_approval_when_no_annotations() {
        use crate::tools::mcp::protocol::McpTool;
        let tool = McpTool {
            name: "simple_tool".to_string(),
            description: "A simple tool".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            annotations: None,
        };
        assert!(!tool.requires_approval());
    }

    /// Mock transport for testing transport abstraction behavior.
    struct MockTransport {
        supports_http: bool,
        responses: std::sync::Mutex<Vec<McpResponse>>,
        recorded_requests: std::sync::Mutex<Vec<McpRequest>>,
        recorded_headers: std::sync::Mutex<Vec<HashMap<String, String>>>,
        shutdown_called: std::sync::atomic::AtomicBool,
        send_delay: Duration,
        list_changed_enabled: std::sync::atomic::AtomicBool,
        tools_generation: AtomicU64,
    }

    impl MockTransport {
        fn new(supports_http: bool, responses: Vec<McpResponse>) -> Self {
            Self {
                supports_http,
                responses: std::sync::Mutex::new(responses),
                recorded_requests: std::sync::Mutex::new(Vec::new()),
                recorded_headers: std::sync::Mutex::new(Vec::new()),
                shutdown_called: std::sync::atomic::AtomicBool::new(false),
                send_delay: Duration::ZERO,
                list_changed_enabled: std::sync::atomic::AtomicBool::new(false),
                tools_generation: AtomicU64::new(0),
            }
        }

        fn new_delayed(responses: Vec<McpResponse>, send_delay: Duration) -> Self {
            Self {
                send_delay,
                ..Self::new(false, responses)
            }
        }
        fn recorded_headers(&self) -> Vec<HashMap<String, String>> {
            self.recorded_headers.lock().unwrap().clone()
        }

        fn recorded_requests(&self) -> Vec<McpRequest> {
            self.recorded_requests.lock().unwrap().clone()
        }

        fn shutdown_called(&self) -> bool {
            self.shutdown_called.load(Ordering::SeqCst)
        }

        fn notify_tools_changed(&self) {
            if self.list_changed_enabled.load(Ordering::Acquire) {
                self.tools_generation.fetch_add(1, Ordering::AcqRel);
            }
        }
    }

    #[async_trait]
    impl McpTransport for MockTransport {
        async fn send(
            &self,
            request: &McpRequest,
            headers: &HashMap<String, String>,
        ) -> Result<McpResponse, ToolError> {
            tokio::time::sleep(self.send_delay).await;
            self.recorded_requests.lock().unwrap().push(request.clone());
            self.recorded_headers.lock().unwrap().push(headers.clone());
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                return Err(ToolError::ExternalService(
                    "No more mock responses".to_string(),
                ));
            }
            Ok(responses.remove(0))
        }
        async fn shutdown(&self) -> Result<(), ToolError> {
            self.shutdown_called.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn supports_http_features(&self) -> bool {
            self.supports_http
        }
        fn set_tools_list_changed(&self, enabled: bool) {
            self.list_changed_enabled.store(enabled, Ordering::Release);
        }
        fn tools_generation(&self) -> u64 {
            self.tools_generation.load(Ordering::Acquire)
        }
    }

    /// Mock transport that can return errors and successful responses in a
    /// controlled sequence.
    struct RetryMockTransport {
        supports_http: bool,
        outcomes: std::sync::Mutex<std::collections::VecDeque<Result<McpResponse, ToolError>>>,
        recorded_headers: std::sync::Mutex<Vec<HashMap<String, String>>>,
    }

    impl RetryMockTransport {
        fn new(supports_http: bool, outcomes: Vec<Result<McpResponse, ToolError>>) -> Self {
            Self {
                supports_http,
                outcomes: std::sync::Mutex::new(outcomes.into()),
                recorded_headers: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn recorded_headers(&self) -> Vec<HashMap<String, String>> {
            self.recorded_headers.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl McpTransport for RetryMockTransport {
        async fn send(
            &self,
            _request: &McpRequest,
            headers: &HashMap<String, String>,
        ) -> Result<McpResponse, ToolError> {
            self.recorded_headers.lock().unwrap().push(headers.clone());
            let mut outcomes = self.outcomes.lock().unwrap();
            if outcomes.is_empty() {
                return Err(ToolError::ExternalService(
                    "No more mock outcomes".to_string(),
                ));
            }
            outcomes.pop_front().unwrap()
        }

        async fn shutdown(&self) -> Result<(), ToolError> {
            Ok(())
        }

        fn supports_http_features(&self) -> bool {
            self.supports_http
        }
    }

    #[tokio::test]
    async fn test_non_http_transport_skips_401_retry() {
        // initialize response, then notification ack (consumed but ignored),
        // then list_tools response
        let init_response = McpResponse {
            jsonrpc: "2.0".to_string(),
            id: Some(1),
            result: Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "serverInfo": {"name": "test", "version": "1.0"}
            })),
            error: None,
        };
        let notification_ack = McpResponse {
            jsonrpc: "2.0".to_string(),
            id: None,
            result: None,
            error: None,
        };
        let list_response = McpResponse {
            jsonrpc: "2.0".to_string(),
            id: Some(2),
            result: Some(serde_json::json!({"tools": []})),
            error: None,
        };
        let transport = Arc::new(MockTransport::new(
            false,
            vec![init_response, notification_ack, list_response],
        ));
        let client = McpClient::new_with_transport(
            "test-stdio",
            transport.clone(),
            None,
            None,
            "default",
            None,
        );
        let result = client.list_tools().await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 0);
        let headers = transport.recorded_headers();
        // 3 sends: initialize + notifications/initialized + list_tools
        assert_eq!(headers.len(), 3);
        assert!(!headers[0].contains_key("Authorization"));
        assert!(!headers[0].contains_key("Mcp-Session-Id"));
    }

    #[tokio::test]
    async fn test_transport_supports_http_features_accessor() {
        let http_transport = HttpMcpTransport::new("http://localhost:8080", "test");
        assert!(http_transport.supports_http_features());
        let mock_non_http = MockTransport::new(false, vec![]);
        assert!(!mock_non_http.supports_http_features());
    }

    #[tokio::test]
    async fn test_client_shutdown_releases_transport_and_session() {
        let transport = Arc::new(MockTransport::new(false, Vec::new()));
        let sessions = Arc::new(McpSessionManager::new());
        sessions
            .get_or_create("test-shutdown", "stdio://test-shutdown")
            .await;
        let client = McpClient::new_with_transport(
            "test-shutdown",
            transport.clone(),
            Some(Arc::clone(&sessions)),
            None,
            "default",
            None,
        );

        client.shutdown().await.expect("client shutdown");

        assert!(transport.shutdown_called());
        assert!(sessions.active_servers().await.is_empty());
    }

    /// Regression test for issue #890: stdio clients must auto-initialize
    /// even without a session manager, and the second call should be idempotent.
    #[tokio::test]
    async fn test_stdio_client_auto_initializes_without_session_manager() {
        let init_response = McpResponse {
            jsonrpc: "2.0".to_string(),
            id: Some(1),
            result: Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "serverInfo": {"name": "test", "version": "1.0"}
            })),
            error: None,
        };
        let notification_ack = McpResponse {
            jsonrpc: "2.0".to_string(),
            id: None,
            result: None,
            error: None,
        };
        let transport = Arc::new(MockTransport::new(
            false,
            vec![init_response, notification_ack],
        ));
        let client = McpClient::new_with_transport(
            "test-stdio",
            transport.clone(),
            None, // no session manager
            None,
            "default",
            None,
        );

        // First call should send initialize + notification
        let result = client.initialize().await;
        assert!(result.is_ok());
        assert_eq!(transport.recorded_headers().len(), 2);

        // Second call should be a no-op (idempotent via local flag)
        let result2 = client.initialize().await;
        assert!(result2.is_ok());
        assert_eq!(transport.recorded_headers().len(), 2); // no additional sends
    }

    fn successful_initialize_response() -> McpResponse {
        McpResponse {
            jsonrpc: "2.0".to_string(),
            id: Some(1),
            result: Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "serverInfo": {"name": "test", "version": "1.0"}
            })),
            error: None,
        }
    }

    fn notification_ack() -> McpResponse {
        McpResponse {
            jsonrpc: "2.0".to_string(),
            id: None,
            result: None,
            error: None,
        }
    }

    fn tools_page(tools: serde_json::Value, next_cursor: Option<&str>) -> McpResponse {
        let mut result = serde_json::json!({"tools": tools});
        if let Some(cursor) = next_cursor {
            result["nextCursor"] = serde_json::json!(cursor);
        }
        McpResponse {
            jsonrpc: "2.0".to_string(),
            id: Some(2),
            result: Some(result),
            error: None,
        }
    }

    fn test_client(transport: Arc<MockTransport>) -> McpClient {
        McpClient::new_with_transport("test", transport, None, None, "default", None)
    }

    #[tokio::test]
    async fn test_initialize_rejects_unsupported_version_before_notification() {
        let mut response = successful_initialize_response();
        response.result.as_mut().expect("result")["protocolVersion"] =
            serde_json::json!("2025-03-26");
        let transport = Arc::new(MockTransport::new(false, vec![response]));
        let client = test_client(transport.clone());

        let error = client.initialize().await.expect_err("version must fail");

        assert!(
            error
                .to_string()
                .contains("Unsupported MCP protocol version")
        );
        let requests = transport.recorded_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "initialize");
        assert!(transport.shutdown_called());
        assert!(client.initialized.get().is_none());
    }

    #[tokio::test]
    async fn test_list_tools_follows_pages_in_order() {
        let transport = Arc::new(MockTransport::new(
            false,
            vec![
                successful_initialize_response(),
                notification_ack(),
                tools_page(
                    serde_json::json!([{"name": "first", "inputSchema": {}}]),
                    Some("page-2"),
                ),
                tools_page(
                    serde_json::json!([{"name": "second", "inputSchema": {}}]),
                    None,
                ),
            ],
        ));
        let client = test_client(transport.clone());

        let tools = client.list_tools().await.expect("list all pages");

        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        let requests = transport.recorded_requests();
        assert!(requests[2].params.is_none());
        assert_eq!(
            requests[3].params,
            Some(serde_json::json!({"cursor": "page-2"}))
        );
    }

    #[tokio::test]
    async fn list_changed_generation_invalidates_tools_cache() {
        let mut initialize = successful_initialize_response();
        initialize.result.as_mut().expect("initialize result")["capabilities"] =
            serde_json::json!({"tools": {"listChanged": true}});
        let transport = Arc::new(MockTransport::new(
            false,
            vec![
                initialize,
                notification_ack(),
                tools_page(
                    serde_json::json!([{"name": "old", "inputSchema": {}}]),
                    None,
                ),
                tools_page(
                    serde_json::json!([{"name": "new", "inputSchema": {}}]),
                    None,
                ),
            ],
        ));
        let client = test_client(Arc::clone(&transport));

        assert_eq!(client.list_tools().await.unwrap()[0].name, "old");
        assert_eq!(client.list_tools().await.unwrap()[0].name, "old");
        transport.notify_tools_changed();
        assert_eq!(client.list_tools().await.unwrap()[0].name, "new");
        assert_eq!(client.tools_generation(), 1);
        assert_eq!(transport.recorded_requests().len(), 4);
    }

    #[tokio::test]
    async fn test_worker_tool_limit_sees_all_paginated_tools() {
        let first_page = (0..200)
            .map(|index| serde_json::json!({"name": format!("tool-{index}"), "inputSchema": {}}))
            .collect::<Vec<_>>();
        let second_page = (200..257)
            .map(|index| serde_json::json!({"name": format!("tool-{index}"), "inputSchema": {}}))
            .collect::<Vec<_>>();
        let transport = Arc::new(MockTransport::new(
            false,
            vec![
                successful_initialize_response(),
                notification_ack(),
                tools_page(serde_json::json!(first_page), Some("page-2")),
                tools_page(serde_json::json!(second_page), None),
            ],
        ));

        let error = test_client(transport)
            .create_worker_tools_with_generation()
            .await
            .err()
            .expect("aggregate worker limit must fail");
        assert!(error.to_string().contains("257 discovered, max 256"));
    }

    #[tokio::test]
    async fn test_list_tools_rejects_repeated_cursor() {
        let transport = Arc::new(MockTransport::new(
            false,
            vec![
                successful_initialize_response(),
                notification_ack(),
                tools_page(serde_json::json!([]), Some("repeat")),
                tools_page(serde_json::json!([]), Some("repeat")),
            ],
        ));
        let client = test_client(transport);

        let error = client
            .list_tools()
            .await
            .expect_err("cursor loop must fail");
        assert!(error.to_string().contains("repeated cursor 'repeat'"));
    }

    #[tokio::test]
    async fn test_list_tools_enforces_count_bound() {
        let tools = (0..=MAX_TOOLS_LIST_COUNT)
            .map(|index| serde_json::json!({"name": format!("tool-{index}"), "inputSchema": {}}))
            .collect::<Vec<_>>();
        let transport = Arc::new(MockTransport::new(
            false,
            vec![
                successful_initialize_response(),
                notification_ack(),
                tools_page(serde_json::json!(tools), None),
            ],
        ));

        let error = test_client(transport)
            .list_tools()
            .await
            .expect_err("count limit must fail");
        assert!(error.to_string().contains("exceeded 4096 tools"));
    }

    #[tokio::test]
    async fn test_list_tools_enforces_serialized_byte_bound() {
        let transport = Arc::new(MockTransport::new(
            false,
            vec![
                successful_initialize_response(),
                notification_ack(),
                tools_page(
                    serde_json::json!([{
                        "name": "oversized",
                        "description": "x".repeat(MAX_TOOLS_LIST_BYTES),
                        "inputSchema": {}
                    }]),
                    None,
                ),
            ],
        ));

        let error = test_client(transport)
            .list_tools()
            .await
            .expect_err("byte limit must fail");
        assert!(error.to_string().contains("serialized bytes"));
    }

    #[tokio::test]
    async fn test_list_tools_enforces_page_bound() {
        let mut responses = vec![successful_initialize_response(), notification_ack()];
        responses.extend(
            (0..MAX_TOOLS_LIST_PAGES)
                .map(|page| tools_page(serde_json::json!([]), Some(&format!("page-{page}")))),
        );
        let transport = Arc::new(MockTransport::new(false, responses));

        let error = test_client(transport)
            .list_tools()
            .await
            .expect_err("page limit must fail");
        assert!(error.to_string().contains("exceeded 100 pages"));
    }

    #[tokio::test]
    async fn test_list_tools_enforces_whole_operation_timeout() {
        let transport = Arc::new(MockTransport::new_delayed(
            vec![successful_initialize_response()],
            Duration::from_millis(50),
        ));

        let error = test_client(transport)
            .list_tools_with_timeout(Duration::from_millis(1))
            .await
            .expect_err("operation timeout must fail");
        assert!(error.to_string().contains("operation limit"));
    }

    #[tokio::test]
    async fn test_http_session_error_triggers_reinitialize_and_retry() {
        let init_response = McpResponse {
            jsonrpc: "2.0".to_string(),
            id: Some(1),
            result: Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "serverInfo": {"name": "test", "version": "1.0"}
            })),
            error: None,
        };
        let notification_ack = McpResponse {
            jsonrpc: "2.0".to_string(),
            id: None,
            result: None,
            error: None,
        };
        let notification_ack2 = notification_ack.clone();
        let session_error = Err(ToolError::ExternalService(
            "[test] MCP server returned status: 400 - No valid session ID provided".to_string(),
        ));
        let reinit_response = McpResponse {
            jsonrpc: "2.0".to_string(),
            id: Some(2),
            result: Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "serverInfo": {"name": "test", "version": "1.0"}
            })),
            error: None,
        };
        let call_response = McpResponse {
            jsonrpc: "2.0".to_string(),
            id: Some(3),
            result: Some(serde_json::json!({
                "content": [{"type": "text", "text": "pong"}],
                "is_error": false
            })),
            error: None,
        };

        let transport = Arc::new(RetryMockTransport::new(
            true,
            vec![
                Ok(init_response),
                Ok(notification_ack),
                session_error,
                Ok(reinit_response),
                Ok(notification_ack2),
                Ok(call_response),
            ],
        ));
        let session_manager = Arc::new(McpSessionManager::new());
        let client = McpClient::new_with_transport(
            "test-http",
            transport.clone(),
            Some(session_manager),
            None,
            "default",
            None,
        );

        client.initialize().await.expect("initial handshake");

        let result = client
            .call_tool("echo", serde_json::json!({"input": "hello"}))
            .await
            .expect("call should recover after session expiry");
        assert!(!result.is_error);
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.content[0].as_text(), Some("pong"));

        let headers = transport.recorded_headers();
        assert_eq!(headers.len(), 6);
    }

    #[test]
    fn test_strip_top_level_nulls_removes_null_fields() {
        let input = serde_json::json!({
            "query": "search term",
            "sort": null,
            "filter": null,
            "page_size": 10
        });
        let result = strip_top_level_nulls(input);
        let obj = result.as_object().unwrap();
        assert_eq!(obj.len(), 2);
        assert_eq!(obj["query"], "search term");
        assert_eq!(obj["page_size"], 10);
        assert!(!obj.contains_key("sort"));
        assert!(!obj.contains_key("filter"));
    }

    #[test]
    fn test_strip_top_level_nulls_preserves_non_objects() {
        let input = serde_json::json!("just a string");
        let result = strip_top_level_nulls(input.clone());
        assert_eq!(result, input);
    }

    #[test]
    fn test_strip_top_level_nulls_preserves_nested_nulls() {
        let input = serde_json::json!({
            "outer": { "inner": null },
            "top_null": null
        });
        let result = strip_top_level_nulls(input);
        let obj = result.as_object().unwrap();
        assert_eq!(obj.len(), 1);
        assert!(obj["outer"]["inner"].is_null());
    }

    // --- Issue 1 regression: new_with_config rejects non-HTTP transport ---

    #[test]
    fn test_new_with_config_rejects_stdio_transport() {
        let config = McpServerConfig::new_stdio(
            "stdio-server",
            "echo",
            vec!["hello".to_string()],
            HashMap::new(),
        );
        let result = McpClient::new_with_config(config);
        let err = result
            .err()
            .expect("stdio config must be rejected")
            .to_string();
        assert!(
            err.contains("new_with_config only supports HTTP"),
            "error should explain the restriction: {}",
            err
        );
    }

    // --- Issue 13: McpToolWrapper unit tests ---

    fn make_test_mcp_tool(destructive: bool) -> McpTool {
        use crate::tools::mcp::protocol::McpToolAnnotations;
        McpTool {
            name: "do_thing".to_string(),
            description: "Does a thing".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "input": {"type": "string"}
                }
            }),
            annotations: if destructive {
                Some(McpToolAnnotations {
                    destructive_hint: true,
                    side_effects_hint: false,
                    read_only_hint: false,
                    execution_time_hint: None,
                    ..Default::default()
                })
            } else {
                None
            },
        }
    }

    #[test]
    fn test_mcp_tool_wrapper_name_is_prefixed() {
        let client = Arc::new(McpClient::new("http://localhost:8080"));
        let wrapper = McpToolWrapper {
            tool: make_test_mcp_tool(false),
            prefixed_name: "mcp__myserver__do_thing".to_string(),
            client,
        };
        assert_eq!(wrapper.name(), "mcp__myserver__do_thing");
    }

    #[test]
    fn test_mcp_tool_wrapper_description() {
        let client = Arc::new(McpClient::new("http://localhost:8080"));
        let wrapper = McpToolWrapper {
            tool: make_test_mcp_tool(false),
            prefixed_name: "mcp__s__do_thing".to_string(),
            client,
        };
        assert_eq!(wrapper.description(), "Does a thing");
    }

    #[test]
    fn test_mcp_tool_wrapper_parameters_schema() {
        let client = Arc::new(McpClient::new("http://localhost:8080"));
        let wrapper = McpToolWrapper {
            tool: make_test_mcp_tool(false),
            prefixed_name: "mcp__s__do_thing".to_string(),
            client,
        };
        let schema = wrapper.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["input"].is_object());
    }

    #[test]
    fn test_mcp_tool_wrapper_requires_sanitization() {
        let client = Arc::new(McpClient::new("http://localhost:8080"));
        let wrapper = McpToolWrapper {
            tool: make_test_mcp_tool(false),
            prefixed_name: "mcp__s__do_thing".to_string(),
            client,
        };
        assert!(
            wrapper.requires_sanitization(),
            "MCP tools should always require sanitization"
        );
    }

    #[test]
    fn test_mcp_tool_wrapper_approval_destructive() {
        let client = Arc::new(McpClient::new("http://localhost:8080"));
        let wrapper = McpToolWrapper {
            tool: make_test_mcp_tool(true),
            prefixed_name: "mcp__s__do_thing".to_string(),
            client,
        };
        let approval = wrapper.requires_approval(&serde_json::json!({}));
        assert_eq!(approval, ApprovalRequirement::UnlessAutoApproved);
    }

    #[test]
    fn test_mcp_tool_wrapper_approval_non_destructive() {
        let client = Arc::new(McpClient::new("http://localhost:8080"));
        let wrapper = McpToolWrapper {
            tool: make_test_mcp_tool(false),
            prefixed_name: "mcp__s__do_thing".to_string(),
            client,
        };
        let approval = wrapper.requires_approval(&serde_json::json!({}));
        assert_eq!(approval, ApprovalRequirement::Never);
    }

    #[test]
    fn test_mcp_tool_wrapper_always_approval_from_server_config() {
        let mut client = McpClient::new("http://localhost:8080");
        client.server_config = Some(
            McpServerConfig::new("configured", "http://localhost:8080")
                .with_always_require_approval(["do_thing"]),
        );
        let wrapper = McpToolWrapper {
            tool: make_test_mcp_tool(false),
            prefixed_name: "configured_do_thing".to_string(),
            client: Arc::new(client),
        };

        assert_eq!(
            wrapper.requires_approval(&serde_json::json!({})),
            ApprovalRequirement::Always
        );
    }

    #[tokio::test]
    async fn test_mcp_tool_wrapper_rejects_another_user() {
        let transport = Arc::new(MockTransport::new(false, Vec::new()));
        let client = Arc::new(McpClient::new_with_transport(
            "owned-server",
            transport,
            None,
            None,
            "alice",
            None,
        ));
        let wrapper = McpToolWrapper {
            tool: make_test_mcp_tool(false),
            prefixed_name: "owned-server_do_thing".to_string(),
            client,
        };
        let context = JobContext::with_user("bob", "test", "test");

        let error = wrapper
            .execute(serde_json::json!({}), &context)
            .await
            .expect_err("another user must not invoke an owner's MCP client");

        assert!(matches!(error, ToolError::NotAuthorized(_)));
    }

    // Regression test: empty/whitespace-only tokens must not produce a
    // malformed `Authorization: Bearer ` header (GitHub MCP returns 400
    // "Authorization header is badly formatted" in this case).
    #[tokio::test]
    async fn test_build_headers_skips_empty_token() {
        use crate::secrets::{CreateSecretParams, DecryptedSecret, Secret, SecretError, SecretRef};
        use uuid::Uuid;

        // In-memory secrets store that returns a whitespace-only string for the token.
        struct EmptyTokenStore;
        #[async_trait]
        impl crate::secrets::SecretsStore for EmptyTokenStore {
            async fn create(
                &self,
                _user_id: &str,
                _params: CreateSecretParams,
            ) -> Result<Secret, SecretError> {
                unimplemented!()
            }
            async fn get(&self, _user_id: &str, _name: &str) -> Result<Secret, SecretError> {
                unimplemented!()
            }
            async fn get_decrypted(
                &self,
                _user_id: &str,
                _name: &str,
            ) -> Result<DecryptedSecret, SecretError> {
                DecryptedSecret::from_bytes(b"   ".to_vec())
            }
            async fn exists(&self, _user_id: &str, _name: &str) -> Result<bool, SecretError> {
                Ok(true)
            }
            async fn delete(&self, _user_id: &str, _name: &str) -> Result<bool, SecretError> {
                Ok(true)
            }
            async fn list(&self, _user_id: &str) -> Result<Vec<SecretRef>, SecretError> {
                Ok(Vec::new())
            }
            async fn record_usage(&self, _secret_id: Uuid) -> Result<(), SecretError> {
                Ok(())
            }
            async fn is_accessible(
                &self,
                _user_id: &str,
                _secret_name: &str,
                _allowed_secrets: &[String],
            ) -> Result<bool, SecretError> {
                Ok(true)
            }
        }

        let config = McpServerConfig::new("github", "https://api.githubcopilot.com/mcp/");
        let session_manager = Arc::new(McpSessionManager::new());
        let secrets: Arc<dyn crate::secrets::SecretsStore + Send + Sync> =
            Arc::new(EmptyTokenStore);

        let client = McpClient::new_authenticated(config, session_manager, secrets, "test-user");

        let headers = client.build_request_headers().await.unwrap(); // safety: test
        assert!(
            // safety: test
            !headers.contains_key("Authorization"),
            "Empty/whitespace token must not produce an Authorization header, got: {:?}",
            headers.get("Authorization")
        );
    }

    // Regression test: tokens with leading/trailing whitespace must be trimmed
    // before being used in the Authorization header.
    #[tokio::test]
    async fn test_build_headers_trims_token() {
        use crate::secrets::{CreateSecretParams, DecryptedSecret, Secret, SecretError, SecretRef};
        use uuid::Uuid;

        struct PaddedTokenStore;
        #[async_trait]
        impl crate::secrets::SecretsStore for PaddedTokenStore {
            async fn create(
                &self,
                _user_id: &str,
                _params: CreateSecretParams,
            ) -> Result<Secret, SecretError> {
                unimplemented!()
            }
            async fn get(&self, _user_id: &str, _name: &str) -> Result<Secret, SecretError> {
                unimplemented!()
            }
            async fn get_decrypted(
                &self,
                _user_id: &str,
                _name: &str,
            ) -> Result<DecryptedSecret, SecretError> {
                DecryptedSecret::from_bytes(b"  gho_abc123  \n".to_vec())
            }
            async fn exists(&self, _user_id: &str, _name: &str) -> Result<bool, SecretError> {
                Ok(true)
            }
            async fn delete(&self, _user_id: &str, _name: &str) -> Result<bool, SecretError> {
                Ok(true)
            }
            async fn list(&self, _user_id: &str) -> Result<Vec<SecretRef>, SecretError> {
                Ok(Vec::new())
            }
            async fn record_usage(&self, _secret_id: Uuid) -> Result<(), SecretError> {
                Ok(())
            }
            async fn is_accessible(
                &self,
                _user_id: &str,
                _secret_name: &str,
                _allowed_secrets: &[String],
            ) -> Result<bool, SecretError> {
                Ok(true)
            }
        }

        let config = McpServerConfig::new("github", "https://api.githubcopilot.com/mcp/");
        let session_manager = Arc::new(McpSessionManager::new());
        let secrets: Arc<dyn crate::secrets::SecretsStore + Send + Sync> =
            Arc::new(PaddedTokenStore);

        let client = McpClient::new_authenticated(config, session_manager, secrets, "test-user");

        let headers = client.build_request_headers().await.unwrap(); // safety: test
        assert_eq!(
            // safety: test
            headers.get("Authorization").unwrap(), // safety: test
            "Bearer gho_abc123",
            "Token must be trimmed before use in Authorization header"
        );
    }
}
