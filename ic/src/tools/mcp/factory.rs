//! Factory for creating MCP clients through runtime-neutral adapters.

use std::sync::Arc;

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};

use crate::secrets::SecretsStore;
use crate::tools::ToolError;
use crate::tools::mcp::config::{
    EffectiveTransport, McpServerConfig, validate_managed_study_notes_runtime,
};
use crate::tools::mcp::runtime::{
    McpRuntimeAdapter, McpRuntimeContext, McpRuntimeRegistry, McpRuntimeRequest,
};
use crate::tools::mcp::{McpClient, McpProcessManager, McpSessionManager, McpTransport};

pub use crate::tools::mcp::runtime::McpRuntimeError as McpFactoryError;

/// Create an `McpClient` with the built-in runtime adapters.
pub async fn create_client_from_config(
    server: McpServerConfig,
    session_manager: &Arc<McpSessionManager>,
    process_manager: &Arc<McpProcessManager>,
    secrets: Option<Arc<dyn SecretsStore + Send + Sync>>,
    user_id: &str,
) -> Result<McpClient, McpFactoryError> {
    let registry = default_runtime_registry();
    create_client_with_runtime_registry(
        server,
        session_manager,
        process_manager,
        secrets,
        user_id,
        &registry,
    )
    .await
}

/// Create a client through an explicit adapter registry.
pub async fn create_client_with_runtime_registry(
    server: McpServerConfig,
    session_manager: &Arc<McpSessionManager>,
    process_manager: &Arc<McpProcessManager>,
    secrets: Option<Arc<dyn SecretsStore + Send + Sync>>,
    user_id: &str,
    registry: &McpRuntimeRegistry,
) -> Result<McpClient, McpFactoryError> {
    validate_activation_config(&server)?;
    let context = McpRuntimeContext::new(session_manager, process_manager, secrets, user_id);
    registry
        .connect(McpRuntimeRequest::new(server, context))
        .await
}

fn validate_activation_config(server: &McpServerConfig) -> Result<(), McpFactoryError> {
    let server_name = server.name.clone();
    server
        .validate()
        .map_err(|error| McpFactoryError::InvalidConfig {
            name: server_name.clone(),
            reason: error.to_string(),
        })?;
    validate_managed_study_notes_runtime(server).map_err(|error| {
        McpFactoryError::InvalidConfig {
            name: server_name.clone(),
            reason: error.to_string(),
        }
    })?;
    if let Some(provenance) = &server.provenance {
        provenance
            .verify_not_revoked()
            .map_err(|error| McpFactoryError::Provenance {
                name: server_name,
                reason: error.to_string(),
            })?;
    }
    Ok(())
}

fn default_runtime_registry() -> McpRuntimeRegistry {
    let adapters: Vec<Arc<dyn McpRuntimeAdapter>> = vec![
        Arc::new(HttpRuntimeAdapter),
        Arc::new(StdioRuntimeAdapter),
        Arc::new(UnixRuntimeAdapter),
        Arc::new(WasmRuntimeAdapter),
        Arc::new(GatewayRuntimeAdapter),
    ];
    McpRuntimeRegistry::new(adapters)
}

struct HttpRuntimeAdapter;
struct StdioRuntimeAdapter;
struct UnixRuntimeAdapter;
struct WasmRuntimeAdapter;
struct GatewayRuntimeAdapter;

#[async_trait]
impl McpRuntimeAdapter for HttpRuntimeAdapter {
    fn kind(&self) -> crate::tools::mcp::runtime::McpRuntimeKind {
        crate::tools::mcp::runtime::McpRuntimeKind::Http
    }

    async fn connect(&self, request: McpRuntimeRequest<'_>) -> Result<McpClient, McpFactoryError> {
        let (server, context) = request.into_parts();
        let server_name = server.name.clone();
        if let Some(secrets) = context.secrets() {
            let has_tokens =
                crate::tools::mcp::is_authenticated(&server, &secrets, context.user_id()).await;
            if has_tokens || server.requires_auth() {
                return Ok(McpClient::new_authenticated(
                    server,
                    Arc::clone(context.session_manager()),
                    secrets,
                    context.user_id(),
                ));
            }
        }
        McpClient::new_with_config_and_session_manager(
            server,
            Arc::clone(context.session_manager()),
            context.user_id(),
        )
        .map_err(|error| McpFactoryError::InvalidConfig {
            name: server_name,
            reason: error.to_string(),
        })
    }
}

#[async_trait]
impl McpRuntimeAdapter for StdioRuntimeAdapter {
    fn kind(&self) -> crate::tools::mcp::runtime::McpRuntimeKind {
        crate::tools::mcp::runtime::McpRuntimeKind::Stdio
    }

