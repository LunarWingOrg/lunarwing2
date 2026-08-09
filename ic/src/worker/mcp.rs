//! Per-job MCP activation and lifecycle inside core Docker workers.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use secrecy::SecretString;

use crate::error::WorkerError;
use crate::tools::mcp::config::{EffectiveTransport, McpServerConfig};
use crate::tools::mcp::{McpClient, McpProcessManager, McpSessionManager};
use crate::tools::{ApprovalRequirement, Tool, ToolRegistry};
use crate::worker::api::McpCredentialResponse;

const MCP_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const MCP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(30);

/// Runtime state owned exclusively by one worker job.
pub struct WorkerMcpRuntime {
    registry: Arc<ToolRegistry>,
    sessions: Arc<McpSessionManager>,
    processes: Arc<McpProcessManager>,
    clients: Vec<(String, McpClient)>,
    wrappers: Vec<Arc<dyn Tool>>,
}

impl WorkerMcpRuntime {
    /// Activate full immutable snapshot. Any failure tears down all partial state.
    pub async fn activate(
        registry: Arc<ToolRegistry>,
        servers: Vec<McpServerConfig>,
        credentials: Vec<McpCredentialResponse>,
        owner_id: &str,
    ) -> Result<Self, WorkerError> {
        let mut credential_env = group_credentials(&servers, credentials)?;
        let sessions = Arc::new(McpSessionManager::new());
        let processes = Arc::new(McpProcessManager::new());
        let mut runtime = Self {
            registry,
            sessions,
            processes,
            clients: Vec::new(),
            wrappers: Vec::new(),
        };

        for server in servers {
            let server_name = server.name.clone();
            let secret_env = credential_env.remove(&server_name).unwrap_or_default();
            let client = match tokio::time::timeout(
                MCP_STARTUP_TIMEOUT,
                crate::tools::mcp::factory::create_worker_client_from_config(
                    server,
                    &runtime.sessions,
                    &runtime.processes,
                    secret_env,
                    owner_id,
                ),
            )
            .await
            {
                Ok(Ok(client)) => client,
                Ok(Err(error)) => {
                    let reason =
                        format!("worker MCP activation failed for '{server_name}': {error}");
                    return Err(runtime.activation_failure(reason).await);
                }
                Err(_) => {
                    let reason = format!("worker MCP startup timed out for '{server_name}'");
                    return Err(runtime.activation_failure(reason).await);
                }
            };
            runtime.clients.push((server_name.clone(), client));
            let discovery = match runtime.clients.last() {
                Some((_, client)) => {
                    tokio::time::timeout(MCP_STARTUP_TIMEOUT, client.create_worker_tools()).await
                }
                None => {
                    return Err(WorkerError::ExecutionFailed {
                        reason: "worker MCP client lifecycle invariant failed".to_string(),
                    });
                }
            };
            let wrappers = match discovery {
                Ok(Ok(wrappers)) => wrappers,
                Ok(Err(error)) => {
                    let reason =
                        format!("worker MCP tool discovery failed for '{server_name}': {error}");
                    return Err(runtime.activation_failure(reason).await);
                }
                Err(_) => {
                    let reason = format!("worker MCP startup timed out for '{server_name}'");
                    return Err(runtime.activation_failure(reason).await);
                }
            };
            if let Some(tool) = wrappers.iter().find(|tool| {
                tool.requires_approval(&serde_json::Value::Null) != ApprovalRequirement::Never
                    || !tool.allows_autonomous_execution()
            }) {
                let tool_name = tool.name().to_string();
                let reason = format!(
                    "worker MCP tool '{tool_name}' is not available for autonomous execution"
                );
                return Err(runtime.activation_failure(reason).await);
            }
            runtime.wrappers.extend(wrappers);
        }

        if let Err(conflicts) = runtime
            .registry
            .replace_runtime_tools(&[], runtime.wrappers.clone())
            .await
        {
            let reason = format!(
                "worker MCP tool registration conflicts: {}",
                conflicts.join(", ")
            );
            return Err(runtime.activation_failure(reason).await);
        }
        Ok(runtime)
    }

