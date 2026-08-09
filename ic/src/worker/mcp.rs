//! Per-job MCP activation, crash supervision, and lifecycle inside core Docker workers.
//!
//! After activation, one supervisor task per server watches its managed stdio
//! child through the shared process manager. An unexpected exit, closed stdio,
//! or missing runtime state restarts the server from the job's immutable
//! snapshot with jittered exponential backoff. Resolved secret values are
//! retained in trusted worker memory for the job lifetime so restarts can
//! re-spawn children without re-fetching grants from the orchestrator; they
//! are zeroized when the runtime drops. Restarts stop after
//! `MCP_RESTART_MAX_ATTEMPTS` unstable attempts, leaving the server down for
//! the rest of the job. Job teardown cancels supervision first, then unloads
//! tools, clients, sessions, and processes with bounded waits.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::Rng;
use secrecy::SecretString;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::error::WorkerError;
use crate::tools::mcp::config::{EffectiveTransport, McpServerConfig};
use crate::tools::mcp::{
    MAX_WORKER_MCP_TOOL_BYTES, MAX_WORKER_MCP_TOOLS, McpClient, McpProcessManager,
    McpProcessState, McpSessionManager,
};
use crate::tools::{ApprovalRequirement, Tool, ToolRegistry};
use crate::worker::api::McpCredentialResponse;

const MCP_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const MCP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(30);
const MCP_SUPERVISOR_INTERVAL: Duration = Duration::from_secs(2);
const MCP_SUPERVISOR_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const MCP_RESTART_BACKOFF_BASE: Duration = Duration::from_secs(1);
const MCP_RESTART_BACKOFF_MAX: Duration = Duration::from_secs(30);
const MCP_RESTART_MAX_ATTEMPTS: u32 = 5;
const MCP_RESTART_STABILITY_WINDOW: Duration = Duration::from_secs(30);

/// Active per-server runtime state. Guarded by the shared mutex so crash
/// detection, restart, and job teardown serialize against each other.
struct WorkerMcpServer {
    /// Immutable job snapshot reused verbatim by restarts.
    config: McpServerConfig,
    /// Resolved secret environment retained for the job lifetime so restarts
    /// can re-spawn the child without a new orchestrator grant fetch.
    secret_env: HashMap<String, SecretString>,
    client: McpClient,
    wrappers: Vec<Arc<dyn Tool>>,
    descriptor_bytes: usize,
    restart_attempts: u32,
    healthy_since: Instant,
}

/// Runtime state shared with the per-server supervisor tasks.
struct WorkerMcpShared {
    registry: Arc<ToolRegistry>,
    sessions: Arc<McpSessionManager>,
    processes: Arc<McpProcessManager>,
    owner_id: String,
    servers: Mutex<HashMap<String, WorkerMcpServer>>,
}

/// Runtime state owned exclusively by one worker job.
pub struct WorkerMcpRuntime {
    shared: Arc<WorkerMcpShared>,
    supervisor_cancel: CancellationToken,
    supervisor_handles: Vec<tokio::task::JoinHandle<()>>,
}

impl WorkerMcpRuntime {
    /// Activate full immutable snapshot. Any failure tears down all partial state.
    pub async fn activate(
        registry: Arc<ToolRegistry>,
        servers: Vec<McpServerConfig>,
        credentials: Vec<McpCredentialResponse>,
        owner_id: &str,
    ) -> Result<Self, WorkerError> {
        if !servers.is_empty() {
            require_sibling_process_isolation().await?;
        }
        let mut credential_env = group_credentials(&servers, credentials)?;
        let shared = Arc::new(WorkerMcpShared {
            registry,
            sessions: Arc::new(McpSessionManager::new()),
            processes: Arc::new(McpProcessManager::new()),
            owner_id: owner_id.to_string(),
            servers: Mutex::new(HashMap::new()),
        });
        let mut runtime = Self {
            shared,
            supervisor_cancel: CancellationToken::new(),
            supervisor_handles: Vec::new(),
        };
        let mut all_wrappers: Vec<Arc<dyn Tool>> = Vec::new();

        for server in servers {
            let secret_env = credential_env.remove(&server.name).unwrap_or_default();
            match runtime.activate_one(server, secret_env).await {
                Ok(wrappers) => all_wrappers.extend(wrappers),
                Err(reason) => return Err(runtime.activation_failure(reason).await),
            }
        }

        if let Err(conflicts) = runtime
            .shared
            .registry
            .replace_runtime_tools(&[], all_wrappers)
            .await
        {
            let reason = format!(
                "worker MCP tool registration conflicts: {}",
                conflicts.join(", ")
            );
            return Err(runtime.activation_failure(reason).await);
        }
        runtime.spawn_supervisors().await;
        Ok(runtime)
    }

