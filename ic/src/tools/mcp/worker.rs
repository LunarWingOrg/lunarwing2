//! Job-worker placement contract for stdio MCP servers.
//!
//! The orchestrator converts an installed, owner-scoped MCP configuration into a
//! bounded descriptor only after a sandbox job explicitly selects it. Secret names
//! and values are not serialized in the descriptor. The worker receives credential
//! values through the existing job-scoped credential endpoint and injects only the
//! environment variables declared by this descriptor.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use crate::tools::mcp::config::{ConfigError, EffectiveTransport, McpPlacement, McpServerConfig};
use crate::tools::mcp::provenance::McpPackageProvenance;
use crate::tools::mcp::{McpClient, McpProcessManager, McpSessionManager, McpTransport};
use crate::tools::tool::ToolError;

/// Maximum MCP runtimes a single sandbox job may request.
pub const MAX_WORKER_MCP_SERVERS: usize = 16;
const MAX_WORKER_MCP_ARGS: usize = 128;
const MAX_WORKER_MCP_ENV: usize = 64;
const MAX_WORKER_MCP_APPROVAL_TOOLS: usize = 256;
const MAX_DESCRIPTOR_NAME_BYTES: usize = 128;
const MAX_DESCRIPTOR_ITEM_BYTES: usize = 16 * 1024;
const MAX_DESCRIPTOR_ENV_BYTES: usize = 128 * 1024;

/// Resolved, secret-free MCP runtime descriptor delivered to one job worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerMcpServerDescriptor {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Job credential environment names this MCP child may receive.
    #[serde(default)]
    pub credential_env: Vec<String>,
    #[serde(default)]
    pub always_require_approval: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<McpPackageProvenance>,
}

/// Resolve an ordered, explicit list of installed worker-local MCP servers.
pub fn select_worker_mcp_servers(
    names: &[String],
    servers: &crate::tools::mcp::McpServersFile,
    granted_secrets: &HashMap<String, String>,
) -> Result<Vec<WorkerMcpServerDescriptor>, ConfigError> {
    if names.len() > MAX_WORKER_MCP_SERVERS {
        return Err(invalid(format!(
            "too many worker MCP servers requested (max {MAX_WORKER_MCP_SERVERS})"
        )));
    }

    let mut seen = HashSet::new();
    let mut descriptors = Vec::with_capacity(names.len());
    for name in names {
        validate_text("server name", name, MAX_DESCRIPTOR_NAME_BYTES)?;
        if !seen.insert(name.as_str()) {
            return Err(invalid(format!(
                "worker MCP server '{name}' was requested more than once"
            )));
        }
        let config = servers.get(name).ok_or_else(|| {
            invalid(format!(
                "worker MCP server '{name}' is not installed for this owner"
            ))
        })?;
        descriptors.push(WorkerMcpServerDescriptor::from_config(
            config,
            granted_secrets,
        )?);
    }
    Ok(descriptors)
}

impl WorkerMcpServerDescriptor {
    /// Resolve an installed worker-local config against explicit per-job grants.
    ///
    /// `granted_secrets` maps environment variable name to secret name. Every
    /// configured secret reference must have an exact matching job grant.
    pub fn from_config(
        config: &McpServerConfig,
        granted_secrets: &HashMap<String, String>,
    ) -> Result<Self, ConfigError> {
        config.validate()?;
        if !config.is_worker_local() {
            return Err(invalid(format!(
                "MCP server '{}' is not an enabled job-worker runtime",
                config.name
            )));
        }
        let EffectiveTransport::Stdio {
            command,
            args,
            env,
            env_secrets,
        } = config.effective_transport()
        else {
            return Err(invalid("job-worker placement requires stdio transport"));
        };

        let mut credential_env = Vec::with_capacity(env_secrets.len());
        for (env_name, secret_name) in env_secrets {
            match granted_secrets.get(env_name) {
                Some(granted) if granted == secret_name => credential_env.push(env_name.clone()),
                _ => {
                    return Err(invalid(format!(
                        "MCP server '{}' requires job credential '{}' mapped from secret '{}'",
                        config.name, env_name, secret_name
                    )));
                }
            }
        }
        credential_env.sort_unstable();

        let descriptor = Self {
            name: config.name.clone(),
            command: command.to_string(),
            args: args.to_vec(),
            env: env.clone(),
            credential_env,
            always_require_approval: config.always_require_approval.clone(),
            provenance: config.provenance.clone(),
        };
        descriptor.validate()?;
        Ok(descriptor)
    }

