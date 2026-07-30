//! Factory for creating MCP clients from server configuration.
//!
//! Encapsulates the transport dispatch logic (stdio, Unix socket, HTTP)
//! so that callers don't need to match on `EffectiveTransport` themselves.

use std::collections::HashMap;
use std::sync::Arc;

use crate::secrets::SecretsStore;
use crate::tools::ToolError;
use crate::tools::mcp::config::{
    EffectiveTransport, McpServerConfig, validate_managed_study_notes_runtime,
};
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
    validate_managed_study_notes_runtime(&server).map_err(|error| {
        McpFactoryError::InvalidConfig {
            name: server_name.clone(),
            reason: error.to_string(),
        }
    })?;

    match server.effective_transport() {
        EffectiveTransport::Stdio {
            command,
            args,
            env,
            env_secrets,
            auto_install,
        } => {
            if let Some(install) = auto_install {
                crate::tools::mcp::package_install::ensure_package_installed(
                    &server_name,
                    command,
                    install,
                )
                .await
                .map_err(|e| McpFactoryError::StdioSpawn {
                    name: server_name.clone(),
                    reason: e.to_string(),
                })?;
            }

            let resolved_env = resolve_stdio_env(
                &server_name,
                env,
                env_secrets,
                secrets.as_deref(),
                user_id,
            )
            .await?;

            let transport = process_manager
                .spawn_stdio(&server_name, command, args.to_vec(), resolved_env)
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

/// Resolve the environment for a stdio MCP server, injecting values from the
/// secrets store for every entry in `env_secrets` (env var name -> secret name).
///
/// Fails closed: a missing/unreadable secret — or a missing secrets store —
/// aborts the spawn. Secret values are decrypted at spawn time, never logged,
/// and never persisted in the server configuration. Note that crash restarts
/// reuse the environment resolved at the initial spawn; rotating a secret
/// requires deactivating and reactivating the server.
async fn resolve_stdio_env(
    server_name: &str,
    env: &HashMap<String, String>,
    env_secrets: &HashMap<String, String>,
    secrets: Option<&(dyn SecretsStore + Send + Sync)>,
    user_id: &str,
) -> Result<HashMap<String, String>, McpFactoryError> {
    let mut resolved = env.clone();
    if env_secrets.is_empty() {
        return Ok(resolved);
    }

    let secrets = secrets.ok_or_else(|| McpFactoryError::InvalidConfig {
        name: server_name.to_string(),
        reason: format!(
            "stdio env_secrets requires a secrets store, but none is available ({} variable(s) requested)",
            env_secrets.len()
        ),
    })?;

    for (var, secret_name) in env_secrets {
        let secret =
            secrets
                .get_decrypted(user_id, secret_name)
                .await
                .map_err(|e| McpFactoryError::InvalidConfig {
                    name: server_name.to_string(),
                    reason: format!(
                        "failed to resolve secret '{secret_name}' for environment variable '{var}': {e}"
                    ),
                })?;
        if resolved
            .insert(var.clone(), secret.expose().to_string())
            .is_some()
        {
            tracing::warn!(
                "MCP server '{}': env_secrets entry overrides plain env variable '{}'",
                server_name,
                var
            );
        }
    }
    Ok(resolved)
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

    fn test_secrets_store() -> Arc<crate::secrets::InMemorySecretsStore> {
        // 32-byte test master key (same constraint as production crypto).
        let key = secrecy::SecretString::from("test-master-key-32-bytes-long!!!".to_string());
        let crypto = Arc::new(crate::secrets::SecretsCrypto::new(key).expect("test crypto"));
        Arc::new(crate::secrets::InMemorySecretsStore::new(crypto))
    }

    async fn create_secret(store: &Arc<crate::secrets::InMemorySecretsStore>, name: &str, value: &str) {
        store
            .create(
                "user",
                crate::secrets::CreateSecretParams {
                    name: name.to_string(),
                    value: secrecy::SecretString::from(value.to_string()),
                    provider: Some("test".to_string()),
                    expires_at: None,
                },
            )
            .await
            .expect("create test secret");
    }

    #[tokio::test]
    async fn test_resolve_stdio_env_passthrough_without_secret_mappings() {
        let env = HashMap::from([("PLAIN".to_string(), "value".to_string())]);
        let resolved = resolve_stdio_env("srv", &env, &HashMap::new(), None, "user")
            .await
            .expect("no mappings must not need a store");
        assert_eq!(resolved, env);
    }

    #[tokio::test]
    async fn test_resolve_stdio_env_fails_closed_without_store() {
        let mappings = HashMap::from([("TOKEN".to_string(), "my_token".to_string())]);
        let err = resolve_stdio_env("srv", &HashMap::new(), &mappings, None, "user")
            .await
            .expect_err("env_secrets without a store must fail");
        assert!(err.to_string().contains("secrets store"), "{err}");
    }

    #[tokio::test]
    async fn test_resolve_stdio_env_fails_closed_for_missing_secret() {
        let store = test_secrets_store();
        let mappings = HashMap::from([("TOKEN".to_string(), "missing_token".to_string())]);
        let err = resolve_stdio_env(
            "srv",
            &HashMap::new(),
            &mappings,
            Some(store.as_ref()),
            "user",
        )
        .await
        .expect_err("missing secret must fail closed");
        assert!(err.to_string().contains("missing_token"), "{err}");
    }

    #[tokio::test]
    async fn test_resolve_stdio_env_injects_secret_values() {
        let store = test_secrets_store();
        create_secret(&store, "github_token", "ghp_test_value").await;

        let env = HashMap::from([("PLAIN".to_string(), "value".to_string())]);
        let mappings = HashMap::from([("GITHUB_TOKEN".to_string(), "github_token".to_string())]);
        let resolved = resolve_stdio_env("srv", &env, &mappings, Some(store.as_ref()), "user")
            .await
            .expect("secret injection must succeed");

        assert_eq!(resolved.get("PLAIN").unwrap(), "value");
        assert_eq!(resolved.get("GITHUB_TOKEN").unwrap(), "ghp_test_value");
    }

    #[tokio::test]
    async fn test_resolve_stdio_env_secret_overrides_plain_env() {
        let store = test_secrets_store();
        create_secret(&store, "api_key", "secret_value").await;

        let env = HashMap::from([("API_KEY".to_string(), "plaintext".to_string())]);
        let mappings = HashMap::from([("API_KEY".to_string(), "api_key".to_string())]);
        let resolved = resolve_stdio_env("srv", &env, &mappings, Some(store.as_ref()), "user")
            .await
            .expect("secret injection must succeed");

        assert_eq!(resolved.get("API_KEY").unwrap(), "secret_value");
    }

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