    /// Start one client, discover and validate its tools, and track the
    /// partial runtime so activation failure cleans it up.
    async fn activate_one(
        &self,
        config: McpServerConfig,
        secret_env: HashMap<String, SecretString>,
    ) -> Result<Vec<Arc<dyn Tool>>, String> {
        let name = config.name.clone();
        let client = match tokio::time::timeout(
            MCP_STARTUP_TIMEOUT,
            crate::tools::mcp::factory::create_worker_client_from_config(
                config.clone(),
                &self.shared.sessions,
                &self.shared.processes,
                secret_env.clone(),
                &self.shared.owner_id,
            ),
        )
        .await
        {
            Ok(Ok(client)) => client,
            Ok(Err(error)) => {
                return Err(format!("worker MCP activation failed for '{name}': {error}"));
            }
            Err(_) => return Err(format!("worker MCP startup timed out for '{name}'")),
        };
        self.shared.servers.lock().await.insert(
            name.clone(),
            WorkerMcpServer {
                config,
                secret_env,
                client: client.clone(),
                wrappers: Vec::new(),
                descriptor_bytes: 0,
                restart_attempts: 0,
                healthy_since: Instant::now(),
            },
        );
        let discovery =
            match tokio::time::timeout(MCP_STARTUP_TIMEOUT, client.create_worker_tools()).await {
                Ok(Ok(discovery)) => discovery,
                Ok(Err(error)) => {
                    return Err(format!(
                        "worker MCP tool discovery failed for '{name}': {error}"
                    ));
                }
                Err(_) => return Err(format!("worker MCP startup timed out for '{name}'")),
            };
        let (wrappers, discovered_bytes) = discovery;
        let (current_tools, current_bytes) = {
            let servers = self.shared.servers.lock().await;
            aggregate_totals(&servers)
        };
        validate_aggregate_discovery(
            current_tools,
            current_bytes,
            wrappers.len(),
            discovered_bytes,
        )?;
        if let Some(reason) = autonomous_tool_violation(&wrappers) {
            return Err(reason);
        }
        let mut servers = self.shared.servers.lock().await;
        let Some(server) = servers.get_mut(&name) else {
            return Err("worker MCP client lifecycle invariant failed".to_string());
        };
        server.wrappers = wrappers.clone();
        server.descriptor_bytes = discovered_bytes;
        Ok(wrappers)
    }

    /// Spawn one crash-supervision task per activated server.
    async fn spawn_supervisors(&mut self) {
        let names = self
            .shared
            .servers
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for name in names {
            let shared = Arc::clone(&self.shared);
            let cancel = self.supervisor_cancel.child_token();
            let handle = tokio::spawn(supervise_worker_mcp_server(shared, name, cancel));
            self.supervisor_handles.push(handle);
        }
    }