    /// Validate the authenticated wire descriptor again inside the worker.
    pub fn validate(&self) -> Result<(), ConfigError> {
        validate_text("name", &self.name, MAX_DESCRIPTOR_NAME_BYTES)?;
        validate_text("command", &self.command, MAX_DESCRIPTOR_ITEM_BYTES)?;
        if self.args.len() > MAX_WORKER_MCP_ARGS {
            return Err(invalid(format!(
                "worker MCP server '{}' has too many arguments (max {MAX_WORKER_MCP_ARGS})",
                self.name
            )));
        }
        for arg in &self.args {
            validate_text("argument", arg, MAX_DESCRIPTOR_ITEM_BYTES)?;
        }
        if self.env.len() > MAX_WORKER_MCP_ENV {
            return Err(invalid(format!(
                "worker MCP server '{}' has too many environment values (max {MAX_WORKER_MCP_ENV})",
                self.name
            )));
        }
        let env_bytes = self
            .env
            .iter()
            .map(|(name, value)| name.len().saturating_add(value.len()))
            .sum::<usize>();
        if env_bytes > MAX_DESCRIPTOR_ENV_BYTES {
            return Err(invalid(format!(
                "worker MCP server '{}' environment exceeds {MAX_DESCRIPTOR_ENV_BYTES} bytes",
                self.name
            )));
        }
        validate_environment(&self.env, &self.credential_env)?;
        if self.always_require_approval.len() > MAX_WORKER_MCP_APPROVAL_TOOLS {
            return Err(invalid(format!(
                "worker MCP server '{}' has too many approval-gated tools",
                self.name
            )));
        }
        for tool in &self.always_require_approval {
            validate_text("approval-required tool", tool, MAX_DESCRIPTOR_ITEM_BYTES)?;
        }
        reject_runtime_package_launcher(&self.command)?;
        if let Some(provenance) = &self.provenance {
            provenance.validate()?;
            provenance
                .verify_not_revoked()
                .map_err(|error| invalid(error.to_string()))?;
        }
        Ok(())
    }

    fn sanitized_config(&self) -> McpServerConfig {
        let mut config = McpServerConfig::new_stdio(
            &self.name,
            &self.command,
            self.args.clone(),
            self.env.clone(),
        )
        .with_placement(McpPlacement::JobWorker)
        .with_always_require_approval(self.always_require_approval.clone());
        config.provenance = self.provenance.clone();
        config
    }
}

/// Start one descriptor inside a job worker and return a normal MCP client.
pub async fn create_worker_mcp_client(
    descriptor: &WorkerMcpServerDescriptor,
    credentials: &HashMap<String, String>,
    session_manager: &Arc<McpSessionManager>,
    process_manager: &Arc<McpProcessManager>,
    user_id: &str,
) -> Result<McpClient, ToolError> {
    descriptor
        .validate()
        .map_err(|error| ToolError::InvalidParameters(error.to_string()))?;
    verify_worker_provenance(descriptor).await?;

    let mut child_env = descriptor
        .env
        .iter()
        .map(|(name, value)| (name.clone(), SecretString::from(value.clone())))
        .collect::<Vec<_>>();
    for env_name in &descriptor.credential_env {
        let value = credentials.get(env_name).ok_or_else(|| {
            ToolError::NotAuthorized(format!(
                "worker MCP server '{}' is missing granted credential '{}'",
                descriptor.name, env_name
            ))
        })?;
        child_env.push((env_name.clone(), SecretString::from(value.clone())));
    }

    let transport = process_manager
        .spawn_stdio(
            &descriptor.name,
            &descriptor.command,
            descriptor.args.clone(),
            child_env
                .iter()
                .map(|(name, value)| (name.as_str(), value.expose_secret())),
            !descriptor.credential_env.is_empty(),
            None,
        )
        .await?;
    let config = descriptor.sanitized_config();
    Ok(McpClient::new_with_transport(
        &descriptor.name,
        transport as Arc<dyn McpTransport>,
        Some(Arc::clone(session_manager)),
        None,
        user_id,
        Some(config),
    ))
}

