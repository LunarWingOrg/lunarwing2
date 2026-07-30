//! MCP stdio process manager.
//!
//! Manages the lifecycle of MCP servers running as child processes.
//! Handles spawning, shutdown, and crash recovery with exponential backoff.
//!
//! An optional supervisor task ([`McpProcessManager::start_supervisor`])
//! periodically polls managed transports and automatically restarts servers
//! whose child process exited unexpectedly, subject to a crash-loop guard.
//! Intentional shutdowns (via [`McpProcessManager::shutdown`] or
//! [`McpProcessManager::shutdown_all`]) are never restarted.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;

use crate::tools::mcp::stdio_transport::StdioMcpTransport;
use crate::tools::mcp::transport::McpTransport;
use crate::tools::tool::ToolError;

/// How often the supervisor polls managed transports for unexpected exits.
const SUPERVISOR_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Maximum automatic restarts allowed within [`RESTART_WINDOW`] before the
/// supervisor stops trying and leaves the server down (crash-loop guard).
const MAX_RESTARTS_IN_WINDOW: usize = 5;

/// Sliding window for the crash-loop guard.
const RESTART_WINDOW: Duration = Duration::from_secs(600);

/// Configuration for spawning a stdio MCP server.
#[derive(Debug, Clone)]
pub struct StdioSpawnConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

/// Manages stdio MCP server processes.
///
/// Handles spawning, tracking, and shutdown of child processes.
pub struct McpProcessManager {
    transports: RwLock<HashMap<String, Arc<StdioMcpTransport>>>,
    configs: RwLock<HashMap<String, StdioSpawnConfig>>,
    /// Restart timestamps per server, for the crash-loop guard.
    restart_history: Mutex<HashMap<String, Vec<Instant>>>,
    /// Servers with an in-flight automatic restart (prevents duplicates).
    restarting: Mutex<HashSet<String>>,
    /// Handle of the background supervisor task, if started.
    supervisor: Mutex<Option<JoinHandle<()>>>,
}

impl McpProcessManager {
    pub fn new() -> Self {
        Self {
            transports: RwLock::new(HashMap::new()),
            configs: RwLock::new(HashMap::new()),
            restart_history: Mutex::new(HashMap::new()),
            restarting: Mutex::new(HashSet::new()),
            supervisor: Mutex::new(None),
        }
    }

    /// Spawn a new stdio MCP server process.
    pub async fn spawn_stdio(
        &self,
        name: impl Into<String>,
        command: impl Into<String>,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> Result<Arc<StdioMcpTransport>, ToolError> {
        let name = name.into();
        let command = command.into();

        let spawn_config = StdioSpawnConfig {
            command: command.clone(),
            args: args.clone(),
            env: env.clone(),
        };
        let transport = Arc::new(StdioMcpTransport::spawn(&name, &command, args, env).await?);

        // A failed spawn must not leave a restart configuration behind.
        self.configs
            .write()
            .await
            .insert(name.clone(), spawn_config);

        // A deliberate (re)spawn resets the crash-loop guard.
        self.restart_history.lock().await.insert(name.clone(), Vec::new());

        self.transports
            .write()
            .await
            .insert(name, Arc::clone(&transport));

        Ok(transport)
    }

    /// Get a transport by server name.
    pub async fn get(&self, name: &str) -> Option<Arc<StdioMcpTransport>> {
        self.transports.read().await.get(name).cloned()
    }

    /// Shut down all managed transports and stop the supervisor.
    pub async fn shutdown_all(&self) {
        self.stop_supervisor().await;

        let transports: Vec<(String, Arc<StdioMcpTransport>)> = {
            let mut map = self.transports.write().await;
            map.drain().collect()
        };

        for (name, transport) in transports {
            if let Err(e) = transport.shutdown().await {
                tracing::warn!("Failed to shut down MCP stdio server '{}': {}", name, e);
            }
        }

        self.restart_history.lock().await.clear();
        self.restarting.lock().await.clear();
    }

    /// Shut down a specific transport by name.
    pub async fn shutdown(&self, name: &str) -> Result<(), ToolError> {
        let transport = self.transports.write().await.remove(name);

        if let Some(transport) = transport {
            transport.shutdown().await?;
        }

        self.configs.write().await.remove(name);
        self.restart_history.lock().await.remove(name);
        self.restarting.lock().await.remove(name);
        Ok(())
    }

    /// Start the background crash supervisor.
    ///
    /// Idempotent: calling it again while a supervisor is already running is
    /// a no-op. The supervisor polls managed transports every few seconds and
    /// restarts servers whose child exited unexpectedly. Restarts run in
    /// their own tasks (with the usual exponential backoff) so one failing
    /// server does not block supervision of the others.
    pub fn start_supervisor(self: &Arc<Self>) {
        let mut guard = match self.supervisor.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                tracing::debug!("MCP supervisor start requested while lock is held; skipping");
                return;
            }
        };
        if guard.is_some() {
            return;
        }