    async fn connect(&self, request: McpRuntimeRequest<'_>) -> Result<McpClient, McpFactoryError> {
        let (server, context) = request.into_parts();
        let server_name = server.name.clone();
        let secrets = context.secrets();
        let EffectiveTransport::Stdio {
            command,
            args,
            env,
            env_secrets,
        } = server.effective_transport()
        else {
            return Err(runtime_mismatch(&server_name));
        };
        if let Some(provenance) = server.provenance.clone() {
            verify_stdio_provenance(&server_name, provenance, command).await?;
        }
        let resolved_env = resolve_stdio_env(
            &server_name,
            env,
            env_secrets,
            secrets.as_deref(),
            context.user_id(),
        )
        .await?;
        let transport = context
            .process_manager()
            .spawn_stdio(
                &server_name,
                command,
                args.to_vec(),
                resolved_env
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.expose_secret())),
                !env_secrets.is_empty(),
                server.sandbox.as_ref(),
            )
            .await
            .map_err(|error| McpFactoryError::StdioSpawn {
                name: server_name.clone(),
                reason: error.to_string(),
            })?;
        Ok(McpClient::new_with_transport(
            &server_name,
            transport as Arc<dyn McpTransport>,
            None,
            secrets,
            context.user_id(),
            Some(server),
        ))
    }
}

#[async_trait]
impl McpRuntimeAdapter for UnixRuntimeAdapter {
    fn kind(&self) -> crate::tools::mcp::runtime::McpRuntimeKind {
        crate::tools::mcp::runtime::McpRuntimeKind::Unix
    }

    async fn connect(&self, request: McpRuntimeRequest<'_>) -> Result<McpClient, McpFactoryError> {
        let (server, context) = request.into_parts();
        let server_name = server.name.clone();
        let EffectiveTransport::Unix { socket_path } = server.effective_transport() else {
            return Err(runtime_mismatch(&server_name));
        };
        #[cfg(unix)]
        {
            let transport = crate::tools::mcp::unix_transport::UnixMcpTransport::connect(
                &server_name,
                socket_path,
            )
            .await
            .map_err(|error| McpFactoryError::UnixConnect {
                name: server_name.clone(),
                reason: error.to_string(),
            })?;
            Ok(McpClient::new_with_transport(
                &server_name,
                Arc::new(transport) as Arc<dyn McpTransport>,
                None,
                context.secrets(),
                context.user_id(),
                Some(server),
            ))
        }
        #[cfg(not(unix))]
        {
            let _ = socket_path;
            Err(McpFactoryError::UnixNotSupported { name: server_name })
        }
    }
}

#[async_trait]
impl McpRuntimeAdapter for WasmRuntimeAdapter {
    fn kind(&self) -> crate::tools::mcp::runtime::McpRuntimeKind {
        crate::tools::mcp::runtime::McpRuntimeKind::Wasm
    }

    async fn connect(&self, request: McpRuntimeRequest<'_>) -> Result<McpClient, McpFactoryError> {
        let (server, context) = request.into_parts();
        let server_name = server.name.clone();
        let (component_path, memory_mb, fuel, timeout_seconds) = match server.effective_transport()
        {
            EffectiveTransport::Wasm {
                component_path,
                memory_mb,
                fuel,
                timeout_seconds,
            } => (
                component_path.to_path_buf(),
                memory_mb,
                fuel,
                timeout_seconds,
            ),
            _ => return Err(runtime_mismatch(&server_name)),
        };
        let expected_component_sha256 = server
            .provenance
            .as_ref()
            .filter(|provenance| {
                provenance.trust == crate::tools::mcp::provenance::PackageTrust::Pinned
            })
            .and_then(|provenance| provenance.component_sha256.clone());
        if let Some(provenance) = server.provenance.clone() {
            verify_wasm_provenance(&server_name, provenance, component_path.clone()).await?;
        }
        let transport = crate::tools::mcp::wasm_transport::WasmMcpTransport::load(
            &server_name,
            &component_path,
            memory_mb,
            fuel,
            timeout_seconds,
            expected_component_sha256,
        )
        .await
        .map_err(|error| McpFactoryError::WasmLoad {
            name: server_name.clone(),
            reason: error.to_string(),
        })?;
        Ok(McpClient::new_with_transport(
            &server_name,
            Arc::new(transport),
            None,
            context.secrets(),
            context.user_id(),
            Some(server),
        ))
    }
}

