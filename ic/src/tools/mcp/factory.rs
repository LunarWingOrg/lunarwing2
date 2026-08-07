//! Factory for creating MCP clients from server configuration.
//!
//! Encapsulates the transport dispatch logic (stdio, Unix socket, HTTP)
//! so that callers don't need to match on `EffectiveTransport` themselves.

use std::sync::Arc;

use secrecy::{ExposeSecret, SecretString};

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
    #[error("Package provenance check failed for MCP server '{name}': {reason}")]
    Provenance { name: String, reason: String },
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
    server
        .validate()
        .map_err(|error| McpFactoryError::InvalidConfig {
            name: server_name.clone(),
            reason: error.to_string(),
        })?;
    validate_managed_study_notes_runtime(&server).map_err(|error| {
        McpFactoryError::InvalidConfig {
            name: server_name.clone(),
            reason: error.to_string(),
        }
    })?;

    // Revocation applies to every transport; digest pinning is enforced for
    // stdio before the child is spawned.
    if let Some(provenance) = &server.provenance {
        provenance
            .verify_not_revoked()
            .map_err(|error| McpFactoryError::Provenance {
                name: server_name.clone(),
                reason: error.to_string(),
            })?;
    }

    match server.effective_transport() {
        EffectiveTransport::Stdio {
            command,
            args,
            env,
            env_secrets,
        } => {
            if let Some(provenance) = server.provenance.clone() {
                verify_stdio_provenance(&server_name, provenance, command).await?;
            }
            let resolved_env =
                resolve_stdio_env(&server_name, env, env_secrets, secrets.as_deref(), user_id)
                    .await?;
            let transport = process_manager
                .spawn_stdio(
                    &server_name,
                    command,
                    args.to_vec(),
                    resolved_env
                        .iter()
                        .map(|(name, value)| (name.as_str(), value.expose_secret())),
                    !env_secrets.is_empty(),
                )
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

/// Verify a pinned stdio executable digest off the async executor.
///
/// Hashing is blocking filesystem work, so it runs on the blocking thread
/// pool; a failure prevents the child process from being spawned at all.
async fn verify_stdio_provenance(
    server_name: &str,
    provenance: crate::tools::mcp::provenance::McpPackageProvenance,
    command: &str,
) -> Result<(), McpFactoryError> {
    let command = command.to_string();
    let name = server_name.to_string();
    let result = tokio::task::spawn_blocking(move || provenance.verify_stdio_command(&command))
        .await
        .map_err(|error| McpFactoryError::Provenance {
            name: name.clone(),
            reason: format!("provenance verification task failed: {error}"),
        })?;
    if let Err(error) = result {
        tracing::warn!(
            server = %name,
            error = %error,
            "MCP package provenance verification failed; activation blocked"
        );
        return Err(McpFactoryError::Provenance {
            name,
            reason: error.to_string(),
        });
    }
    tracing::debug!(server = %name, "MCP package provenance verified");
    Ok(())
}

/// Resolve configured values and owner-scoped secret references for one spawn.
/// Resolver-owned secret strings are zeroized when client creation finishes.
async fn resolve_stdio_env(
    server_name: &str,
    env: &std::collections::HashMap<String, String>,
    env_secrets: &std::collections::HashMap<String, String>,
    secrets: Option<&(dyn SecretsStore + Send + Sync)>,
    user_id: &str,
) -> Result<Vec<(String, SecretString)>, McpFactoryError> {
    let mut resolved = env
        .iter()
        .map(|(name, value)| (name.clone(), SecretString::from(value.clone())))
        .collect::<Vec<_>>();

    if env_secrets.is_empty() {
        return Ok(resolved);
    }

    let secrets = secrets.ok_or_else(|| McpFactoryError::InvalidConfig {
        name: server_name.to_string(),
        reason: "secret environment references require an available secrets store".to_string(),
    })?;

    for (env_name, secret_name) in env_secrets {
        let secret = secrets
            .get_decrypted(user_id, secret_name)
            .await
            .map_err(|error| McpFactoryError::InvalidConfig {
                name: server_name.to_string(),
                reason: format!(
                    "failed to resolve secret '{secret_name}' for environment variable '{env_name}': {error}"
                ),
            })?;
        resolved.push((
            env_name.clone(),
            SecretString::from(secret.expose().to_string()),
        ));
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
        let key = secrecy::SecretString::from(crate::secrets::keychain::generate_master_key_hex());
        let crypto = Arc::new(crate::secrets::SecretsCrypto::new(key).expect("test crypto"));
        Arc::new(crate::secrets::InMemorySecretsStore::new(crypto))
    }

    #[tokio::test]
    async fn resolves_owner_scoped_stdio_secret_environment() {
        let store = test_secrets_store();
        store
            .create(
                "owner-a",
                crate::secrets::CreateSecretParams::new("api_token", "secret-value"),
            )
            .await
            .expect("create secret");

        let env = std::collections::HashMap::from([("LOG_LEVEL".to_string(), "warn".to_string())]);
        let refs =
            std::collections::HashMap::from([("API_TOKEN".to_string(), "api_token".to_string())]);
        let resolved = resolve_stdio_env("server", &env, &refs, Some(store.as_ref()), "owner-a")
            .await
            .expect("resolve secret environment");

        let resolved = resolved
            .iter()
            .map(|(name, value)| (name.as_str(), value.expose_secret()))
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(resolved.get("LOG_LEVEL"), Some(&"warn"));
        assert_eq!(resolved.get("API_TOKEN"), Some(&"secret-value"));
    }

    #[tokio::test]
    async fn stdio_secret_environment_fails_closed() {
        let refs =
            std::collections::HashMap::from([("API_TOKEN".to_string(), "api_token".to_string())]);
        let no_store = resolve_stdio_env(
            "server",
            &std::collections::HashMap::new(),
            &refs,
            None,
            "owner-a",
        )
        .await
        .expect_err("missing store must fail");
        assert!(no_store.to_string().contains("secrets store"));

        let store = test_secrets_store();
        store
            .create(
                "owner-a",
                crate::secrets::CreateSecretParams::new("api_token", "owner-a-value"),
            )
            .await
            .expect("create owner-a secret");
        let wrong_owner = resolve_stdio_env(
            "server",
            &std::collections::HashMap::new(),
            &refs,
            Some(store.as_ref()),
            "owner-b",
        )
        .await
        .expect_err("missing owner secret must fail");
        assert!(wrong_owner.to_string().contains("api_token"));
    }

    #[tokio::test]
    async fn stdio_activation_enforces_package_provenance() {
        use crate::tools::mcp::provenance::{
            McpPackageProvenance, PackageTrust, ProvenanceIdentity,
        };

        let session_manager = Arc::new(McpSessionManager::new());
        let process_manager = Arc::new(McpProcessManager::new());

        // Revoked packages fail closed before any child is spawned.
        let mut revoked =
            McpPackageProvenance::pin_for_command("cat", ProvenanceIdentity::default())
                .expect("pin cat");
        revoked.trust = PackageTrust::Revoked;
        let config = McpServerConfig::new_stdio(
            "revoked-server",
            "cat",
            Vec::new(),
            std::collections::HashMap::new(),
        )
        .with_provenance(revoked);
        let error = match create_client_from_config(
            config,
            &session_manager,
            &process_manager,
            None,
            "test-user",
        )
        .await
        {
            Ok(_) => panic!("revoked package must not activate"),
            Err(error) => error,
        };
        assert!(matches!(error, McpFactoryError::Provenance { .. }));
        assert!(
            process_manager.managed_servers().await.is_empty(),
            "revoked activation must not spawn a child"
        );

        // A doctored digest fails closed as well.
        let mut mismatched =
            McpPackageProvenance::pin_for_command("cat", ProvenanceIdentity::default())
                .expect("pin cat");
        mismatched.command_sha256 = Some("0".repeat(64));
        let config = McpServerConfig::new_stdio(
            "mismatched-server",
            "cat",
            Vec::new(),
            std::collections::HashMap::new(),
        )
        .with_provenance(mismatched);
        let error = match create_client_from_config(
            config,
            &session_manager,
            &process_manager,
            None,
            "test-user",
        )
        .await
        {
            Ok(_) => panic!("digest mismatch must not activate"),
            Err(error) => error,
        };
        assert!(matches!(error, McpFactoryError::Provenance { .. }));
        assert!(process_manager.managed_servers().await.is_empty());

        // A correct pin activates and spawns normally.
        let pinned = McpPackageProvenance::pin_for_command("cat", ProvenanceIdentity::default())
            .expect("pin cat");
        let config = McpServerConfig::new_stdio(
            "pinned-server",
            "cat",
            Vec::new(),
            std::collections::HashMap::new(),
        )
        .with_provenance(pinned);
        let client = create_client_from_config(
            config,
            &session_manager,
            &process_manager,
            None,
            "test-user",
        )
        .await
        .expect("pinned package with matching digest activates");
        client.shutdown().await.expect("shutdown pinned client");
        process_manager
            .shutdown("pinned-server")
            .await
            .expect("clean up pinned process");
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
