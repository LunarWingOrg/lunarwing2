//! Runtime-neutral MCP activation boundary.
//!
//! Transports describe how JSON-RPC messages move. Runtime adapters additionally
//! own activation concerns such as process spawning, WASM actor creation, remote
//! authentication, and gateway routing. Keeping this boundary above
//! [`McpTransport`](crate::tools::mcp::McpTransport) allows new placements to reuse
//! the same client, discovery, lifecycle, and tool-wrapper logic.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::secrets::SecretsStore;
use crate::tools::mcp::config::{EffectiveTransport, McpServerConfig};
use crate::tools::mcp::{McpClient, McpProcessManager, McpSessionManager};

/// Runtime family selected by an MCP server configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum McpRuntimeKind {
    Http,
    Stdio,
    Unix,
    Wasm,
    Gateway,
}

impl McpRuntimeKind {
    /// Resolve the runtime family without exposing transport-specific fields.
    pub fn from_config(config: &McpServerConfig) -> Self {
        match config.effective_transport() {
            EffectiveTransport::Http => Self::Http,
            EffectiveTransport::Stdio { .. } => Self::Stdio,
            EffectiveTransport::Unix { .. } => Self::Unix,
            EffectiveTransport::Wasm { .. } => Self::Wasm,
            EffectiveTransport::Gateway { .. } => Self::Gateway,
        }
    }
}

/// Shared services available to an MCP runtime adapter during activation.
#[derive(Clone)]
pub struct McpRuntimeContext<'a> {
    session_manager: &'a Arc<McpSessionManager>,
    process_manager: &'a Arc<McpProcessManager>,
    secrets: Option<Arc<dyn SecretsStore + Send + Sync>>,
    user_id: &'a str,
}

impl<'a> McpRuntimeContext<'a> {
    pub fn new(
        session_manager: &'a Arc<McpSessionManager>,
        process_manager: &'a Arc<McpProcessManager>,
        secrets: Option<Arc<dyn SecretsStore + Send + Sync>>,
        user_id: &'a str,
    ) -> Self {
        Self {
            session_manager,
            process_manager,
            secrets,
            user_id,
        }
    }

    pub fn session_manager(&self) -> &Arc<McpSessionManager> {
        self.session_manager
    }

    pub fn process_manager(&self) -> &Arc<McpProcessManager> {
        self.process_manager
    }

    pub fn secrets(&self) -> Option<Arc<dyn SecretsStore + Send + Sync>> {
        self.secrets.clone()
    }

    pub fn user_id(&self) -> &str {
        self.user_id
    }
}

/// One activation request passed to a runtime adapter.
pub struct McpRuntimeRequest<'a> {
    server: McpServerConfig,
    context: McpRuntimeContext<'a>,
}

impl<'a> McpRuntimeRequest<'a> {
    pub fn new(server: McpServerConfig, context: McpRuntimeContext<'a>) -> Self {
        Self { server, context }
    }

    pub fn server(&self) -> &McpServerConfig {
        &self.server
    }

    pub fn context(&self) -> &McpRuntimeContext<'a> {
        &self.context
    }

    pub fn into_parts(self) -> (McpServerConfig, McpRuntimeContext<'a>) {
        (self.server, self.context)
    }
}

/// Error returned while selecting or activating an MCP runtime.
#[derive(Debug, thiserror::Error)]
pub enum McpRuntimeError {
    #[error("Failed to spawn stdio MCP server '{name}': {reason}")]
    StdioSpawn { name: String, reason: String },
    #[error("Failed to connect to Unix MCP server '{name}': {reason}")]
    UnixConnect { name: String, reason: String },
    #[error("Unix socket transport is not supported on this platform (server '{name}')")]
    UnixNotSupported { name: String },
    #[error("Failed to load MCP-over-WASM server '{name}': {reason}")]
    WasmLoad { name: String, reason: String },
    #[error("Failed to configure MCP gateway server '{name}': {reason}")]
    Gateway { name: String, reason: String },
    #[error("Invalid configuration for MCP server '{name}': {reason}")]
    InvalidConfig { name: String, reason: String },
    #[error("Package provenance check failed for MCP server '{name}': {reason}")]
    Provenance { name: String, reason: String },
    #[error("No MCP runtime adapter is registered for {kind:?}")]
    MissingAdapter { kind: McpRuntimeKind },
}

/// Activates one runtime family and returns a normal MCP client.
#[async_trait]
pub trait McpRuntimeAdapter: Send + Sync {
    fn kind(&self) -> McpRuntimeKind;

    async fn connect(&self, request: McpRuntimeRequest<'_>) -> Result<McpClient, McpRuntimeError>;
}

/// Extensible runtime adapter registry used by the MCP client factory.
pub struct McpRuntimeRegistry {
    adapters: HashMap<McpRuntimeKind, Arc<dyn McpRuntimeAdapter>>,
}

impl McpRuntimeRegistry {
    pub fn new(adapters: impl IntoIterator<Item = Arc<dyn McpRuntimeAdapter>>) -> Self {
        let mut registry = Self {
            adapters: HashMap::new(),
        };
        for adapter in adapters {
            registry.register(adapter);
        }
        registry
    }

    pub fn register(&mut self, adapter: Arc<dyn McpRuntimeAdapter>) {
        self.adapters.insert(adapter.kind(), adapter);
    }

    pub async fn connect(
        &self,
        request: McpRuntimeRequest<'_>,
    ) -> Result<McpClient, McpRuntimeError> {
        let kind = McpRuntimeKind::from_config(request.server());
        let adapter = self
            .adapters
            .get(&kind)
            .ok_or(McpRuntimeError::MissingAdapter { kind })?;
        adapter.connect(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestAdapter;

    #[async_trait]
    impl McpRuntimeAdapter for TestAdapter {
        fn kind(&self) -> McpRuntimeKind {
            McpRuntimeKind::Http
        }

        async fn connect(
            &self,
            request: McpRuntimeRequest<'_>,
        ) -> Result<McpClient, McpRuntimeError> {
            Ok(McpClient::new_with_name(
                request.server().name.clone(),
                request.server().url.clone(),
            ))
        }
    }

    #[tokio::test]
    async fn registry_dispatches_by_runtime_kind() {
        let sessions = Arc::new(McpSessionManager::new());
        let processes = Arc::new(McpProcessManager::new());
        let context = McpRuntimeContext::new(&sessions, &processes, None, "owner");
        let request = McpRuntimeRequest::new(
            McpServerConfig::new("remote", "http://localhost:1234/mcp"),
            context,
        );
        let adapters: Vec<Arc<dyn McpRuntimeAdapter>> = vec![Arc::new(TestAdapter)];
        let client = McpRuntimeRegistry::new(adapters)
            .connect(request)
            .await
            .expect("adapter dispatch");

        assert_eq!(client.server_name(), "remote");
    }

    #[tokio::test]
    async fn registry_fails_closed_without_adapter() {
        let sessions = Arc::new(McpSessionManager::new());
        let processes = Arc::new(McpProcessManager::new());
        let context = McpRuntimeContext::new(&sessions, &processes, None, "owner");
        let request = McpRuntimeRequest::new(
            McpServerConfig::new_wasm("portable", "/opt/mcp/server.wasm"),
            context,
        );
        let registry = McpRuntimeRegistry::new(Vec::new());
        let error = match registry.connect(request).await {
            Ok(_) => panic!("missing adapter must fail"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            McpRuntimeError::MissingAdapter {
                kind: McpRuntimeKind::Wasm
            }
        ));
    }
}
