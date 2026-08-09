//! Model Context Protocol (MCP) integration.
//!
//! MCP allows the agent to connect to external tool servers that provide
//! additional capabilities through a standardized protocol.
//!
//! Supports local, hosted, sandboxed WASM, and gateway-routed servers.
//! Runtime adapters cover HTTP (Streamable HTTP / SSE), stdio subprocesses,
//! Unix domain sockets, stateful WASM components, and dedicated/shared gateways.
//!
//! ## Usage
//!
//! ```ignore
//! // Simple client (no auth)
//! let client = McpClient::new("http://localhost:8080");
//!
//! // Authenticated client (for hosted servers)
//! let client = McpClient::new_authenticated(
//!     config,
//!     session_manager,
//!     secrets,
//!     "user_id",
//! );
//!
//! // List and register tools
//! let tools = client.create_tools().await?;
//! for tool in tools {
//!     registry.register(tool);
//! }
//! ```

pub mod auth;
mod client;
pub mod config;
pub mod factory;
pub(crate) mod http_transport;
pub(crate) mod process;
mod protocol;
pub mod provenance;
pub mod runtime;
pub mod sandbox;
pub mod session;
pub(crate) mod stdio_transport;
pub(crate) mod transport;
#[cfg(unix)]
pub(crate) mod unix_transport;

pub use auth::{is_authenticated, refresh_access_token};
pub use client::McpClient;
pub use config::{
    McpGatewayMode, McpServerConfig, McpServersFile, McpTransportConfig, OAuthConfig,
};
pub use factory::{McpFactoryError, create_client_from_config};
pub use process::{McpProcessManager, McpProcessState};
pub use protocol::{InitializeResult, McpRequest, McpResponse, McpTool};
pub use provenance::{McpPackageProvenance, PackageEcosystem, PackageTrust, ProvenanceError};
pub use runtime::{
    McpRuntimeAdapter, McpRuntimeContext, McpRuntimeError, McpRuntimeKind, McpRuntimeRegistry,
    McpRuntimeRequest,
};
pub use sandbox::{McpSandboxConfig, SandboxError};
pub use session::McpSessionManager;
pub use transport::McpTransport;
