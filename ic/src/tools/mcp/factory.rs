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
                    Ok(McpClient::new_with_config_and_session_manager(
                        server,
                        Arc::clone(session_manager),
                        user_id,
                    )
                    .map_err(|e| McpFactoryError::InvalidConfig {
                        name: server_name.clone(),
                        reason: e.to_string(),
                    })?)
                }
            } else {
                Ok(McpClient::new_with_config_and_session_manager(
                    server,
                    Arc::clone(session_manager),
                    user_id,
                )
                .map_err(|e| McpFactoryError::InvalidConfig {
                    name: server_name,
                    reason: e.to_string(),
                })?)
            }
        }
    }
}

/// Release all runtime state associated with an MCP client.
pub(crate) async fn shutdown_client_runtime(
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
        (Some(error), None) => Err(ToolError::ExternalService(format!(
            "Failed to stop MCP server '{server_name}' transport: {error}"
        ))),
        (None, Some(error)) => Err(ToolError::ExternalService(format!(
            "Failed to stop MCP server '{server_name}' process: {error}"
        ))),
        (Some(client_error), Some(process_error)) => Err(ToolError::ExternalService(format!(
            "Failed to stop MCP server '{server_name}' transport ({client_error}) and process ({process_error})"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn test_factory_non_oauth_http_has_session_manager() {
        use axum::http::{HeaderMap, StatusCode};
        use axum::{Router, routing::delete};
        use tokio::net::TcpListener;

        let terminated = Arc::new(AtomicBool::new(false));
        let terminated_by_handler = Arc::clone(&terminated);
        let app = Router::new().route(
            "/",
            delete(move |headers: HeaderMap| {
                let terminated = Arc::clone(&terminated_by_handler);
                async move {
                    if headers
                        .get("mcp-session-id")
                        .and_then(|value| value.to_str().ok())
                        == Some("factory-session")
                    {
                        terminated.store(true, Ordering::Release);
                    }
                    StatusCode::NO_CONTENT
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let server = McpServerConfig::new("test-server", format!("http://{address}"));
        let session_manager = Arc::new(McpSessionManager::new());
        let process_manager = Arc::new(McpProcessManager::new());
        session_manager
            .get_or_create("test-server", &server.url)
            .await;
        session_manager
            .update_session_id("test-server", Some("factory-session".to_string()))
            .await;

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
        assert_eq!(client.owner_user_id(), "test-user");
        client.shutdown().await.expect("shutdown factory client");
        assert!(
            terminated.load(Ordering::Acquire),
            "factory HTTP transport must terminate its remote session"
        );
        task.abort();
    }
}
