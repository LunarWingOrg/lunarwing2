//! MCP gateway transport adapter.
//!
//! Dedicated gateways expose one server per endpoint. Shared gateways expose one
//! endpoint for multiple upstream servers and select the upstream with a
//! host-managed route header. Both shapes retain normal Streamable HTTP session,
//! OAuth, shutdown, and retry behavior through [`HttpMcpTransport`].

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::tools::mcp::config::McpGatewayMode;
use crate::tools::mcp::http_transport::HttpMcpTransport;
use crate::tools::mcp::protocol::{McpRequest, McpResponse};
use crate::tools::mcp::session::McpSessionManager;
use crate::tools::mcp::transport::McpTransport;
use crate::tools::tool::ToolError;

pub const GATEWAY_MODE_HEADER: &str = "X-LunarWing-MCP-Gateway-Mode";
pub const GATEWAY_ROUTE_HEADER: &str = "X-LunarWing-MCP-Route";

/// Streamable HTTP adapter for dedicated and shared MCP gateways.
pub struct GatewayMcpTransport {
    inner: HttpMcpTransport,
    mode: McpGatewayMode,
    route: Option<String>,
}

impl GatewayMcpTransport {
    pub fn new(
        endpoint: impl Into<String>,
        server_name: impl Into<String>,
        mode: McpGatewayMode,
        route: Option<String>,
        session_manager: Arc<McpSessionManager>,
    ) -> Self {
        Self {
            inner: HttpMcpTransport::new(endpoint, server_name)
                .with_session_manager(session_manager),
            mode,
            route,
        }
    }

    fn managed_headers(&self, headers: &HashMap<String, String>) -> HashMap<String, String> {
        let mut managed = headers.clone();
        managed.retain(|name, _| {
            !name.eq_ignore_ascii_case(GATEWAY_MODE_HEADER)
                && !name.eq_ignore_ascii_case(GATEWAY_ROUTE_HEADER)
        });
        let mode = match self.mode {
            McpGatewayMode::Dedicated => "dedicated",
            McpGatewayMode::Shared => "shared",
        };
        managed.insert(GATEWAY_MODE_HEADER.to_string(), mode.to_string());
        if let Some(route) = &self.route {
            managed.insert(GATEWAY_ROUTE_HEADER.to_string(), route.clone());
        }
        managed
    }
}

#[async_trait]
impl McpTransport for GatewayMcpTransport {
    async fn send(
        &self,
        request: &McpRequest,
        headers: &HashMap<String, String>,
    ) -> Result<McpResponse, ToolError> {
        self.inner
            .send(request, &self.managed_headers(headers))
            .await
    }

    async fn shutdown(&self) -> Result<(), ToolError> {
        self.inner
            .shutdown_with_headers(&self.managed_headers(&HashMap::new()))
            .await
    }

    async fn shutdown_with_headers(
        &self,
        headers: &HashMap<String, String>,
    ) -> Result<(), ToolError> {
        self.inner
            .shutdown_with_headers(&self.managed_headers(headers))
            .await
    }

    fn supports_http_features(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedicated_gateway_owns_mode_header() {
        let transport = GatewayMcpTransport::new(
            "http://localhost:1234/mcp",
            "test",
            McpGatewayMode::Dedicated,
            None,
            Arc::new(McpSessionManager::new()),
        );
        let supplied = HashMap::from([
            (
                GATEWAY_MODE_HEADER.to_ascii_lowercase(),
                "shared".to_string(),
            ),
            (GATEWAY_ROUTE_HEADER.to_string(), "attacker".to_string()),
            ("Authorization".to_string(), "Bearer token".to_string()),
        ]);
        let headers = transport.managed_headers(&supplied);

        assert_eq!(
            headers.get(GATEWAY_MODE_HEADER).map(String::as_str),
            Some("dedicated")
        );
        assert!(!headers.contains_key(GATEWAY_ROUTE_HEADER));
        assert_eq!(
            headers.get("Authorization").map(String::as_str),
            Some("Bearer token")
        );
    }

    #[test]
    fn shared_gateway_injects_validated_route() {
        let transport = GatewayMcpTransport::new(
            "http://localhost:1234/mcp",
            "test",
            McpGatewayMode::Shared,
            Some("project-tools".to_string()),
            Arc::new(McpSessionManager::new()),
        );
        let headers = transport.managed_headers(&HashMap::new());

        assert_eq!(
            headers.get(GATEWAY_MODE_HEADER).map(String::as_str),
            Some("shared")
        );
        assert_eq!(
            headers.get(GATEWAY_ROUTE_HEADER).map(String::as_str),
            Some("project-tools")
        );
    }
}