#[async_trait]
impl McpRuntimeAdapter for GatewayRuntimeAdapter {
    fn kind(&self) -> crate::tools::mcp::runtime::McpRuntimeKind {
        crate::tools::mcp::runtime::McpRuntimeKind::Gateway
    }

    async fn connect(&self, request: McpRuntimeRequest<'_>) -> Result<McpClient, McpFactoryError> {
        let (server, context) = request.into_parts();
        let server_name = server.name.clone();
        let (mode, route) = match server.effective_transport() {
            EffectiveTransport::Gateway { mode, route } => (mode, route.map(str::to_string)),
            _ => return Err(runtime_mismatch(&server_name)),
        };
        let transport = crate::tools::mcp::gateway_transport::GatewayMcpTransport::new(
            server.url.clone(),
            &server_name,
            mode,
            route,
            Arc::clone(context.session_manager()),
        );
        Ok(McpClient::new_with_transport(
            &server_name,
            Arc::new(transport),
            Some(Arc::clone(context.session_manager())),
            context.secrets(),
            context.user_id(),
            Some(server),
        ))
    }
}

fn runtime_mismatch(name: &str) -> McpFactoryError {
    McpFactoryError::InvalidConfig {
        name: name.to_string(),
        reason: "runtime adapter received a mismatched transport".to_string(),
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

/// Verify a pinned component digest off the async executor before instantiation.
async fn verify_wasm_provenance(
    server_name: &str,
    provenance: crate::tools::mcp::provenance::McpPackageProvenance,
    component_path: std::path::PathBuf,
) -> Result<(), McpFactoryError> {
    let name = server_name.to_string();
    let result =
        tokio::task::spawn_blocking(move || provenance.verify_wasm_component(&component_path))
            .await
            .map_err(|error| McpFactoryError::Provenance {
                name: name.clone(),
                reason: format!("provenance verification task failed: {error}"),
            })?;
    result.map_err(|error| McpFactoryError::Provenance {
        name,
        reason: error.to_string(),
    })
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
        match tokio::time::timeout(std::time::Duration::from_secs(30), client.shutdown()).await {
            Ok(result) => result.err(),
            Err(_) => Some(ToolError::ExternalService(format!(
                "Timed out stopping MCP server '{server_name}' transport"
            ))),
        }
    } else {
        None
    };
    let process_error = match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        process_manager.shutdown(server_name),
    )
    .await
    {
        Ok(result) => result.err(),
        Err(_) => Some(ToolError::ExternalService(format!(
            "Timed out stopping MCP server '{server_name}' process"
        ))),
    };
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

    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_activation_applies_sandbox_policy() {
        use crate::tools::mcp::sandbox::McpSandboxConfig;

        let session_manager = Arc::new(McpSessionManager::new());
        let process_manager = Arc::new(McpProcessManager::new());

        // A limit above the daemon's hard limit fails before any child spawns.
        let config = McpServerConfig::new_stdio(
            "over-limited",
            "cat",
            Vec::new(),
            std::collections::HashMap::new(),
        )
        .with_sandbox(McpSandboxConfig {
            max_file_descriptors: Some(u64::MAX),
            ..McpSandboxConfig::default()
        });
        let error = match create_client_from_config(
            config,
            &session_manager,
            &process_manager,
            None,
            "test-user",
        )
        .await
        {
            Ok(_) => panic!("impossible sandbox limit must not activate"),
            Err(error) => error,
        };
        assert!(matches!(error, McpFactoryError::StdioSpawn { .. }));
        assert!(
            process_manager.managed_servers().await.is_empty(),
            "failed sandbox preparation must not spawn a child"
        );

        // A sane policy activates and the child observes the limit.
        let config = McpServerConfig::new_stdio(
            "sandboxed",
            "sh",
            vec![
                "-c".to_string(),
                "test \"$(ulimit -v)\" = 65536".to_string(),
            ],
            std::collections::HashMap::new(),
        )
        .with_sandbox(McpSandboxConfig {
            max_memory_mb: Some(64),
            ..McpSandboxConfig::default()
        });
        let client = create_client_from_config(
            config,
            &session_manager,
            &process_manager,
            None,
            "test-user",
        )
        .await
        .expect("sandboxed server activates");

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(crate::tools::mcp::McpProcessState::Exited(status)) =
                    process_manager.state("sandboxed").await
                {
                    assert!(status.success(), "child must observe the memory limit");
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("sandboxed child did not exit in time");

        client.shutdown().await.expect("shutdown sandboxed client");
        process_manager
            .shutdown("sandboxed")
            .await
            .expect("clean up sandboxed process");
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
