//! Factory for creating MCP clients from server configuration.
//!
//! Encapsulates the transport dispatch logic (stdio, Unix socket, HTTP)
//! so that callers don't need to match on `EffectiveTransport` themselves.

use std::sync::Arc;

use crate::secrets::SecretsStore;
use crate::tools::ToolError;
use crate::tools::mcp::config::{EffectiveTransport, McpServerConfig};
use crate::tools::mcp::{McpClient, McpProcessManager, McpSessionManager, McpTransport};

/// Error returned when MCP client creation fails.
#[derive(Debug, thiserror::Error)]
pub enum McpFactoryError {
    #[error("Failed to spawn stdio MCP server '{name}': {reason}")]
    StdioSpawn { name: String, reason: String },
    #[error("Failed to connect to Unix MCP server '{name}': {reason}")]
    UnixConnect { name: String, reason: String },
    #[error("Unix socket transport is not supported on this platform (server '{name}')")]
    UnixNotSupported { name: String },
    #[error("Invalid configuration for MCP server '{name}': {reason}")]
    InvalidConfig { name: String, reason: String },
}

/// Create an `McpClient` from a server configuration, dispatching on the
/// effective transport type.
pub async fn create_client_from_config(
    server: McpServerConfig,
    session_manager: &Arc<McpSessionManager>,
    process_manager: &Arc<McpProcessManager>,
    secrets: Option<Arc<dyn SecretsStore + Send + Sync>>,
    user_id: &str,
) -> Result<McpClient, McpFactoryError> {
    let server_name = server.name.clone();

    match server.effective_transport() {
        EffectiveTransport::Stdio { command, args, env } => {
            let transport = process_manager
                .spawn_stdio(&server_name, command, args.to_vec(), env.clone())
                .await
                .map_err(|e| McpFactoryError::StdioSpawn {
                    name: server_name.clone(),
                    reason: e.to_string(),
                })?;

            Ok(McpClient::new_with_transport(
                &server_name,
                transport as Arc<dyn McpTransport>,
                None,
                secrets,
                user_id,
                Some(server),
            ))
        }
        #[cfg(unix)]
        EffectiveTransport::Unix { socket_path } => {
            let transport = crate::tools::mcp::unix_transport::UnixMcpTransport::connect(
                &server_name,
                socket_path,
            )
            .await
            .map_err(|e| McpFactoryError::UnixConnect {
                name: server_name.clone(),
                reason: e.to_string(),
            })?;

            Ok(McpClient::new_with_transport(
                &server_name,
                Arc::new(transport) as Arc<dyn McpTransport>,
                None,
                secrets,
                user_id,
                Some(server),
            ))
        }
        #[cfg(not(unix))]
        EffectiveTransport::Unix { .. } => {
            Err(McpFactoryError::UnixNotSupported { name: server_name })
        }
        EffectiveTransport::Http => {
            if let Some(ref secrets) = secrets {
                let has_tokens =
                    crate::tools::mcp::is_authenticated(&server, secrets, user_id).await;

                if has_tokens || server.requires_auth() {
                    Ok(McpClient::new_authenticated(
                        server,
                        Arc::clone(session_manager),
                        Arc::clone(secrets),
                        user_id,
                    ))
                } else {
                    Ok(McpClient::new_with_config(server)
                        .map_err(|e| McpFactoryError::InvalidConfig {
                            name: server_name.clone(),
                            reason: e.to_string(),
                        })?
                        .with_session_manager(Arc::clone(session_manager)))
                }
            } else {
                Ok(McpClient::new_with_config(server)
                    .map_err(|e| McpFactoryError::InvalidConfig {
                        name: server_name,
                        reason: e.to_string(),
                    })?
                    .with_session_manager(Arc::clone(session_manager)))
            }
        }
    }
}

/// Shut down a live MCP client, its managed stdio process (when present), and session state.
pub async fn shutdown_client_runtime(
    server_name: &str,
    client: Option<&McpClient>,
    session_manager: &McpSessionManager,
    process_manager: &McpProcessManager,
) -> Result<(), ToolError> {
    let client_error = if let Some(client) = client {
        client.shutdown().await.err()
    } else {
        None
    };
    let process_error = process_manager.shutdown(server_name).await.err();
    session_manager.terminate(server_name).await;

    match (client_error, process_error) {
        (None, None) => Ok(()),
        (Some(client_error), None) => Err(ToolError::ExternalService(format!(
            "Failed to stop MCP server '{server_name}' transport: {client_error}"
        ))),
        (None, Some(process_error)) => Err(ToolError::ExternalService(format!(
            "Failed to stop MCP server '{server_name}' process: {process_error}"
        ))),
        (Some(client_error), Some(process_error)) => Err(ToolError::ExternalService(format!(
            "Failed to stop MCP server '{server_name}' transport ({client_error}) and process ({process_error})"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_factory_non_oauth_http_has_session_manager() {
        let server = McpServerConfig::new("test-server", "http://localhost:9999");
        let session_manager = Arc::new(McpSessionManager::new());
        let process_manager = Arc::new(McpProcessManager::new());

        let client = create_client_from_config(
            server,
            &session_manager,
            &process_manager,
            None,
            "test-user",
        )
        .await
        .expect("factory should succeed for HTTP config");

        assert!(
            client.has_session_manager(),
            "non-OAuth HTTP clients must carry a session manager"
        );
    }

    #[tokio::test]
    async fn test_shutdown_client_runtime_stops_managed_stdio_process() {
        let session_manager = Arc::new(McpSessionManager::new());
        let process_manager = Arc::new(McpProcessManager::new());
        let transport = process_manager
            .spawn_stdio(
                "shutdown-test",
                "cat",
                Vec::<String>::new(),
                std::collections::HashMap::new(),
            )
            .await
            .expect("spawn test process");
        let client = McpClient::new_with_transport(
            "shutdown-test",
            transport,
            Some(Arc::clone(&session_manager)),
            None,
            "test",
            None,
        );
        session_manager
            .get_or_create("shutdown-test", "stdio://shutdown-test")
            .await;

        shutdown_client_runtime(
            "shutdown-test",
            Some(&client),
            &session_manager,
            &process_manager,
        )
        .await
        .expect("shutdown runtime");

        assert!(process_manager.managed_servers().await.is_empty());
        assert!(
            !session_manager
                .active_servers()
                .await
                .contains(&"shutdown-test".to_string())
        );
    }
}