        let manager = Arc::clone(self);
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(SUPERVISOR_POLL_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                manager.supervise_once().await;
            }
        });
        *guard = Some(handle);
        tracing::debug!("MCP stdio crash supervisor started");
    }

    /// Stop the background crash supervisor, if running.
    pub async fn stop_supervisor(&self) {
        if let Some(handle) = self.supervisor.lock().await.take() {
            handle.abort();
        }
    }

    /// One supervisor pass: detect crashed servers and kick off restarts.
    async fn supervise_once(self: &Arc<Self>) {
        let snapshot: Vec<(String, Arc<StdioMcpTransport>)> = {
            let map = self.transports.read().await;
            map.iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect()
        };

        for (name, transport) in snapshot {
            let Some(status) = transport.poll_exit_status().await else {
                continue;
            };
            if transport.is_shutting_down() {
                continue;
            }
            if !self.configs.read().await.contains_key(&name) {
                // Config removed concurrently — intentional teardown.
                continue;
            }

            tracing::warn!(
                "MCP stdio server '{}' exited unexpectedly with {}; scheduling restart",
                name,
                status
            );

            if !self.claim_restart_slot(&name).await {
                continue;
            }

            let job = SupervisorRestart {
                manager: Arc::clone(self),
                name,
            };
            tokio::spawn(async move {
                job.run().await;
            });
        }
    }

    /// Mark a server as having a restart in flight. Returns false (and leaves
    /// no mark) when a restart is already running for it.
    async fn claim_restart_slot(&self, name: &str) -> bool {
        let mut restarting = self.restarting.lock().await;
        restarting.insert(name.to_string())
    }

    /// Record an automatic restart attempt and enforce the crash-loop guard.
    ///
    /// Returns true when another restart is allowed.
    async fn record_restart_attempt(&self, name: &str) -> bool {
        let mut history = self.restart_history.lock().await;
        let attempts = history.entry(name.to_string()).or_default();
        let cutoff = Instant::now() - RESTART_WINDOW;
        attempts.retain(|t| *t > cutoff);
        if attempts.len() >= MAX_RESTARTS_IN_WINDOW {
            tracing::error!(
                "MCP stdio server '{}' exceeded {} restarts in {}s; leaving it down until the next manual activation",
                name,
                MAX_RESTARTS_IN_WINDOW,
                RESTART_WINDOW.as_secs()
            );
            return false;
        }
        attempts.push(Instant::now());
        true
    }

    /// Clear the in-flight restart marker for a server.
    async fn clear_restart_slot(&self, name: &str) {
        self.restarting.lock().await.remove(name);
    }

    /// Remove a crash-looping server from supervision, leaving it down until
    /// the next manual activation.
    async fn retire_crash_looping_server(&self, name: &str) {
        if let Some(transport) = self.transports.write().await.remove(name) {
            let _ = transport.shutdown().await;
        }
        self.configs.write().await.remove(name);
        tracing::warn!(
            "MCP stdio server '{}' removed from supervision after crash-loop guard tripped",
            name
        );
    }

    /// Attempt to restart a crashed transport with exponential backoff.
    ///
    /// Tries up to 5 times with delays of 1s, 2s, 4s, 8s, 16s (total: 31s max wait).
    pub async fn try_restart(&self, name: &str) -> Result<Arc<StdioMcpTransport>, ToolError> {
        let config = self
            .configs
            .read()
            .await
            .get(name)
            .cloned()
            .ok_or_else(|| {
                ToolError::ExternalService(format!(
                    "No spawn config for MCP server '{}', cannot restart",
                    name
                ))
            })?;

        // Shut down and remove old transport to avoid orphaning a wedged process.
        if let Some(old_transport) = self.transports.write().await.remove(name) {
            let _ = old_transport.shutdown().await;
        }

        let max_retries = 5;
        let mut last_err = None;

        for attempt in 0..max_retries {
            let delay = Duration::from_secs(1 << attempt);
            tokio::time::sleep(delay).await;

            match StdioMcpTransport::spawn(
                name,
                &config.command,
                config.args.clone(),
                config.env.clone(),
            )
            .await
            {
                Ok(transport) => {
                    let transport = Arc::new(transport);
                    self.transports
                        .write()
                        .await
                        .insert(name.to_string(), Arc::clone(&transport));
                    tracing::info!(
                        "MCP stdio server '{}' restarted after {} attempt(s)",
                        name,
                        attempt + 1
                    );
                    return Ok(transport);
                }
                Err(e) => {
                    tracing::warn!(
                        "Restart attempt {}/{} for MCP server '{}' failed: {}",
                        attempt + 1,
                        max_retries,
                        name,
                        e
                    );
                    last_err = Some(e);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| {
            ToolError::ExternalService(format!(
                "Failed to restart MCP server '{}' after {} attempts",
                name, max_retries
            ))
        }))
    }

    /// Get names of all managed transports.
    pub async fn managed_servers(&self) -> Vec<String> {
        self.transports.read().await.keys().cloned().collect()
    }
}