    /// Unregister exact wrappers first, then stop every client/process with bound time.
    pub async fn shutdown(&mut self) -> Result<(), WorkerError> {
        let mut errors = Vec::new();
        if let Err(conflicts) = self
            .registry
            .replace_runtime_tools(&self.wrappers, Vec::new())
            .await
        {
            errors.push(format!(
                "failed to unregister MCP tools: {}",
                conflicts.join(", ")
            ));
        }
        self.wrappers.clear();

        let shutdowns = self.clients.iter().map(|(name, client)| {
            crate::tools::mcp::factory::shutdown_client_runtime(
                name,
                Some(client),
                &self.sessions,
                &self.processes,
            )
        });
        match tokio::time::timeout(MCP_CLEANUP_TIMEOUT, futures::future::join_all(shutdowns)).await
        {
            Ok(results) => {
                errors.extend(
                    results
                        .into_iter()
                        .filter_map(Result::err)
                        .map(|e| e.to_string()),
                );
            }
            Err(_) => errors.push("worker MCP cleanup timed out".to_string()),
        }
        self.clients.clear();
        if tokio::time::timeout(Duration::from_secs(5), self.processes.shutdown_all())
            .await
            .is_err()
        {
            errors.push("worker MCP process cleanup timed out".to_string());
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(WorkerError::ExecutionFailed {
                reason: errors.join("; "),
            })
        }
    }

    async fn activation_failure(&mut self, reason: String) -> WorkerError {
        match self.shutdown().await {
            Ok(()) => WorkerError::ExecutionFailed { reason },
            Err(cleanup_error) => WorkerError::ExecutionFailed {
                reason: format!("{reason}; cleanup failed: {cleanup_error}"),
            },
        }
    }
}

fn group_credentials(
    servers: &[McpServerConfig],
    credentials: Vec<McpCredentialResponse>,
) -> Result<HashMap<String, HashMap<String, SecretString>>, WorkerError> {
    let expected = servers
        .iter()
        .map(|server| {
            let names = match server.effective_transport() {
                EffectiveTransport::Stdio { env_secrets, .. } => {
                    env_secrets.keys().cloned().collect::<HashSet<_>>()
                }
                _ => HashSet::new(),
            };
            (server.name.clone(), names)
        })
        .collect::<HashMap<_, _>>();
    let mut grouped: HashMap<String, HashMap<String, SecretString>> = HashMap::new();
    for mut credential in credentials {
        let value = SecretString::from(std::mem::take(&mut credential.value));
        let Some(expected_names) = expected.get(&credential.server_name) else {
            return Err(credential_coverage_error());
        };
        if !expected_names.contains(&credential.env_var)
            || grouped
                .entry(credential.server_name)
                .or_default()
                .insert(credential.env_var, value)
                .is_some()
        {
            return Err(credential_coverage_error());
        }
    }
    let exact = expected.iter().all(|(server, names)| {
        grouped.get(server).map_or(names.is_empty(), |values| {
            values.len() == names.len() && names.iter().all(|name| values.contains_key(name))
        })
    });
    exact
        .then_some(grouped)
        .ok_or_else(credential_coverage_error)
}

fn credential_coverage_error() -> WorkerError {
    WorkerError::ExecutionFailed {
        reason: "worker MCP credentials do not exactly cover descriptor secret environment"
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::mcp::config::{McpPlacement, McpServerConfig};

    fn server() -> McpServerConfig {
        McpServerConfig::new_stdio_with_secrets(
            "example",
            "cat",
            Vec::new(),
            HashMap::new(),
            HashMap::from([("TOKEN".to_string(), "worker-grant".to_string())]),
        )
        .with_placement(McpPlacement::JobWorker)
    }

    #[test]
    fn credential_grouping_requires_exact_coverage() {
        assert!(group_credentials(&[server()], Vec::new()).is_err());
        assert!(
            group_credentials(
                &[server()],
                vec![McpCredentialResponse {
                    server_name: "other".into(),
                    env_var: "TOKEN".into(),
                    value: "value".into(),
                }]
            )
            .is_err()
        );
        let grouped = group_credentials(
            &[server()],
            vec![McpCredentialResponse {
                server_name: "example".into(),
                env_var: "TOKEN".into(),
                value: "value".into(),
            }],
        )
        .unwrap();
        assert_eq!(grouped["example"].len(), 1);
    }

    #[tokio::test]
    async fn empty_runtime_shutdown_is_bounded_and_idempotent() {
        let registry = Arc::new(ToolRegistry::new());
        let mut runtime = WorkerMcpRuntime {
            registry,
            sessions: Arc::new(McpSessionManager::new()),
            processes: Arc::new(McpProcessManager::new()),
            clients: Vec::new(),
            wrappers: Vec::new(),
        };
        runtime.shutdown().await.unwrap();
        runtime.shutdown().await.unwrap();
    }
}