async fn verify_worker_provenance(descriptor: &WorkerMcpServerDescriptor) -> Result<(), ToolError> {
    let Some(provenance) = descriptor.provenance.clone() else {
        return Ok(());
    };
    let command = descriptor.command.clone();
    tokio::task::spawn_blocking(move || provenance.verify_stdio_command(&command))
        .await
        .map_err(|error| {
            ToolError::ExternalService(format!(
                "worker MCP provenance task failed for '{}': {error}",
                descriptor.name
            ))
        })?
        .map_err(|error| {
            ToolError::NotAuthorized(format!(
                "worker MCP provenance check failed for '{}': {error}",
                descriptor.name
            ))
        })
}

fn validate_environment(
    env: &HashMap<String, String>,
    credential_env: &[String],
) -> Result<(), ConfigError> {
    let mut names = HashSet::new();
    for (name, value) in env {
        validate_env_name(name)?;
        if value.contains('\0') {
            return Err(invalid(format!(
                "worker MCP environment variable '{name}' contains a NUL byte"
            )));
        }
        names.insert(canonical_env_name(name));
    }
    if credential_env.len() > MAX_WORKER_MCP_ENV {
        return Err(invalid(format!(
            "worker MCP descriptor has too many credential environment values (max {MAX_WORKER_MCP_ENV})"
        )));
    }
    for name in credential_env {
        validate_env_name(name)?;
        if !names.insert(canonical_env_name(name)) {
            return Err(invalid(format!(
                "worker MCP environment variable '{name}' is duplicated or has both plain and credential values"
            )));
        }
    }
    Ok(())
}