    /// Stop supervision, unregister exact wrappers, then stop every
    /// client/process with bound time.
    pub async fn shutdown(&mut self) -> Result<(), WorkerError> {
        self.stop_supervisors().await;
        let mut errors = Vec::new();
        let servers = std::mem::take(&mut *self.shared.servers.lock().await);
        let wrappers = servers
            .values()
            .flat_map(|server| server.wrappers.iter().cloned())
            .collect::<Vec<Arc<dyn Tool>>>();
        if !wrappers.is_empty()
            && let Err(conflicts) = self
                .shared
                .registry
                .replace_runtime_tools(&wrappers, Vec::new())
                .await
        {
            errors.push(format!(
                "failed to unregister MCP tools: {}",
                conflicts.join(", ")
            ));
        }

        let shutdowns = servers.iter().map(|(name, server)| {
            crate::tools::mcp::factory::shutdown_client_runtime(
                name,
                Some(&server.client),
                &self.shared.sessions,
                &self.shared.processes,
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
        drop(servers);
        if tokio::time::timeout(Duration::from_secs(5), self.shared.processes.shutdown_all())
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

    /// Cancel every supervisor task; abort any that outlive the bounded wait.
    async fn stop_supervisors(&mut self) {
        self.supervisor_cancel.cancel();
        let mut handles = std::mem::take(&mut self.supervisor_handles);
        if handles.is_empty() {
            return;
        }
        if tokio::time::timeout(
            MCP_SUPERVISOR_SHUTDOWN_TIMEOUT,
            futures::future::join_all(handles.iter_mut()),
        )
        .await
        .is_err()
        {
            tracing::warn!(
                "Worker MCP supervisor tasks did not stop before timeout; aborting them"
            );
            for handle in &handles {
                handle.abort();
            }
            let _ = futures::future::join_all(handles.iter_mut()).await;
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

    #[cfg(test)]
    pub(crate) async fn restart_attempts(&self, name: &str) -> Option<u32> {
        self.shared
            .servers
            .lock()
            .await
            .get(name)
            .map(|server| server.restart_attempts)
    }
}

impl Drop for WorkerMcpRuntime {
    fn drop(&mut self) {
        // Last-resort cancellation when `shutdown` was never awaited (for
        // example on early worker exit); teardown bounds remain in `shutdown`.
        self.supervisor_cancel.cancel();
        for handle in &self.supervisor_handles {
            handle.abort();
        }
    }
}

/// Watch one managed stdio child and restart it after unexpected death.
async fn supervise_worker_mcp_server(
    shared: Arc<WorkerMcpShared>,
    name: String,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            () = tokio::time::sleep(MCP_SUPERVISOR_INTERVAL) => {}
        }
        let needs_restart = match shared.processes.state(&name).await {
            Ok(McpProcessState::Running) => false,
            Ok(McpProcessState::StdioClosed) => {
                tracing::warn!(server = %name, "Worker MCP server stdio closed unexpectedly");
                true
            }
            Ok(McpProcessState::Exited(status)) => {
                tracing::warn!(server = %name, %status, "Worker MCP server exited unexpectedly");
                true
            }
            Ok(McpProcessState::Missing) | Err(_) => {
                tracing::warn!(server = %name, "Worker MCP server runtime is missing");
                true
            }
        };
        if needs_restart && !restart_worker_mcp_server(&shared, &name, &cancel).await {
            break;
        }
    }
}

/// Claim one restart attempt with backoff, then rebuild the server.
///
/// Returns `false` when supervision must stop: shutdown was requested, the
/// server state is gone, or the restart circuit opened.
async fn restart_worker_mcp_server(
    shared: &Arc<WorkerMcpShared>,
    name: &str,
    cancel: &CancellationToken,
) -> bool {
    let attempts = {
        let mut servers = shared.servers.lock().await;
        match servers.get_mut(name) {
            Some(server) => match claim_worker_mcp_restart(server) {
                Some(attempts) => attempts,
                None => {
                    tracing::error!(
                        server = %name,
                        attempts = server.restart_attempts,
                        "Worker MCP restart supervision stopped after repeated crashes; \
                         server stays down for the rest of this job"
                    );
                    return false;
                }
            },
            None => return false,
        }
    };
    if attempts > 1 {
        let delay = worker_mcp_restart_backoff(attempts);
        tracing::warn!(
            server = %name,
            attempts,
            retry_millis = delay.as_millis(),
            "Worker MCP restart delayed after unstable runtime"
        );
        tokio::select! {
            () = cancel.cancelled() => return false,
            () = tokio::time::sleep(delay) => {}
        }
    }
    rebuild_worker_mcp_server(shared, name, cancel).await
}

/// Tear down the dead runtime, rebuild it from the immutable snapshot, and
/// atomically swap its registered tools. Old tools stay registered until the
/// replacement is ready so in-flight calls fail against the old client
/// instead of disappearing mid-job.
async fn rebuild_worker_mcp_server(
    shared: &Arc<WorkerMcpShared>,
    name: &str,
    cancel: &CancellationToken,
) -> bool {
    let (old_client, old_wrappers, config, secret_env) = {
        let servers = shared.servers.lock().await;
        match servers.get(name) {
            Some(server) => (
                server.client.clone(),
                server.wrappers.clone(),
                server.config.clone(),
                server.secret_env.clone(),
            ),
            None => return false,
        }
    };
    match cancelable_step(
        cancel,
        MCP_CLEANUP_TIMEOUT,
        crate::tools::mcp::factory::shutdown_client_runtime(
            name,
            Some(&old_client),
            &shared.sessions,
            &shared.processes,
        ),
    )
    .await
    {
        Err(WorkerMcpStepAbort::Cancelled) => return false,
        Err(WorkerMcpStepAbort::TimedOut) => {
            tracing::warn!(server = %name, "Worker MCP crash cleanup timed out");
        }
        Ok(Err(error)) => {
            tracing::warn!(server = %name, %error, "Worker MCP crash cleanup was incomplete");
        }
        Ok(Ok(())) => {}
    }

    let client = match cancelable_step(
        cancel,
        MCP_STARTUP_TIMEOUT,
        crate::tools::mcp::factory::create_worker_client_from_config(
            config,
            &shared.sessions,
            &shared.processes,
            secret_env,
            &shared.owner_id,
        ),
    )
    .await
    {
        Ok(Ok(client)) => client,
        Ok(Err(error)) => {
            tracing::warn!(server = %name, %error, "Worker MCP restart initialization failed");
            return true;
        }
        Err(WorkerMcpStepAbort::Cancelled) => {
            cleanup_partial_restart(shared, name, None).await;
            return false;
        }
        Err(WorkerMcpStepAbort::TimedOut) => {
            tracing::warn!(server = %name, "Worker MCP restart initialization timed out");
            cleanup_partial_restart(shared, name, None).await;
            return true;
        }
    };

    let (wrappers, descriptor_bytes) =
        match rebuild_discover_tools(shared, name, &client, cancel).await {
            RebuildDiscovery::Discovered(discovery) => discovery,
            RebuildDiscovery::Retry => return true,
            RebuildDiscovery::Stop => return false,
        };
    finish_worker_mcp_rebuild(shared, name, client, old_wrappers, wrappers, descriptor_bytes).await
}

/// Discovery result for one restart attempt.
enum RebuildDiscovery {
    Discovered((Vec<Arc<dyn Tool>>, usize)),
    /// Discovery failed or was rejected; keep supervising and retry later.
    Retry,
    /// Shutdown was requested; stop supervising.
    Stop,
}

async fn rebuild_discover_tools(
    shared: &Arc<WorkerMcpShared>,
    name: &str,
    client: &McpClient,
    cancel: &CancellationToken,
) -> RebuildDiscovery {
    let (wrappers, descriptor_bytes) =
        match cancelable_step(cancel, MCP_STARTUP_TIMEOUT, client.create_worker_tools()).await {
            Ok(Ok(discovery)) => discovery,
            Ok(Err(error)) => {
                tracing::warn!(server = %name, %error, "Worker MCP restart tool discovery failed");
                cleanup_partial_restart(shared, name, Some(client)).await;
                return RebuildDiscovery::Retry;
            }
            Err(WorkerMcpStepAbort::Cancelled) => {
                cleanup_partial_restart(shared, name, Some(client)).await;
                return RebuildDiscovery::Stop;
            }
            Err(WorkerMcpStepAbort::TimedOut) => {
                tracing::warn!(server = %name, "Worker MCP restart tool discovery timed out");
                cleanup_partial_restart(shared, name, Some(client)).await;
                return RebuildDiscovery::Retry;
            }
        };
    if let Some(reason) = autonomous_tool_violation(&wrappers) {
        tracing::warn!(server = %name, %reason, "Worker MCP restart rejected discovered tools");
        cleanup_partial_restart(shared, name, Some(client)).await;
        return RebuildDiscovery::Retry;
    }
    let (other_tools, other_bytes) = {
        let servers = shared.servers.lock().await;
        servers
            .iter()
            .filter(|(other, _)| other.as_str() != name)
            .fold((0, 0), |(tools, bytes), (_, server)| {
                (tools + server.wrappers.len(), bytes + server.descriptor_bytes)
            })
    };
    if let Err(reason) =
        validate_aggregate_discovery(other_tools, other_bytes, wrappers.len(), descriptor_bytes)
    {
        tracing::warn!(server = %name, %reason, "Worker MCP restart exceeded aggregate limits");
        cleanup_partial_restart(shared, name, Some(client)).await;
        return RebuildDiscovery::Retry;
    }
    RebuildDiscovery::Discovered((wrappers, descriptor_bytes))
}

/// Atomically swap the rebuilt tools into the registry and track the new
/// runtime. Returns `false` when supervision must stop.
async fn finish_worker_mcp_rebuild(
    shared: &Arc<WorkerMcpShared>,
    name: &str,
    client: McpClient,
    old_wrappers: Vec<Arc<dyn Tool>>,
    wrappers: Vec<Arc<dyn Tool>>,
    descriptor_bytes: usize,
) -> bool {
    if let Err(conflicts) = shared
        .registry
        .replace_runtime_tools(&old_wrappers, wrappers.clone())
        .await
    {
        tracing::warn!(
            server = %name,
            tools = ?conflicts,
            "Worker MCP restart tool replacement conflicted"
        );
        cleanup_partial_restart(shared, name, Some(&client)).await;
        return true;
    }
    let tool_count = wrappers.len();
    let mut servers = shared.servers.lock().await;
    match servers.get_mut(name) {
        Some(server) => {
            server.client = client;
            server.wrappers = wrappers;
            server.descriptor_bytes = descriptor_bytes;
            server.healthy_since = Instant::now();
        }
        None => {
            // Teardown drained the servers concurrently; undo the swap.
            drop(servers);
            let _ = shared
                .registry
                .replace_runtime_tools(&wrappers, Vec::new())
                .await;
            cleanup_partial_restart(shared, name, Some(&client)).await;
            return false;
        }
    }
    drop(servers);
    tracing::info!(
        server = %name,
        tools = tool_count,
        "Restarted worker MCP server after unexpected exit"
    );
    true
}

/// Bounded cleanup of a partially rebuilt runtime after a failed or cancelled
/// restart step.
async fn cleanup_partial_restart(shared: &WorkerMcpShared, name: &str, client: Option<&McpClient>) {
    match tokio::time::timeout(
        MCP_CLEANUP_TIMEOUT,
        crate::tools::mcp::factory::shutdown_client_runtime(
            name,
            client,
            &shared.sessions,
            &shared.processes,
        ),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(server = %name, %error, "Worker MCP restart cleanup was incomplete");
        }
        Err(_) => tracing::warn!(server = %name, "Worker MCP restart cleanup timed out"),
    }
}

enum WorkerMcpStepAbort {
    Cancelled,
    TimedOut,
}

/// Run one restart step with a hard deadline and prompt shutdown cancellation.
async fn cancelable_step<T>(
    cancel: &CancellationToken,
    timeout: Duration,
    future: impl std::future::Future<Output = T>,
) -> Result<T, WorkerMcpStepAbort> {
    tokio::select! {
        () = cancel.cancelled() => Err(WorkerMcpStepAbort::Cancelled),
        result = tokio::time::timeout(timeout, future) => {
            result.map_err(|_| WorkerMcpStepAbort::TimedOut)
        }
    }
}

/// Claim one restart attempt. A server that stayed healthy for the stability
/// window resets its attempt window; `None` means the circuit is open.
fn claim_worker_mcp_restart(server: &mut WorkerMcpServer) -> Option<u32> {
    if server.healthy_since.elapsed() >= MCP_RESTART_STABILITY_WINDOW {
        server.restart_attempts = 0;
    }
    if server.restart_attempts >= MCP_RESTART_MAX_ATTEMPTS {
        return None;
    }
    server.restart_attempts += 1;
    Some(server.restart_attempts)
}

fn worker_mcp_restart_backoff(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(5);
    let capped = MCP_RESTART_BACKOFF_BASE
        .checked_mul(1_u32 << exponent)
        .unwrap_or(MCP_RESTART_BACKOFF_MAX)
        .min(MCP_RESTART_BACKOFF_MAX);
    let max_millis = capped.as_millis() as u64;
    let min_millis = (max_millis / 2).max(1);
    Duration::from_millis(rand::thread_rng().gen_range(min_millis..=max_millis))
}

/// Worker jobs run without approvals; any tool needing approval or blocking
/// autonomous execution rejects the whole activation or restart.
fn autonomous_tool_violation(wrappers: &[Arc<dyn Tool>]) -> Option<String> {
    wrappers
        .iter()
        .find(|tool| {
            tool.requires_approval(&serde_json::Value::Null) != ApprovalRequirement::Never
                || !tool.allows_autonomous_execution()
        })
        .map(|tool| {
            format!(
                "worker MCP tool '{}' is not available for autonomous execution",
                tool.name()
            )
        })
}

fn aggregate_totals(servers: &HashMap<String, WorkerMcpServer>) -> (usize, usize) {
    servers.values().fold((0, 0), |(tools, bytes), server| {
        (
            tools + server.wrappers.len(),
            bytes + server.descriptor_bytes,
        )
    })
}

#[cfg(target_os = "linux")]
async fn require_sibling_process_isolation() -> Result<(), WorkerError> {
    let scope = tokio::fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope")
        .await
        .map_err(|error| WorkerError::ExecutionFailed {
            reason: format!("worker-local MCP requires Linux Yama sibling isolation: {error}"),
        })?;
    if ptrace_scope_isolates_siblings(&scope) {
        Ok(())
    } else {
        Err(WorkerError::ExecutionFailed {
            reason: "worker-local MCP requires kernel.yama.ptrace_scope >= 1".to_string(),
        })
    }
}

#[cfg(not(target_os = "linux"))]
async fn require_sibling_process_isolation() -> Result<(), WorkerError> {
    Err(WorkerError::ExecutionFailed {
        reason: "worker-local MCP requires Linux Yama sibling isolation".to_string(),
    })
}

fn ptrace_scope_isolates_siblings(value: &str) -> bool {
    value.trim().parse::<u8>().is_ok_and(|scope| scope >= 1)
}

fn validate_aggregate_discovery(
    current_tools: usize,
    current_bytes: usize,
    discovered_tools: usize,
    discovered_bytes: usize,
) -> Result<usize, String> {
    let total_tools = current_tools
        .checked_add(discovered_tools)
        .filter(|total| *total <= MAX_WORKER_MCP_TOOLS)
        .ok_or_else(|| {
            format!("worker MCP aggregate tool limit exceeded (max {MAX_WORKER_MCP_TOOLS})")
        })?;
    let total_bytes = current_bytes
        .checked_add(discovered_bytes)
        .filter(|total| *total <= MAX_WORKER_MCP_TOOL_BYTES)
        .ok_or_else(|| {
            format!(
                "worker MCP aggregate tool descriptors exceed {MAX_WORKER_MCP_TOOL_BYTES} bytes"
            )
        })?;
    debug_assert!(total_tools <= MAX_WORKER_MCP_TOOLS);
    Ok(total_bytes)
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

    fn empty_runtime() -> WorkerMcpRuntime {
        WorkerMcpRuntime {
            shared: Arc::new(WorkerMcpShared {
                registry: Arc::new(ToolRegistry::new()),
                sessions: Arc::new(McpSessionManager::new()),
                processes: Arc::new(McpProcessManager::new()),
                owner_id: "owner".to_string(),
                servers: Mutex::new(HashMap::new()),
            }),
            supervisor_cancel: CancellationToken::new(),
            supervisor_handles: Vec::new(),
        }
    }

    fn fake_server_state(healthy_since: Instant, restart_attempts: u32) -> WorkerMcpServer {
        WorkerMcpServer {
            config: server(),
            secret_env: HashMap::new(),
            client: McpClient::new("http://127.0.0.1:9"),
            wrappers: Vec::new(),
            descriptor_bytes: 0,
            restart_attempts,
            healthy_since,
        }
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

    #[test]
    fn aggregate_discovery_limits_span_all_servers() {
        let bytes = validate_aggregate_discovery(128, 1024, 128, 1024).unwrap();
        assert_eq!(bytes, 2048);
        assert!(validate_aggregate_discovery(128, 0, 129, 0).is_err());
        assert!(validate_aggregate_discovery(0, MAX_WORKER_MCP_TOOL_BYTES, 0, 1,).is_err());
    }

    #[test]
    fn yama_scope_must_block_same_uid_siblings() {
        assert!(!ptrace_scope_isolates_siblings("0\n"));
        assert!(ptrace_scope_isolates_siblings("1\n"));
        assert!(ptrace_scope_isolates_siblings("3"));
        assert!(!ptrace_scope_isolates_siblings("unknown"));
    }

    #[test]
    fn restart_claim_opens_circuit_after_max_attempts() {
        let mut state = fake_server_state(Instant::now(), 0);
        for expected in 1..=MCP_RESTART_MAX_ATTEMPTS {
            assert_eq!(claim_worker_mcp_restart(&mut state), Some(expected));
        }
        assert_eq!(claim_worker_mcp_restart(&mut state), None);
        assert_eq!(state.restart_attempts, MCP_RESTART_MAX_ATTEMPTS);
        assert_eq!(claim_worker_mcp_restart(&mut state), None);
    }

    #[test]
    fn restart_claim_resets_after_stability_window() {
        let mut state = fake_server_state(Instant::now() - MCP_RESTART_STABILITY_WINDOW, 3);
        assert_eq!(claim_worker_mcp_restart(&mut state), Some(1));
    }

    #[test]
    fn restart_backoff_stays_within_configured_bounds() {
        for attempt in 1..=8 {
            let capped = (MCP_RESTART_BACKOFF_BASE * (1_u32 << (attempt - 1).min(5)))
                .min(MCP_RESTART_BACKOFF_MAX);
            for _ in 0..64 {
                let delay = worker_mcp_restart_backoff(attempt);
                assert!(delay <= capped);
                assert!(delay >= Duration::from_millis(1));
            }
        }
    }

    #[tokio::test]
    async fn empty_runtime_shutdown_is_bounded_and_idempotent() {
        let mut runtime = empty_runtime();
        runtime.shutdown().await.unwrap();
        runtime.shutdown().await.unwrap();
    }

    /// Minimal newline-delimited JSON-RPC stdio server: answers `initialize`
    /// with an empty result and `tools/list` with no tools, records every
    /// spawn, then dies after `$DIE_AFTER` seconds.
    #[cfg(target_os = "linux")]
    const FAKE_SERVER_SCRIPT: &str = r#"
echo spawn >> "$SPAWN_LOG"
( sleep "$DIE_AFTER"; kill "$$" ) &
while IFS= read -r line; do
  case "$line" in
    *tools/list*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[]}}\n' "$id"
      ;;
    *'"id"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
      ;;
  esac
done
"#;

    #[cfg(target_os = "linux")]
    async fn supervision_preconditions_met() -> bool {
        match tokio::fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope").await {
            Ok(scope) => ptrace_scope_isolates_siblings(&scope),
            Err(_) => false,
        }
    }

    #[cfg(target_os = "linux")]
    fn spawn_log_line_count(spawn_log: &std::path::Path) -> usize {
        std::fs::read_to_string(spawn_log)
            .map(|content| content.lines().count())
            .unwrap_or(0)
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn supervisor_restarts_exited_child_until_shutdown() {
        if !supervision_preconditions_met().await {
            return;
        }
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or(0);
        let spawn_log = std::env::temp_dir().join(format!(
            "worker-mcp-supervisor-{}-{unique}.log",
            std::process::id()
        ));
        let env = HashMap::from([
            (
                "SPAWN_LOG".to_string(),
                spawn_log.to_string_lossy().to_string(),
            ),
            ("DIE_AFTER".to_string(), "2".to_string()),
        ]);
        let config = McpServerConfig::new_stdio(
            "fake",
            "sh",
            vec!["-c".to_string(), FAKE_SERVER_SCRIPT.to_string()],
            env,
        )
        .with_placement(McpPlacement::JobWorker);
        let registry = Arc::new(ToolRegistry::new());
        let mut runtime = WorkerMcpRuntime::activate(registry, vec![config], Vec::new(), "owner")
            .await
            .expect("activate fake server");

        let deadline = Instant::now() + Duration::from_secs(20);
        let spawns = loop {
            let count = spawn_log_line_count(&spawn_log);
            if count >= 2 {
                break count;
            }
            assert!(
                Instant::now() < deadline,
                "supervisor did not restart the exited child (spawns={count})"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        };
        assert!(spawns >= 2, "expected at least one restart, saw {spawns}");
        assert!(
            runtime
                .restart_attempts("fake")
                .await
                .is_some_and(|attempts| attempts >= 1)
        );

        runtime.shutdown().await.expect("shutdown");
        let stable = spawn_log_line_count(&spawn_log);
        tokio::time::sleep(MCP_SUPERVISOR_INTERVAL * 2 + Duration::from_secs(1)).await;
        assert_eq!(
            stable,
            spawn_log_line_count(&spawn_log),
            "no restarts may occur after shutdown"
        );
        let _ = std::fs::remove_file(&spawn_log);
    }
}
