//! Factory for creating MCP clients through runtime-neutral adapters.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};

use crate::secrets::SecretsStore;
use crate::tools::ToolError;
use crate::tools::mcp::config::{
    EffectiveTransport, McpPlacement, McpServerConfig, validate_managed_study_notes_runtime,
};
use crate::tools::mcp::runtime::{
    McpRuntimeAdapter, McpRuntimeContext, McpRuntimeRegistry, McpRuntimeRequest,
};
use crate::tools::mcp::{McpClient, McpProcessManager, McpSessionManager, McpTransport};

pub use crate::tools::mcp::runtime::McpRuntimeError as McpFactoryError;

/// Create an `McpClient` with the built-in runtime adapters.
///
/// No external runtime descriptors are registered, so `transport = "external"`
/// servers fail closed with an unknown-runtime error. Composition roots that
/// load operator-declared descriptors should use
/// [`create_client_from_config_with_external_runtimes`].
pub async fn create_client_from_config(
    server: McpServerConfig,
    session_manager: &Arc<McpSessionManager>,
    process_manager: &Arc<McpProcessManager>,
    secrets: Option<Arc<dyn SecretsStore + Send + Sync>>,
    user_id: &str,
) -> Result<McpClient, McpFactoryError> {
    create_client_from_config_with_external_runtimes(
        server,
        session_manager,
        process_manager,
        secrets,
        user_id,
        Arc::new(crate::tools::mcp::descriptor::ExternalRuntimeRegistry::new()),
    )
    .await
}

