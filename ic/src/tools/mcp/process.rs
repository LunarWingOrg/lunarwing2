//! MCP stdio process manager.
//!
//! Manages the lifecycle of MCP servers running as child processes.
//! Handles spawning, tracking, and shutdown.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::tools::mcp::stdio_transport::StdioMcpTransport;
use crate::tools::mcp::transport::McpTransport;
use crate::tools::tool::ToolError;

/// Current health of one managed stdio process.
#[derive(Debug)]
pub enum McpProcessState {
    Running,
    StdioClosed,
    Exited(std::process::ExitStatus),
    Missing,
}

/// Manages stdio MCP server processes.
///
/// Handles spawning, tracking, and shutdown of child processes. Higher-level
/// recovery belongs to `ExtensionManager`, which also owns clients and tools.
pub struct McpProcessManager {
    transports: RwLock<HashMap<String, Arc<StdioMcpTransport>>>,
}

impl McpProcessManager {
    pub fn new() -> Self {
        Self {
            transports: RwLock::new(HashMap::new()),
        }
    }

    /// Spawn a new stdio MCP server process.
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn_stdio(
        &self,
        name: impl Into<String>,
        command: impl Into<String>,
        args: Vec<String>,
        env: impl IntoIterator<Item = (impl AsRef<OsStr>, impl AsRef<OsStr>)>,
        suppress_stderr: bool,
        sandbox: Option<&crate::tools::mcp::sandbox::McpSandboxConfig>,
    ) -> Result<Arc<StdioMcpTransport>, ToolError> {
        let name = name.into();
        let command = command.into();
        let mut transports = self.transports.write().await;
        if transports.contains_key(&name) {
            return Err(ToolError::ExternalService(format!(
                "MCP server process '{name}' is already managed"
            )));
        }
        let transport = Arc::new(
            StdioMcpTransport::spawn(&name, &command, args, env, suppress_stderr, sandbox).await?,
        );
        transports.insert(name, Arc::clone(&transport));

        Ok(transport)
    }

    /// Get a transport by server name.
    pub async fn get(&self, name: &str) -> Option<Arc<StdioMcpTransport>> {
        self.transports.read().await.get(name).cloned()
    }

    /// Query one managed process without waiting for it to exit.
    pub async fn state(&self, name: &str) -> Result<McpProcessState, ToolError> {
        let Some(transport) = self.get(name).await else {
            return Ok(McpProcessState::Missing);
        };
        if let Some(status) = transport.exit_status().await? {
            return Ok(McpProcessState::Exited(status));
        }
        Ok(if transport.reader_finished().await {
            McpProcessState::StdioClosed
        } else {
            McpProcessState::Running
        })
    }

    /// Shut down all managed transports.
    pub async fn shutdown_all(&self) {
        let transports: Vec<(String, Arc<StdioMcpTransport>)> = {
            let mut map = self.transports.write().await;
            map.drain().collect()
        };

        futures::future::join_all(transports.into_iter().map(|(name, transport)| async move {
            if let Err(e) = transport.shutdown().await {
                tracing::warn!("Failed to shut down MCP stdio server '{}': {}", name, e);
            }
        }))
        .await;
    }

    /// Shut down a specific transport by name.
    pub async fn shutdown(&self, name: &str) -> Result<(), ToolError> {
        let transport = self.transports.write().await.remove(name);

        if let Some(transport) = transport {
            transport.shutdown().await?;
        }

        Ok(())
    }

    /// Get names of all managed transports.
    pub async fn managed_servers(&self) -> Vec<String> {
        self.transports.read().await.keys().cloned().collect()
    }
}

impl Default for McpProcessManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_creates_empty_manager() {
        let _manager = McpProcessManager::new();
    }

    #[tokio::test]
    async fn test_managed_servers_returns_empty_list_initially() {
        let manager = McpProcessManager::new();
        let servers = manager.managed_servers().await;
        assert!(servers.is_empty());
    }

    #[tokio::test]
    async fn state_reports_missing_running_and_exited() {
        let manager = McpProcessManager::new();
        assert!(matches!(
            manager.state("server").await.expect("missing state"),
            McpProcessState::Missing
        ));

        manager
            .spawn_stdio(
                "server",
                "cat",
                Vec::new(),
                HashMap::<String, String>::new(),
                false,
                None,
            )
            .await
            .expect("spawn cat");
        assert!(matches!(
            manager.state("server").await.expect("running state"),
            McpProcessState::Running
        ));
        manager.shutdown("server").await.expect("shutdown cat");

        manager
            .spawn_stdio(
                "server",
                "false",
                Vec::new(),
                HashMap::<String, String>::new(),
                false,
                None,
            )
            .await
            .expect("spawn false");
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert!(matches!(
            manager.state("server").await.expect("exited state"),
            McpProcessState::Exited(_)
        ));
        manager.shutdown("server").await.expect("clean up false");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn state_reports_closed_stdio_for_live_child() {
        let manager = McpProcessManager::new();
        manager
            .spawn_stdio(
                "server",
                "sh",
                vec!["-c".to_string(), "exec 1>&-; sleep 10".to_string()],
                HashMap::<String, String>::new(),
                false,
                None,
            )
            .await
            .expect("spawn live child with closed stdout");

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if matches!(
                    manager.state("server").await.expect("stdio state"),
                    McpProcessState::StdioClosed
                ) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("reader did not observe stdout closure");
        manager.shutdown("server").await.expect("clean up child");
    }

    #[tokio::test]
    async fn duplicate_process_name_preserves_existing_transport() {
        let manager = McpProcessManager::new();
        let first = manager
            .spawn_stdio(
                "server",
                "cat",
                Vec::new(),
                HashMap::<String, String>::new(),
                false,
                None,
            )
            .await
            .expect("spawn first process");
        let error = match manager
            .spawn_stdio(
                "server",
                "cat",
                Vec::new(),
                HashMap::<String, String>::new(),
                false,
                None,
            )
            .await
        {
            Ok(_) => panic!("duplicate process must fail"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("already managed"));
        let registered = manager
            .get("server")
            .await
            .expect("first transport retained");
        assert!(Arc::ptr_eq(&first, &registered));
        manager
            .shutdown("server")
            .await
            .expect("clean up first process");
    }

    #[tokio::test]
    async fn test_shutdown_all_on_empty_manager_does_not_panic() {
        let manager = McpProcessManager::new();
        manager.shutdown_all().await;
    }
}