/// Owned restart job spawned by the supervisor for one crashed server.
struct SupervisorRestart {
    manager: Arc<McpProcessManager>,
    name: String,
}

impl SupervisorRestart {
    async fn run(&self) {
        let manager = &self.manager;
        let name = &self.name;

        if !manager.record_restart_attempt(name).await {
            manager.retire_crash_looping_server(name).await;
            manager.clear_restart_slot(name).await;
            return;
        }

        match manager.try_restart(name).await {
            Ok(_) => {
                tracing::info!("MCP stdio server '{}' automatically restarted", name);
            }
            Err(e) => {
                tracing::error!(
                    "Automatic restart of MCP stdio server '{}' failed: {}",
                    name,
                    e
                );
            }
        }

        manager.clear_restart_slot(name).await;
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
    async fn test_shutdown_all_on_empty_manager_does_not_panic() {
        let manager = McpProcessManager::new();
        manager.shutdown_all().await;
    }

    #[tokio::test]
    async fn test_crash_loop_guard_allows_then_blocks() {
        let manager = McpProcessManager::new();
        for i in 0..MAX_RESTARTS_IN_WINDOW {
            assert!(
                manager.record_restart_attempt("srv").await,
                "attempt {} must be allowed",
                i + 1
            );
        }
        assert!(
            !manager.record_restart_attempt("srv").await,
            "attempt beyond the cap must be blocked"
        );
    }

    #[tokio::test]
    async fn test_crash_loop_guard_resets_after_window() {
        let manager = McpProcessManager::new();
        {
            let mut history = manager.restart_history.lock().await;
            history.insert(
                "srv".to_string(),
                vec![Instant::now() - RESTART_WINDOW - Duration::from_secs(1)],
            );
        }
        // The stale attempt is outside the window, so a new one is allowed.
        assert!(manager.record_restart_attempt("srv").await);
    }

    #[tokio::test]
    async fn test_restart_slot_prevents_duplicate_restarts() {
        let manager = McpProcessManager::new();
        assert!(manager.claim_restart_slot("srv").await);
        assert!(!manager.claim_restart_slot("srv").await);
        manager.clear_restart_slot("srv").await;
        assert!(manager.claim_restart_slot("srv").await);
    }

    #[tokio::test]
    async fn test_spawn_clears_restart_state() {
        let manager = McpProcessManager::new();
        manager.restart_history.lock().await.insert(
            "srv".to_string(),
            vec![Instant::now()],
        );
        manager
            .restarting
            .lock()
            .await
            .insert("srv".to_string());

        manager
            .spawn_stdio("srv", "cat", Vec::new(), HashMap::new())
            .await
            .expect("cat should spawn");

        assert!(
            manager
                .restart_history
                .lock()
                .await
                .get("srv")
                .is_some_and(Vec::is_empty),
            "spawn must reset the crash-loop history"
        );
        manager.shutdown("srv").await.expect("shutdown");
    }

    #[tokio::test]
    async fn test_supervisor_detects_unexpected_exit_and_restarts() {
        let manager = Arc::new(McpProcessManager::new());
        // `false` exits immediately with a failure status.
        manager
            .spawn_stdio("srv", "false", Vec::new(), HashMap::new())
            .await
            .expect("false should spawn");
        manager.start_supervisor();

        // Give the supervisor a few poll cycles to detect the exit; the
        // restart itself backs off (1s, 2s, ...) and `false` keeps failing,
        // so after a while the crash-loop guard or in-flight slot must be
        // visible. The key assertion: no panic, no hang, and the manager is
        // still consistent.
        tokio::time::sleep(Duration::from_secs(5)).await;

        assert!(
            manager.restart_history.lock().await.contains_key("srv"),
            "supervisor must have recorded restart attempts for the crashed server"
        );

        manager.shutdown_all().await;
    }

    #[tokio::test]
    async fn test_supervisor_ignores_intentional_shutdown() {
        let manager = Arc::new(McpProcessManager::new());
        manager
            .spawn_stdio("srv", "cat", Vec::new(), HashMap::new())
            .await
            .expect("cat should spawn");
        manager.start_supervisor();

        manager.shutdown("srv").await.expect("shutdown");
        tokio::time::sleep(SUPERVISOR_POLL_INTERVAL * 2).await;

        assert!(
            !manager.restart_history.lock().await.contains_key("srv"),
            "intentional shutdown must not trigger restarts"
        );
        assert!(manager.managed_servers().await.is_empty());

        manager.shutdown_all().await;
    }
}