/// Create an `McpClient` with the built-in runtime adapters plus the
/// operator-declared external runtime descriptors.
pub async fn create_client_from_config_with_external_runtimes(
    server: McpServerConfig,
    session_manager: &Arc<McpSessionManager>,
    process_manager: &Arc<McpProcessManager>,
    secrets: Option<Arc<dyn SecretsStore + Send + Sync>>,
    user_id: &str,
    external: Arc<crate::tools::mcp::descriptor::ExternalRuntimeRegistry>,
) -> Result<McpClient, McpFactoryError> {
    require_host_placement(&server)?;
    let registry = runtime_registry_with_external_runtimes(external);
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

/// Create a job-worker MCP client from already scoped secret environment values.
///
/// Caller must invoke this inside worker process or image so provenance resolves
/// and hashes worker-visible executable before spawning it.
pub(crate) async fn create_worker_client_from_config(
    server: McpServerConfig,
    session_manager: &Arc<McpSessionManager>,
    process_manager: &Arc<McpProcessManager>,
    worker_secret_env: HashMap<String, SecretString>,
    user_id: &str,
) -> Result<McpClient, McpFactoryError> {
    require_worker_stdio_placement(&server)?;
    validate_activation_config(&server)?;
    let context = McpRuntimeContext::new(session_manager, process_manager, None, user_id)
        .with_worker_secret_env(worker_secret_env);
    default_runtime_registry()
        .connect(McpRuntimeRequest::new(server, context))
        .await
}

/// Create a client through an explicit built-in adapter registry.
///
/// Production composition uses the closed default registry; explicit injection keeps
/// dispatch testable but is not a runtime-plugin contract.
pub async fn create_client_with_runtime_registry(
    server: McpServerConfig,
    session_manager: &Arc<McpSessionManager>,
    process_manager: &Arc<McpProcessManager>,
    secrets: Option<Arc<dyn SecretsStore + Send + Sync>>,
    user_id: &str,
    registry: &McpRuntimeRegistry,
) -> Result<McpClient, McpFactoryError> {
    require_host_placement(&server)?;
    validate_activation_config(&server)?;
    let context = McpRuntimeContext::new(session_manager, process_manager, secrets, user_id);
    registry
        .connect(McpRuntimeRequest::new(server, context))
        .await
}

fn require_host_placement(server: &McpServerConfig) -> Result<(), McpFactoryError> {
    match server.placement {
        McpPlacement::Host => Ok(()),
        McpPlacement::JobWorker => Err(McpFactoryError::InvalidConfig {
            name: server.name.clone(),
            reason: "host factory cannot activate a job-worker MCP server".to_string(),
        }),
        McpPlacement::ExternalWorker => Err(McpFactoryError::InvalidConfig {
            name: server.name.clone(),
            reason: "external-worker MCP servers are activated by the named persistent external worker, not the host factory".to_string(),
        }),
    }
}

fn require_worker_stdio_placement(server: &McpServerConfig) -> Result<(), McpFactoryError> {
    if server.placement != McpPlacement::JobWorker {
        return Err(McpFactoryError::InvalidConfig {
            name: server.name.clone(),
            reason: "worker factory requires job_worker placement".to_string(),
        });
    }
    if !matches!(
        server.effective_transport(),
        EffectiveTransport::Stdio { .. }
    ) {
        return Err(McpFactoryError::InvalidConfig {
            name: server.name.clone(),
            reason: "worker factory only supports stdio MCP servers".to_string(),
        });
    }
    Ok(())
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
    runtime_registry_with_external_runtimes(Arc::new(
        crate::tools::mcp::descriptor::ExternalRuntimeRegistry::new(),
    ))
}

/// Build the built-in runtime registry with a populated external runtime
/// descriptor registry. Composition roots that support out-of-tree MCP
/// runtimes pass their descriptors here; the default registry registers none,
/// so external transports fail closed with an unknown-runtime error.
pub fn runtime_registry_with_external_runtimes(
    external: Arc<crate::tools::mcp::descriptor::ExternalRuntimeRegistry>,
) -> McpRuntimeRegistry {
    let adapters: Vec<Arc<dyn McpRuntimeAdapter>> = vec![
        Arc::new(HttpRuntimeAdapter),
        Arc::new(StdioRuntimeAdapter),
        Arc::new(UnixRuntimeAdapter),
        Arc::new(WasmRuntimeAdapter),
        Arc::new(GatewayRuntimeAdapter),
        Arc::new(ExternalRuntimeAdapter {
            descriptors: external,
        }),
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
        let (mut server, context) = request.into_parts();
        let server_name = server.name.clone();
        let resolved_header_secrets = resolve_http_header_secrets(
            &server_name,
            &server,
            context.secrets().as_deref(),
            context.user_id(),
        )
        .await?;
        for (header_name, value) in &resolved_header_secrets {
            server
                .headers
                .insert(header_name.clone(), value.expose_secret().to_string());
        }
        // A configured header secret is the server's static credential: skip
        // the OAuth path entirely so stored tokens (including stale ones from
        // earlier auth attempts) cannot override the resolved header.
        if server.header_secrets.is_empty()
            && let Some(secrets) = context.secrets()
        {
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
        let worker_secret_env = match (server.placement, context.worker_secret_env()) {
            (McpPlacement::Host, None) => None,
            (McpPlacement::JobWorker, Some(worker_secret_env)) => Some(worker_secret_env),
            (McpPlacement::Host, Some(_)) => {
                return Err(McpFactoryError::InvalidConfig {
                    name: server_name,
                    reason: "host MCP runtime cannot receive worker secret environment".to_string(),
                });
            }
            (McpPlacement::JobWorker, None) => {
                return Err(McpFactoryError::InvalidConfig {
                    name: server_name,
                    reason: "job-worker MCP runtime requires worker-scoped environment".to_string(),
                });
            }
            (McpPlacement::ExternalWorker, _) => {
                return Err(McpFactoryError::InvalidConfig {
                    name: server_name,
                    reason: "external-worker MCP servers are activated by the named persistent external worker, not the stdio runtime".to_string(),
                });
            }
        };
        let verified_executable = if let Some(provenance) = server.provenance.clone() {
            verify_stdio_provenance(&server_name, provenance, command).await?
        } else {
            None
        };
        let resolved_env = if let Some(worker_secret_env) = worker_secret_env {
            resolve_worker_stdio_env(&server_name, env, env_secrets, worker_secret_env)?
        } else {
            resolve_stdio_env(
                &server_name,
                env,
                env_secrets,
                secrets.as_deref(),
                context.user_id(),
            )
            .await?
        };
        let env_values = resolved_env
            .iter()
            .map(|(name, value)| (name.as_str(), value.expose_secret()));
        let transport = if let Some(executable) = verified_executable {
            context
                .process_manager()
                .spawn_verified_stdio(
                    &server_name,
                    command,
                    args.to_vec(),
                    env_values,
                    !env_secrets.is_empty(),
                    server.sandbox.as_ref(),
                    executable,
                )
                .await
        } else {
            context
                .process_manager()
                .spawn_stdio(
                    &server_name,
                    command,
                    args.to_vec(),
                    env_values,
                    !env_secrets.is_empty(),
                    server.sandbox.as_ref(),
                )
                .await
        }
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

struct ExternalRuntimeAdapter {
    descriptors: Arc<crate::tools::mcp::descriptor::ExternalRuntimeRegistry>,
}

#[async_trait]
impl McpRuntimeAdapter for ExternalRuntimeAdapter {
    fn kind(&self) -> crate::tools::mcp::runtime::McpRuntimeKind {
        crate::tools::mcp::runtime::McpRuntimeKind::External
    }

    /// Activate an out-of-tree runtime by spawning its descriptor's adapter
    /// process as a stdio MCP server. The server entry's options JSON is
    /// passed in `LUNARWING_MCP_EXTERNAL_OPTIONS`; descriptors that declare
    /// `require_sandbox` get at least the mandatory baseline sandbox profile.
    async fn connect(&self, request: McpRuntimeRequest<'_>) -> Result<McpClient, McpFactoryError> {
        use crate::tools::mcp::descriptor::EXTERNAL_OPTIONS_ENV;

        let (server, context) = request.into_parts();
        let server_name = server.name.clone();
        let EffectiveTransport::External { runtime, options } = server.effective_transport() else {
            return Err(runtime_mismatch(&server_name));
        };
        let external_error = |reason: String| McpFactoryError::External {
            name: server_name.clone(),
            reason,
        };
        let descriptor = self
            .descriptors
            .get(runtime)
            .ok_or_else(|| {
                external_error(format!(
                    "no external runtime descriptor registered for '{runtime}'"
                ))
            })?
            .clone();
        let options_json =
            serde_json::to_string(options).map_err(|error| external_error(error.to_string()))?;
        if options_json.len() > crate::tools::mcp::descriptor::MAX_EXTERNAL_OPTIONS_BYTES {
            return Err(external_error(format!(
                "external runtime options exceed {} bytes",
                crate::tools::mcp::descriptor::MAX_EXTERNAL_OPTIONS_BYTES
            )));
        }

        let mut env = descriptor.env.clone();
        env.insert(EXTERNAL_OPTIONS_ENV.to_string(), options_json);
        let env_values = env
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()));

        let default_sandbox;
        let sandbox = match server.sandbox.as_ref() {
            Some(sandbox) => Some(sandbox),
            None if descriptor.require_sandbox => {
                default_sandbox = crate::tools::mcp::sandbox::McpSandboxConfig::default_profile();
                Some(&default_sandbox)
            }
            None => None,
        };
        // A pinned provenance record applies to the adapter executable the
        // descriptor spawns; verify it exactly like a stdio command.
        let verified_executable = if let Some(provenance) = server.provenance.clone() {
            verify_stdio_provenance(&server_name, provenance, &descriptor.command).await?
        } else {
            None
        };
        let spawn = if let Some(executable) = verified_executable {
            context
                .process_manager()
                .spawn_verified_stdio(
                    &server_name,
                    &descriptor.command,
                    descriptor.args.clone(),
                    env_values,
                    false,
                    sandbox,
                    executable,
                )
                .await
        } else {
            context
                .process_manager()
                .spawn_stdio(
                    &server_name,
                    &descriptor.command,
                    descriptor.args.clone(),
                    env_values,
                    false,
                    sandbox,
                )
                .await
        };
        let transport = spawn.map_err(|error| McpFactoryError::StdioSpawn {
            name: server_name.clone(),
            reason: error.to_string(),
        })?;
        Ok(McpClient::new_with_transport(
            &server_name,
            transport as Arc<dyn McpTransport>,
            None,
            context.secrets(),
            context.user_id(),
            Some(server),
        ))
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
) -> Result<Option<crate::tools::mcp::provenance::VerifiedStdioExecutable>, McpFactoryError> {
    let command = command.to_string();
    let name = server_name.to_string();
    let result = tokio::task::spawn_blocking(move || provenance.prepare_stdio_command(&command))
        .await
        .map_err(|error| McpFactoryError::Provenance {
            name: name.clone(),
            reason: format!("provenance verification task failed: {error}"),
        })?;
    let executable = match result {
        Ok(executable) => executable,
        Err(error) => {
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
    };
    tracing::debug!(server = %name, "MCP package provenance verified");
    Ok(executable)
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

/// Resolve owner-scoped HTTP header secret references for one activation.
///
/// The secret value replaces the header value entirely (store `Bearer
/// lwst_…` as the secret for an `Authorization` header). Fails closed when
/// the store is unavailable or a secret is missing for the activating owner.
async fn resolve_http_header_secrets(
    server_name: &str,
    server: &McpServerConfig,
    secrets: Option<&(dyn SecretsStore + Send + Sync)>,
    user_id: &str,
) -> Result<Vec<(String, SecretString)>, McpFactoryError> {
    let mut resolved = Vec::new();
    if server.header_secrets.is_empty() {
        return Ok(resolved);
    }
    let secrets = secrets.ok_or_else(|| McpFactoryError::InvalidConfig {
        name: server_name.to_string(),
        reason: "HTTP header secret references require an available secrets store".to_string(),
    })?;
    for (header_name, secret_name) in &server.header_secrets {
        let secret = secrets
            .get_decrypted(user_id, secret_name)
            .await
            .map_err(|error| McpFactoryError::InvalidConfig {
                name: server_name.to_string(),
                reason: format!(
                    "failed to resolve secret '{secret_name}' for HTTP header '{header_name}': {error}"
                ),
            })?;
        resolved.push((
            header_name.clone(),
            SecretString::from(secret.expose().to_string()),
        ));
    }
    Ok(resolved)
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

fn resolve_worker_stdio_env(
    server_name: &str,
    env: &HashMap<String, String>,
    env_secrets: &HashMap<String, String>,
    worker_secret_env: &HashMap<String, SecretString>,
) -> Result<Vec<(String, SecretString)>, McpFactoryError> {
    let exact_coverage = env_secrets.len() == worker_secret_env.len()
        && env_secrets
            .keys()
            .all(|name| worker_secret_env.contains_key(name));
    if !exact_coverage {
        return Err(McpFactoryError::InvalidConfig {
            name: server_name.to_string(),
            reason: "worker secret environment must exactly cover configured env_secrets names"
                .to_string(),
        });
    }

    let mut resolved = env
        .iter()
        .map(|(name, value)| (name.clone(), SecretString::from(value.clone())))
        .collect::<Vec<_>>();
    resolved.extend(
        worker_secret_env
            .iter()
            .map(|(name, value)| (name.clone(), value.clone())),
    );
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
    async fn host_factory_rejects_job_worker_before_spawn() {
        use crate::tools::mcp::provenance::{McpPackageProvenance, PackageTrust};

        let sessions = Arc::new(McpSessionManager::new());
        let processes = Arc::new(McpProcessManager::new());
        let server = McpServerConfig::new_stdio("worker", "cat", Vec::new(), HashMap::new())
            .with_provenance(McpPackageProvenance::with_trust(PackageTrust::Revoked))
            .with_placement(McpPlacement::JobWorker);

        let error =
            match create_client_from_config(server, &sessions, &processes, None, "owner").await {
                Ok(_) => panic!("host factory must reject worker placement"),
                Err(error) => error,
            };

        assert!(error.to_string().contains("host factory"));
        assert!(processes.managed_servers().await.is_empty());

        let host = McpServerConfig::new_stdio("host", "cat", Vec::new(), HashMap::new());
        let error = match create_worker_client_from_config(
            host,
            &sessions,
            &processes,
            HashMap::new(),
            "owner",
        )
        .await
        {
            Ok(_) => panic!("worker factory must reject host placement"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("job_worker placement"));
        assert!(processes.managed_servers().await.is_empty());
    }

    #[test]
    fn worker_secret_environment_requires_exact_coverage() {
        let plain = HashMap::from([("LOG_LEVEL".to_string(), "info".to_string())]);
        let refs = HashMap::from([("API_TOKEN".to_string(), "api_token".to_string())]);

        let missing = resolve_worker_stdio_env("worker", &plain, &refs, &HashMap::new())
            .expect_err("missing worker secret must fail");
        assert!(missing.to_string().contains("exactly cover"));

        let extra_values = HashMap::from([
            (
                "API_TOKEN".to_string(),
                SecretString::from("secret".to_string()),
            ),
            (
                "EXTRA".to_string(),
                SecretString::from("secret".to_string()),
            ),
        ]);
        let extra = resolve_worker_stdio_env("worker", &plain, &refs, &extra_values)
            .expect_err("extra worker secret must fail");
        assert!(extra.to_string().contains("exactly cover"));

        let values = HashMap::from([(
            "API_TOKEN".to_string(),
            SecretString::from("secret".to_string()),
        )]);
        let resolved = resolve_worker_stdio_env("worker", &plain, &refs, &values)
            .expect("exact worker secret coverage");
        let names = resolved
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            names,
            std::collections::HashSet::from(["LOG_LEVEL", "API_TOKEN"])
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn worker_factory_verifies_worker_visible_provenance_before_spawn() {
        use crate::tools::mcp::provenance::{McpPackageProvenance, ProvenanceIdentity};

        let sessions = Arc::new(McpSessionManager::new());
        let processes = Arc::new(McpProcessManager::new());
        let mut provenance =
            McpPackageProvenance::pin_for_command("cat", ProvenanceIdentity::default())
                .expect("pin cat");
        provenance.command_sha256 = Some("0".repeat(64));
        let server = McpServerConfig::new_stdio("worker", "cat", Vec::new(), HashMap::new())
            .with_provenance(provenance)
            .with_placement(McpPlacement::JobWorker);

        let error = match create_worker_client_from_config(
            server,
            &sessions,
            &processes,
            HashMap::new(),
            "owner",
        )
        .await
        {
            Ok(_) => panic!("worker-visible digest mismatch must fail"),
            Err(error) => error,
        };

        assert!(matches!(error, McpFactoryError::Provenance { .. }));
        assert!(processes.managed_servers().await.is_empty());
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
    async fn resolves_owner_scoped_http_header_secrets() {
        let store = test_secrets_store();
        store
            .create(
                "kageho",
                crate::secrets::CreateSecretParams::new(
                    "lw2study_hosted_token",
                    "Bearer lwst_value",
                ),
            )
            .await
            .expect("create secret");

        let server = McpServerConfig::new("study-notes", "http://127.0.0.1:1").with_header_secrets(
            HashMap::from([(
                "Authorization".to_string(),
                "lw2study_hosted_token".to_string(),
            )]),
        );
        let resolved =
            resolve_http_header_secrets("study-notes", &server, Some(store.as_ref()), "kageho")
                .await
                .expect("resolve header secrets");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].0, "Authorization");
        assert_eq!(resolved[0].1.expose_secret(), "Bearer lwst_value");
    }

    #[tokio::test]
    async fn http_header_secrets_fail_closed() {
        let server = McpServerConfig::new("study-notes", "http://127.0.0.1:1").with_header_secrets(
            HashMap::from([(
                "Authorization".to_string(),
                "lw2study_hosted_token".to_string(),
            )]),
        );

        let no_store = resolve_http_header_secrets("study-notes", &server, None, "kageho")
            .await
            .expect_err("missing store must fail");
        assert!(no_store.to_string().contains("secrets store"));

        let store = test_secrets_store();
        let missing =
            resolve_http_header_secrets("study-notes", &server, Some(store.as_ref()), "kageho")
                .await
                .expect_err("missing secret must fail");
        assert!(missing.to_string().contains("lw2study_hosted_token"));

        store
            .create(
                "other-owner",
                crate::secrets::CreateSecretParams::new(
                    "lw2study_hosted_token",
                    "Bearer lwst_other",
                ),
            )
            .await
            .expect("create other-owner secret");
        let wrong_owner =
            resolve_http_header_secrets("study-notes", &server, Some(store.as_ref()), "kageho")
                .await
                .expect_err("wrong-owner secret must fail");
        assert!(wrong_owner.to_string().contains("lw2study_hosted_token"));
    }

    #[tokio::test]
    async fn http_header_secret_reaches_wire_and_skips_stored_auth() {
        use axum::{Router, extract::Request, routing::post};
        use std::sync::Mutex as StdMutex;
        use tokio::net::TcpListener;

        let captured: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));
        let captured_for_handler = Arc::clone(&captured);
        let app = Router::new().route(
            "/",
            post(move |req: Request| {
                let captured = Arc::clone(&captured_for_handler);
                async move {
                    *captured.lock().unwrap() = req
                        .headers()
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_string);
                    let body_value = axum::body::to_bytes(req.into_body(), 1 << 20)
                        .await
                        .ok()
                        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
                    let id = body_value
                        .as_ref()
                        .and_then(|body| body.get("id").cloned())
                        .unwrap_or(serde_json::Value::Null);
                    let method = body_value
                        .as_ref()
                        .and_then(|body| body.get("method"))
                        .and_then(|method| method.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let result = if method == "tools/list" {
                        serde_json::json!({"tools": []})
                    } else {
                        serde_json::json!({
                            "protocolVersion": "2024-11-05",
                            "capabilities": {},
                            "serverInfo": {"name": "echo", "version": "0"},
                        })
                    };
                    axum::response::Json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": result,
                    }))
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let store = test_secrets_store();
        store
            .create(
                "kageho",
                crate::secrets::CreateSecretParams::new(
                    "lw2study_hosted_token",
                    "Bearer lwst_value",
                ),
            )
            .await
            .expect("create secret");

        let server = McpServerConfig::new("study-notes", format!("http://{address}"))
            .with_header_secrets(HashMap::from([(
                "Authorization".to_string(),
                "lw2study_hosted_token".to_string(),
            )]));
        let sessions = Arc::new(McpSessionManager::new());
        let processes = Arc::new(McpProcessManager::new());
        let client = create_client_from_config(
            server,
            &sessions,
            &processes,
            Some(Arc::clone(&store) as Arc<dyn SecretsStore + Send + Sync>),
            "kageho",
        )
        .await
        .expect("activation with header secrets succeeds");
        client.test_connection().await.expect("connection succeeds");

        let captured_value = captured.lock().unwrap().clone();
        assert_eq!(
            captured_value.as_deref(),
            Some("Bearer lwst_value"),
            "resolved secret must reach the wire as the Authorization header"
        );
        client.shutdown().await.expect("shutdown client");
        server_handle.abort();
    }

    #[cfg(target_os = "linux")]
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