fn validate_env_name(name: &str) -> Result<(), ConfigError> {
    if name.is_empty() || name.contains(['=', '\0']) {
        return Err(invalid(format!(
            "invalid worker MCP environment variable name '{name}'"
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn canonical_env_name(name: &str) -> String {
    name.to_ascii_uppercase()
}

#[cfg(not(windows))]
fn canonical_env_name(name: &str) -> String {
    name.to_string()
}

fn validate_text(field: &str, value: &str, max_bytes: usize) -> Result<(), ConfigError> {
    if value.is_empty() || value.contains('\0') || value.len() > max_bytes {
        return Err(invalid(format!(
            "worker MCP {field} must contain 1..={max_bytes} bytes and no NUL"
        )));
    }
    Ok(())
}

fn reject_runtime_package_launcher(command: &str) -> Result<(), ConfigError> {
    let launcher = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
        .to_ascii_lowercase();
    let launcher = [".exe", ".cmd", ".bat"]
        .into_iter()
        .find_map(|suffix| launcher.strip_suffix(suffix))
        .unwrap_or(&launcher);
    if matches!(
        launcher,
        "npm"
            | "npx"
            | "pnpm"
            | "yarn"
            | "yarnpkg"
            | "pip"
            | "pip3"
            | "pipx"
            | "uv"
            | "uvx"
            | "cargo"
            | "bun"
            | "bunx"
    ) {
        return Err(invalid(format!(
            "worker MCP command '{launcher}' is a runtime package launcher; install a direct server entrypoint in the worker image instead"
        )));
    }
    Ok(())
}

fn invalid(reason: impl Into<String>) -> ConfigError {
    ConfigError::InvalidConfig {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::mcp::McpPlacement;

    fn worker_config() -> McpServerConfig {
        McpServerConfig::new_stdio_with_secrets(
            "repo",
            "/usr/local/bin/repo-mcp",
            vec!["/workspace".to_string()],
            HashMap::from([("LOG_LEVEL".to_string(), "warn".to_string())]),
            HashMap::from([("API_TOKEN".to_string(), "repo_token".to_string())]),
        )
        .with_placement(McpPlacement::JobWorker)
        .with_always_require_approval(["write"])
    }

    #[test]
    fn selector_preserves_order_and_handles_empty_selection() {
        let mut servers = crate::tools::mcp::McpServersFile::default();
        let mut second = worker_config();
        second.name = "second".to_string();
        servers.upsert(worker_config());
        servers.upsert(second);
        let grants = HashMap::from([("API_TOKEN".to_string(), "repo_token".to_string())]);

        let selected = select_worker_mcp_servers(
            &["second".to_string(), "repo".to_string()],
            &servers,
            &grants,
        )
        .expect("ordered selection");
        assert_eq!(
            selected
                .iter()
                .map(|descriptor| descriptor.name.as_str())
                .collect::<Vec<_>>(),
            ["second", "repo"]
        );
        assert!(
            select_worker_mcp_servers(&[], &servers, &grants)
                .expect("empty selection")
                .is_empty()
        );
    }

    #[test]
    fn selector_rejects_missing_duplicates_and_excess() {
        let mut servers = crate::tools::mcp::McpServersFile::default();
        servers.upsert(worker_config());
        let grants = HashMap::from([("API_TOKEN".to_string(), "repo_token".to_string())]);
        assert!(select_worker_mcp_servers(&["missing".to_string()], &servers, &grants).is_err());
        assert!(
            select_worker_mcp_servers(&["repo".to_string(), "repo".to_string()], &servers, &grants)
                .is_err()
        );
        let excess = (0..=MAX_WORKER_MCP_SERVERS)
            .map(|index| format!("server-{index}"))
            .collect::<Vec<_>>();
        assert!(select_worker_mcp_servers(&excess, &servers, &grants).is_err());
    }

    #[test]
    fn resolves_only_exact_job_credential_grants() {
        let grants = HashMap::from([
            ("API_TOKEN".to_string(), "repo_token".to_string()),
            ("UNRELATED".to_string(), "other_secret".to_string()),
        ]);
        let descriptor = WorkerMcpServerDescriptor::from_config(&worker_config(), &grants)
            .expect("exact grant resolves");

        assert_eq!(descriptor.credential_env, vec!["API_TOKEN"]);
        assert!(!descriptor.env.contains_key("API_TOKEN"));
        let json = serde_json::to_string(&descriptor).expect("serialize descriptor");
        assert!(!json.contains("repo_token"));
        assert!(!json.contains("other_secret"));
    }

    #[test]
    fn rejects_missing_or_mismatched_grants() {
        let missing = WorkerMcpServerDescriptor::from_config(&worker_config(), &HashMap::new())
            .expect_err("missing grant must fail");
        assert!(missing.to_string().contains("API_TOKEN"));

        let mismatched = HashMap::from([("API_TOKEN".to_string(), "wrong".to_string())]);
        assert!(WorkerMcpServerDescriptor::from_config(&worker_config(), &mismatched).is_err());
    }

    #[test]
    fn rejects_runtime_package_launchers_and_wire_duplicates() {
        let mut config = worker_config();
        if let Some(crate::tools::mcp::McpTransportConfig::Stdio { command, .. }) =
            config.transport.as_mut()
        {
            *command = "npx".to_string();
        }
        let grants = HashMap::from([("API_TOKEN".to_string(), "repo_token".to_string())]);
        assert!(WorkerMcpServerDescriptor::from_config(&config, &grants).is_err());

        let mut descriptor = WorkerMcpServerDescriptor::from_config(&worker_config(), &grants)
            .expect("valid descriptor");
        descriptor.credential_env.push("API_TOKEN".to_string());
        assert!(descriptor.validate().is_err());
    }

    #[test]
    fn rejects_non_worker_or_disabled_configs() {
        let grants = HashMap::from([("API_TOKEN".to_string(), "repo_token".to_string())]);
        let mut config = worker_config();
        config.placement = McpPlacement::Orchestrator;
        assert!(WorkerMcpServerDescriptor::from_config(&config, &grants).is_err());

        config.placement = McpPlacement::JobWorker;
        config.enabled = false;
        assert!(WorkerMcpServerDescriptor::from_config(&config, &grants).is_err());
    }
}
