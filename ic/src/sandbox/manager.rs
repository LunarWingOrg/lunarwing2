//! Main sandbox manager coordinating the container runner and selected egress.
//!
//! The `SandboxManager` is the primary entry point for sandboxed execution.
//! It coordinates:
//! - Container lifecycle via the podman/docker CLI runner (no daemon socket)
//! - HTTP proxy for network access control (proxy network modes)
//! - Health-gated tunnel sidecar lifecycle (tunnel network mode)
//! - Credential injection for API calls
//! - Resource limits and timeouts
//!
//! # Architecture
//!
//! ```text
//! ┌───────────────────────────────────────────────────────────────────────────┐
//! │                           SandboxManager                                   │
//! │                                                                            │
//! │   execute(cmd, cwd, policy)                                                │
//! │         │                                                                  │
//! │         ▼                                                                  │
//! │   ┌──────────────┐     ┌──────────────┐     ┌──────────────────────────┐  │
//! │   │ Start Proxy  │────▶│ Run          │────▶│ Collect Output & Cleanup │  │
//! │   │ (if needed)  │     │ Container    │     │                          │  │
//! │   └──────────────┘     └──────────────┘     └──────────────────────────┘  │
//! └───────────────────────────────────────────────────────────────────────────┘
//! ```

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;

use crate::sandbox::cli_runner::{CliRunner, ExecParams, TunnelRunConfig};
use crate::sandbox::config::{ResourceLimits, SandboxConfig, SandboxNetworkMode, SandboxPolicy};
use crate::sandbox::error::{Result, SandboxError};
use crate::sandbox::proxy::{HttpProxy, NetworkProxyBuilder};
use crate::sandbox::tunnel_sidecar::{TunnelSidecar, TunnelSidecarConfig};

/// Output from sandbox execution.
#[derive(Debug, Clone)]
pub struct ExecOutput {
    /// Exit code from the command.
    pub exit_code: i64,
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
    /// Combined output (stdout + stderr).
    pub output: String,
    /// How long the command ran.
    pub duration: Duration,
    /// Whether output was truncated.
    pub truncated: bool,
}

impl From<crate::sandbox::container::ContainerOutput> for ExecOutput {
    fn from(c: crate::sandbox::container::ContainerOutput) -> Self {
        let output = if c.stderr.is_empty() {
            c.stdout.clone()
        } else if c.stdout.is_empty() {
            c.stderr.clone()
        } else {
            format!("{}\n\n--- stderr ---\n{}", c.stdout, c.stderr)
        };

        Self {
            exit_code: c.exit_code,
            stdout: c.stdout,
            stderr: c.stderr,
            output,
            duration: c.duration,
            truncated: c.truncated,
        }
    }
}

/// Main sandbox manager.
///
/// BOUNDARY: this manager powers the generic (shell-tool) sandbox via the
/// podman/docker CLI runner. It is intentionally NOT connected to the
/// orchestrator's worker-container path (`ContainerJobManager`, bollard).
pub struct SandboxManager {
    config: SandboxConfig,
    proxy: Arc<RwLock<Option<HttpProxy>>>,
    runner: Arc<RwLock<Option<Arc<CliRunner>>>>,
    /// Tunnel sidecar handle when `network` is `Tunnel` (WI#248); stopped
    /// on shutdown.
    tunnel: RwLock<Option<TunnelSidecar>>,
    initialization: tokio::sync::Mutex<()>,
    initialized: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    test_runner: Option<Arc<CliRunner>>,
}

impl SandboxManager {
    /// Create a new sandbox manager.
    pub fn new(config: SandboxConfig) -> Self {
        Self {
            config,
            proxy: Arc::new(RwLock::new(None)),
            runner: Arc::new(RwLock::new(None)),
            tunnel: RwLock::new(None),
            initialization: tokio::sync::Mutex::new(()),
            initialized: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            test_runner: None,
        }
    }

    /// Create with an instance-scoped tunnel sidecar for deterministic integration tests.
    #[doc(hidden)]
    pub fn new_with_tunnel_sidecar(config: SandboxConfig, sidecar: TunnelSidecar) -> Self {
        let mut manager = Self::new(config);
        manager.tunnel = RwLock::new(Some(sidecar));
        manager
    }

    /// Create with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(SandboxConfig::default())
    }

    /// Check if the sandbox is available (container runtime CLI present).
    pub async fn is_available(&self) -> bool {
        if !self.config.enabled {
            return false;
        }

        CliRunner::detect(self.config.runtime, self.config.image.clone())
            .await
            .is_ok()
    }

    async fn runner_for_initialize(&self) -> Result<Arc<CliRunner>> {
        #[cfg(test)]
        if let Some(runner) = &self.test_runner {
            return Ok(runner.clone());
        }
        Ok(Arc::new(
            CliRunner::detect(self.config.runtime, self.config.image.clone()).await?,
        ))
    }

    /// Initialize the sandbox (detect runtime CLI, verify image, start proxy).
    pub async fn initialize(&self) -> Result<()> {
        if self.initialized.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(());
        }
        let _initialization = self.initialization.lock().await;
        if self.initialized.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(());
        }

        if !self.config.enabled {
            return Err(SandboxError::Config {
                reason: "sandbox is disabled".to_string(),
            });
        }

        // Detect the runtime CLI (podman preferred, docker fallback)
        let runner = self.runner_for_initialize().await?;

        // Check for / pull the image. Inspection failures are not absence:
        // rootless podman can fail here when its storage filesystem is full.
        if !runner.image_exists().await? {
            if self.config.auto_pull_image {
                runner.pull_image().await?;
            } else {
                return Err(SandboxError::ImageUnavailable {
                    image: self.config.image.clone(),
                    reason: "image not found locally and auto-pull is disabled".to_string(),
                });
            }
        }

        *self.runner.write().await = Some(runner);

        let tunnel_start = self.initialize_tunnel().await;

        // Start the network proxy for the proxy-backed egress modes —
        // advisory and enforced both need the validating proxy. The tunnel
        // mode needs no proxy (egress rides the sidecar's namespace), and
        // `none` starts nothing. Per-call policy overrides (including a
        // FullAccess manager default with sandboxed per-call policies) use
        // the same never-relaxed proxy ACL.
        if matches!(
            self.config.network,
            SandboxNetworkMode::Proxy | SandboxNetworkMode::ProxyEnforced
        ) {
            let proxy = NetworkProxyBuilder::from_config(&self.config)
                .build_and_start(self.config.proxy_port)
                .await?;

            *self.proxy.write().await = Some(proxy);
        }

        self.initialized
            .store(true, std::sync::atomic::Ordering::SeqCst);
        tunnel_start?;

        // Resolve the runtime name before the macro: the read guard's await
        // inside `tracing::info!` field position would make the future !Send.
        let runtime_bin = self
            .runner
            .read()
            .await
            .as_ref()
            .map(|r| r.runtime().binary())
            .unwrap_or("unknown");
        tracing::info!(runtime = runtime_bin, "Sandbox initialized");
        Ok(())
    }

    async fn initialize_tunnel(&self) -> Result<()> {
        if self.config.network != SandboxNetworkMode::Tunnel {
            return Ok(());
        }
        let sidecar = {
            let mut tunnel = self.tunnel.write().await;
            match tunnel.as_ref() {
                Some(sidecar) => sidecar.clone(),
                None => {
                    let config =
                        self.config
                            .tunnel
                            .clone()
                            .ok_or_else(|| SandboxError::Config {
                                reason:
                                    "SANDBOX_NETWORK=tunnel requires SANDBOX_TUNNEL_* settings \
                                 (see docs/guides/SANDBOX_TUNNEL_NETWORKING.md)"
                                        .to_string(),
                            })?;
                    let runtime = self
                        .runner
                        .read()
                        .await
                        .as_ref()
                        .map(|runner| runner.runtime())
                        .ok_or_else(|| SandboxError::RuntimeNotAvailable {
                            reason: "container runtime not initialized".to_string(),
                        })?;
                    let sidecar = TunnelSidecar::new(runtime, config);
                    *tunnel = Some(sidecar.clone());
                    sidecar
                }
            }
        };
        sidecar.ensure_started().await
    }

    /// Shutdown the sandbox (stop proxy and tunnel sidecar, clean up).
    pub async fn shutdown(&self) {
        let _initialization = self.initialization.lock().await;
        if let Some(proxy) = self.proxy.write().await.take() {
            proxy.stop().await;
        }
        if let Some(sidecar) = self.tunnel.write().await.take() {
            sidecar.stop().await;
        }

        self.initialized
            .store(false, std::sync::atomic::Ordering::SeqCst);

        tracing::debug!("Sandbox shut down");
    }

    /// Execute a command in the sandbox.
    pub async fn execute(
        &self,
        command: &str,
        cwd: &Path,
        env: HashMap<String, String>,
    ) -> Result<ExecOutput> {
        self.execute_with_policy(command, cwd, self.config.policy, env)
            .await
    }

    /// Execute a command with a specific policy.
    pub async fn execute_with_policy(
        &self,
        command: &str,
        cwd: &Path,
        policy: SandboxPolicy,
        env: HashMap<String, String>,
    ) -> Result<ExecOutput> {
        // FullAccess policy bypasses the sandbox entirely.
        // Double-check the allow_full_access guard at execution time as well,
        // in case the policy was overridden per-call via execute_with_policy().
        if policy == SandboxPolicy::FullAccess {
            if !self.config.allow_full_access {
                tracing::error!(
                    "FullAccess execution requested but SANDBOX_ALLOW_FULL_ACCESS is not \
                     enabled. Refusing to execute on host. Falling back to error."
                );
                return Err(SandboxError::Config {
                    reason: "FullAccess policy requires SANDBOX_ALLOW_FULL_ACCESS=true".to_string(),
                });
            }
            // Log only the binary name to avoid leaking secrets embedded in
            // command arguments (e.g. tokens in curl headers).
            let binary = command.split_whitespace().next().unwrap_or("<empty>");
            tracing::warn!(
                binary = %binary,
                cwd = %cwd.display(),
                "[FullAccess] Executing command directly on host (no sandbox isolation)"
            );
            return self.execute_direct(command, cwd, env).await;
        }

        // Ensure we're initialized
        if !self.initialized.load(std::sync::atomic::Ordering::SeqCst) {
            self.initialize().await?;
        }

        // Retry transient container failures (Docker daemon glitches, container
        // creation races) up to MAX_SANDBOX_RETRIES times with exponential backoff.
        const MAX_SANDBOX_RETRIES: u32 = 2;
        let mut last_err: Option<SandboxError> = None;

        for attempt in 0..=MAX_SANDBOX_RETRIES {
            if attempt > 0 {
                let delay = std::time::Duration::from_secs(1 << attempt); // 2s, 4s
                tracing::warn!(
                    attempt = attempt + 1,
                    max_attempts = MAX_SANDBOX_RETRIES + 1,
                    delay_secs = delay.as_secs(),
                    "Retrying sandbox execution after transient failure"
                );
                tokio::time::sleep(delay).await;
            }

            match self
                .try_execute_in_container(command, cwd, policy, env.clone())
                .await
            {
                Ok(output) => return Ok(output),
                Err(e) if is_transient_sandbox_error(&e) => {
                    tracing::warn!(
                        attempt = attempt + 1,
                        error = %e,
                        "Transient sandbox error, will retry"
                    );
                    last_err = Some(e);
                }
                Err(e) => return Err(e),
            }
        }

        Err(last_err.unwrap_or_else(|| SandboxError::ExecutionFailed {
            reason: "all retry attempts exhausted".to_string(),
        }))
    }

    /// Single attempt at container execution (no retry logic).
    async fn try_execute_in_container(
        &self,
        command: &str,
        cwd: &Path,
        policy: SandboxPolicy,
        env: HashMap<String, String>,
    ) -> Result<ExecOutput> {
        // Containment: the mounted directory is agent-chosen, so refuse any
        // path resolving into the tenant secret directory (or a bare secret
        // file) before the runtime is ever asked to mount it. Fail closed —
        // the caller must not fall back to an unsandboxed run (WI#205).
        let cwd =
            crate::sandbox::mount_guard::validate_mount_dir(cwd, &self.config.protected_paths)?;

        let tunnel_run = if self.config.network == SandboxNetworkMode::Tunnel {
            let tunnel = self.tunnel.read().await;
            let sidecar = tunnel
                .as_ref()
                .ok_or_else(|| SandboxError::TunnelUnavailable {
                    reason: "sidecar is not initialized; refusing sandbox execution".into(),
                })?;
            // Join only the exact container ID that passed inspection. If a
            // concurrent recreation removes it, this run fails instead of
            // joining a new namespace before its firewall is installed.
            let container_id = sidecar.container_for_run().await?;
            let dns = self
                .config
                .tunnel
                .as_ref()
                .map(|config| config.dns.clone())
                .unwrap_or_default();
            let resolv_conf = sidecar.tunnel_resolv_conf()?;
            Some((container_id, dns, resolv_conf))
        } else {
            None
        };

        let proxy_port = if let Some(proxy) = self.proxy.read().await.as_ref() {
            proxy.addr().await.map(|a| a.port()).unwrap_or(0)
        } else {
            0
        };

        let runner =
            self.runner
                .read()
                .await
                .clone()
                .ok_or_else(|| SandboxError::RuntimeNotAvailable {
                    reason: "container runtime not initialized".to_string(),
                })?;

        let limits = ResourceLimits {
            memory_bytes: self.config.memory_limit_mb * 1024 * 1024,
            cpu_shares: self.config.cpu_shares,
            timeout: self.config.timeout,
            max_output_bytes: 64 * 1024,
        };

        let container_output = runner
            .execute(ExecParams {
                command,
                working_dir: &cwd,
                policy,
                limits: &limits,
                env: &env,
                network: self.config.network,
                proxy_port,
                tunnel: tunnel_run
                    .as_ref()
                    .map(|(id, dns, resolv_conf)| TunnelRunConfig {
                        container_id: id,
                        dns: dns.as_slice(),
                        resolv_conf: resolv_conf.as_deref(),
                    }),
                protected_paths: &self.config.protected_paths,
                tenant_logs: self.config.tenant_logs.enabled(),
            })
            .await?;
        Ok(container_output.into())
    }

    /// Execute a command directly on the host (no sandbox).
    ///
    /// The child environment is scrubbed to the same safe allowlist the shell
    /// tool's direct path uses (CWE-200): the daemon's full environment —
    /// including `DATABASE_URL` and other credentials — is never inherited by
    /// agent-invoked processes, even under the double-opt-in `FullAccess`
    /// policy (WI#141). Caller-provided `env` entries are applied on top of
    /// the scrubbed base.
    async fn execute_direct(
        &self,
        command: &str,
        cwd: &Path,
        env: HashMap<String, String>,
    ) -> Result<ExecOutput> {
        let start = std::time::Instant::now();

        // Spawn through the workload spawner: fresh process group +
        // kill_on_drop always, systemd transient-service isolation when
        // available (BUG-005, WI#178/WI#181). The guard reaps the whole
        // tree on timeout or drop.
        let spawner = crate::tools::process::workload_spawner().await;
        let mut cmd = if cfg!(target_os = "windows") {
            spawner.command("cmd", &["/C", command])
        } else {
            spawner.command("sh", &["-c", command])
        };

        cmd.current_dir(cwd);
        // Scrub environment to prevent secret leakage (CWE-200). Only
        // known-safe variables are forwarded; everything else (DB URLs, API
        // keys, session tokens, sandbox launcher vars) is stripped.
        cmd.env_clear();
        for var in crate::tools::builtin::shell::SAFE_ENV_VARS {
            if let Ok(val) = std::env::var(var) {
                cmd.env(var, val);
            }
        }
        // If the spawner synthesized XDG_RUNTIME_DIR for `systemd-run --user`,
        // re-apply it after the scrub or the user bus is unreachable.
        if let Some(dir) = spawner.xdg_runtime_dir() {
            cmd.env("XDG_RUNTIME_DIR", dir);
        }
        // Caller-provided env (e.g. credentials fetched by the worker runtime)
        // is explicitly provided and safe to forward on top of the scrubbed base.
        cmd.envs(env);

        // In service mode the manager forks the workload, so the scrubbed
        // environment and working directory must be handed over via
        // EnvironmentFile= (mode 0600, deleted after the run) and
        // --working-directory= instead of process inheritance. Rebuilds the
        // command, so stdio must be set after this call.
        let spec = spawner.prepare_service(&mut cmd);
        // wait_with_output only captures piped stdio (unlike Command::output,
        // which pipes implicitly): set the pipes explicitly or output is empty.
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let child = cmd.spawn().map_err(|e| SandboxError::ExecutionFailed {
            reason: e.to_string(),
        })?;
        let mut guard = crate::tools::process::ProcessTreeGuard::for_service(
            &child,
            spec.as_ref(),
            spawner.xdg_runtime_dir(),
        );

        let output = match tokio::time::timeout(self.config.timeout, child.wait_with_output()).await
        {
            Ok(output) => {
                guard.disarm();
                output.map_err(|e| SandboxError::ExecutionFailed {
                    reason: e.to_string(),
                })?
            }
            Err(_) => {
                // The dropped future's kill_on_drop reaps the direct child;
                // stop the transient unit (service mode) and SIGKILL the
                // rest of the process group (BUG-005).
                guard.kill_tree().await;
                return Err(SandboxError::Timeout(self.config.timeout));
            }
        };

        let max_output: usize = 64 * 1024; // 64 KB, matching container path
        let half_max = max_output / 2;

        let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let mut truncated = false;

        if stdout.len() > half_max {
            let end = crate::util::floor_char_boundary(&stdout, half_max);
            stdout.truncate(end);
            truncated = true;
        }
        if stderr.len() > half_max {
            let end = crate::util::floor_char_boundary(&stderr, half_max);
            stderr.truncate(end);
            truncated = true;
        }

        let combined = if stderr.is_empty() {
            stdout.clone()
        } else if stdout.is_empty() {
            stderr.clone()
        } else {
            format!("{}\n\n--- stderr ---\n{}", stdout, stderr)
        };

        Ok(ExecOutput {
            exit_code: output.status.code().unwrap_or(-1) as i64,
            stdout,
            stderr,
            output: combined,
            duration: start.elapsed(),
            truncated,
        })
    }

    /// Execute a build command (convenience method using WorkspaceWrite policy).
    pub async fn build(
        &self,
        command: &str,
        project_dir: &Path,
        env: HashMap<String, String>,
    ) -> Result<ExecOutput> {
        self.execute_with_policy(command, project_dir, SandboxPolicy::WorkspaceWrite, env)
            .await
    }

    /// Get the current configuration.
    pub fn config(&self) -> &SandboxConfig {
        &self.config
    }

    /// Check if the sandbox is initialized.
    pub fn is_initialized(&self) -> bool {
        self.initialized.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Get the proxy port if running.
    pub async fn proxy_port(&self) -> Option<u16> {
        if let Some(proxy) = self.proxy.read().await.as_ref() {
            proxy.addr().await.map(|a| a.port())
        } else {
            None
        }
    }
}

impl Drop for SandboxManager {
    fn drop(&mut self) {
        // Note: async cleanup should be done via shutdown() before dropping
        if self.initialized.load(std::sync::atomic::Ordering::SeqCst) {
            tracing::warn!("SandboxManager dropped without shutdown(), resources may leak");
        }
    }
}

/// Check whether a sandbox error is transient and worth retrying.
///
/// Transient errors are those caused by runtime glitches, container
/// creation race conditions, or container start failures — not by command
/// execution failures, timeouts, or policy violations.
fn is_transient_sandbox_error(err: &SandboxError) -> bool {
    matches!(
        err,
        SandboxError::DockerNotAvailable { .. }
            | SandboxError::RuntimeNotAvailable { .. }
            | SandboxError::ImageUnavailable { .. }
            | SandboxError::ContainerCreationFailed { .. }
            | SandboxError::ContainerStartFailed { .. }
    )
}

/// Builder for creating a sandbox manager.
pub struct SandboxManagerBuilder {
    config: SandboxConfig,
}

impl SandboxManagerBuilder {
    /// Create a new builder.
    pub fn new() -> Self {
        Self {
            config: SandboxConfig::default(),
        }
    }

    /// Enable the sandbox.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.config.enabled = enabled;
        self
    }

    /// Set the sandbox policy.
    ///
    /// **Note:** `SandboxPolicy::FullAccess` additionally requires
    /// `allow_full_access(true)` to be set, or the manager will return
    /// `SandboxError::Config` at execution time. This is an intentional
    /// double opt-in to prevent accidental host execution.
    pub fn policy(mut self, policy: SandboxPolicy) -> Self {
        self.config.policy = policy;
        self
    }

    /// Explicitly allow FullAccess policy (double opt-in).
    pub fn allow_full_access(mut self, allow: bool) -> Self {
        self.config.allow_full_access = allow;
        self
    }

    /// Set the command timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.config.timeout = timeout;
        self
    }

    /// Set the memory limit in MB.
    pub fn memory_limit_mb(mut self, mb: u64) -> Self {
        self.config.memory_limit_mb = mb;
        self
    }

    /// Set the Docker image.
    pub fn image(mut self, image: &str) -> Self {
        self.config.image = image.to_string();
        self
    }

    /// Set the container runtime (default: auto — podman preferred).
    pub fn runtime(mut self, runtime: crate::sandbox::config::SandboxRuntime) -> Self {
        self.config.runtime = runtime;
        self
    }

    /// Set the network egress mode for sandboxed policies.
    pub fn network(mut self, network: SandboxNetworkMode) -> Self {
        self.config.network = network;
        self
    }

    /// Set the tunnel sidecar settings (only consulted when the network
    /// mode is [`SandboxNetworkMode::Tunnel`], WI#248).
    pub fn tunnel(mut self, tunnel: TunnelSidecarConfig) -> Self {
        self.config.tunnel = Some(tunnel);
        self
    }

    /// Add domains to the network allowlist.
    pub fn allow_domains(mut self, domains: Vec<String>) -> Self {
        self.config.network_allowlist.extend(domains);
        self
    }

    /// Build the sandbox manager.
    pub fn build(self) -> SandboxManager {
        SandboxManager::new(self.config)
    }

    /// Build and initialize the sandbox manager.
    pub async fn build_and_init(self) -> Result<SandboxManager> {
        let manager = self.build();
        manager.initialize().await?;
        Ok(manager)
    }
}

impl Default for SandboxManagerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::ContainerOutput;

    #[test]
    fn test_exec_output_from_container_output() {
        let container = ContainerOutput {
            exit_code: 0,
            stdout: "hello".to_string(),
            stderr: String::new(),
            duration: Duration::from_secs(1),
            truncated: false,
        };

        let exec: ExecOutput = container.into();
        assert_eq!(exec.exit_code, 0);
        assert_eq!(exec.output, "hello");
    }

    #[test]
    fn test_exec_output_combined() {
        let container = ContainerOutput {
            exit_code: 1,
            stdout: "out".to_string(),
            stderr: "err".to_string(),
            duration: Duration::from_secs(1),
            truncated: false,
        };

        let exec: ExecOutput = container.into();
        assert!(exec.output.contains("out"));
        assert!(exec.output.contains("err"));
        assert!(exec.output.contains("stderr"));
    }

    #[test]
    fn test_builder_defaults() {
        let manager = SandboxManagerBuilder::new().build();
        assert!(manager.config.enabled); // Enabled by default (startup check disables if Docker unavailable)
    }

    #[test]
    fn test_builder_custom() {
        let manager = SandboxManagerBuilder::new()
            .enabled(true)
            .policy(SandboxPolicy::WorkspaceWrite)
            .timeout(Duration::from_secs(60))
            .memory_limit_mb(1024)
            .image("custom:latest")
            .build();

        assert!(manager.config.enabled);
        assert_eq!(manager.config.policy, SandboxPolicy::WorkspaceWrite);
        assert_eq!(manager.config.timeout, Duration::from_secs(60));
        assert_eq!(manager.config.memory_limit_mb, 1024);
        assert_eq!(manager.config.image, "custom:latest");
    }

    #[tokio::test]
    async fn test_direct_execution() {
        let manager = SandboxManager::new(SandboxConfig {
            enabled: true,
            policy: SandboxPolicy::FullAccess,
            allow_full_access: true,
            ..Default::default()
        });

        let result = manager
            .execute("echo hello", Path::new("."), HashMap::new())
            .await;

        // This should work even without Docker since FullAccess runs directly
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.stdout.contains("hello"));
    }

    #[tokio::test]
    async fn test_direct_execution_blocked_without_allow() {
        let manager = SandboxManager::new(SandboxConfig {
            enabled: true,
            policy: SandboxPolicy::FullAccess,
            allow_full_access: false,
            ..Default::default()
        });

        let result = manager
            .execute("echo hello", Path::new("."), HashMap::new())
            .await;

        // Should be rejected because allow_full_access is false
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("SANDBOX_ALLOW_FULL_ACCESS"),
            "Error should mention SANDBOX_ALLOW_FULL_ACCESS, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_builder_full_access_without_allow_returns_error() {
        let manager = SandboxManagerBuilder::new()
            .enabled(true)
            .policy(SandboxPolicy::FullAccess)
            // Deliberately omitting .allow_full_access(true)
            .build();

        let result = manager
            .execute("echo hello", Path::new("."), HashMap::new())
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("SANDBOX_ALLOW_FULL_ACCESS"),
            "Error should mention SANDBOX_ALLOW_FULL_ACCESS, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_direct_execution_truncates_large_output() {
        let manager = SandboxManager::new(SandboxConfig {
            enabled: true,
            policy: SandboxPolicy::FullAccess,
            allow_full_access: true,
            ..Default::default()
        });

        // Generate output larger than 32KB (half of 64KB limit)
        // printf repeats a 100-char line 400 times = 40KB
        let result = manager
            .execute(
                "printf 'A%.0s' $(seq 1 40000)",
                Path::new("."),
                HashMap::new(),
            )
            .await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.truncated);
        assert!(output.stdout.len() <= 32 * 1024);
    }

    // ── WI#141: FullAccess direct execution must scrub the environment ──

    async fn full_access_manager() -> SandboxManager {
        SandboxManager::new(SandboxConfig {
            enabled: true,
            policy: SandboxPolicy::FullAccess,
            allow_full_access: true,
            ..Default::default()
        })
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_direct_execution_scrubs_daemon_secrets_from_env() {
        // WI#141 repro: `env | grep DATABASE_URL` from an agent shell under
        // full_access policy leaked the postgres connection string.
        let _guard = crate::config::helpers::lock_env();
        // SAFETY: test-only, ENV_MUTEX held (single-threaded runtime).
        unsafe {
            std::env::set_var("DATABASE_URL", "postgres://lunarwing:hunter2@127.0.0.1/db");
            std::env::set_var("SANDBOX_POLICY", "full_access");
            std::env::set_var("SANDBOX_ALLOW_FULL_ACCESS", "true");
        }

        let manager = full_access_manager().await;
        let result = manager
            .execute("env", Path::new("."), HashMap::new())
            .await
            .expect("direct execution");

        assert!(
            !result.stdout.contains("hunter2"),
            "DATABASE_URL password leaked through FullAccess direct execution"
        );
        assert!(
            !result.stdout.contains("postgres://"),
            "DATABASE_URL leaked through FullAccess direct execution"
        );
        assert!(
            !result.stdout.contains("SANDBOX_POLICY"),
            "SANDBOX_POLICY launcher var leaked into the agent shell env"
        );
        assert!(
            !result.stdout.contains("SANDBOX_ALLOW_FULL_ACCESS"),
            "SANDBOX_ALLOW_FULL_ACCESS launcher var leaked into the agent shell env"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_direct_execution_forwards_safe_vars_and_caller_env() {
        let _guard = crate::config::helpers::lock_env();
        // SAFETY: test-only, ENV_MUTEX held (single-threaded runtime).
        unsafe { std::env::set_var("LUNARWING_TEST_141_SECRET", "leaky") };

        let manager = full_access_manager().await;
        let mut env = HashMap::new();
        env.insert(
            "LUNARWING_TEST_141_PASSTHROUGH".to_string(),
            "ok".to_string(),
        );
        let result = manager.execute("env", Path::new("."), env).await;

        assert!(result.is_ok());
        let output = result.unwrap();
        // Allowlist vars survive the scrub...
        assert!(
            output.stdout.contains("PATH="),
            "PATH must survive scrubbing"
        );
        // ...caller-provided env is applied on top...
        assert!(
            output.stdout.contains("LUNARWING_TEST_141_PASSTHROUGH=ok"),
            "caller env must be forwarded on top of the scrubbed base"
        );
        // ...and everything else from the daemon env is dropped.
        assert!(
            !output.stdout.contains("LUNARWING_TEST_141_SECRET"),
            "non-allowlisted daemon env must be scrubbed"
        );
    }

    #[cfg(unix)]
    use crate::sandbox::cli_runner::{ContainerRuntime, TestRuntime};

    #[cfg(unix)]
    #[derive(Default)]
    struct CountingRuntimeState {
        started: bool,
        hash: String,
        instance: String,
        listing: Option<String>,
        runs: usize,
        installs: usize,
        removals: usize,
        live_removals: usize,
        unexpected: Vec<Vec<String>>,
    }

    #[cfg(unix)]
    type SharedRuntimeState = std::sync::Arc<std::sync::Mutex<CountingRuntimeState>>;

    #[cfg(unix)]
    fn lock_runtime_state(
        state: &SharedRuntimeState,
    ) -> std::sync::MutexGuard<'_, CountingRuntimeState> {
        state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(unix)]
    fn fake_output(success: bool, stdout: String, stderr: String) -> std::process::Output {
        use std::os::unix::process::ExitStatusExt;
        std::process::Output {
            status: std::process::ExitStatus::from_raw(if success { 0 } else { 1 << 8 }),
            stdout: stdout.into_bytes(),
            stderr: stderr.into_bytes(),
        }
    }

    #[cfg(unix)]
    fn sidecar_label(args: &[String], label: &str) -> String {
        args.windows(2)
            .find_map(|pair| {
                let value = pair[1].strip_prefix(label)?;
                (pair[0] == "--label").then(|| value.trim_start_matches('=').to_string())
            })
            .unwrap_or_default()
    }

    #[cfg(unix)]
    async fn run_fake_sidecar(state: &SharedRuntimeState, args: &[String]) -> std::process::Output {
        tokio::time::sleep(Duration::from_millis(25)).await;
        let mut state = lock_runtime_state(state);
        state.runs += 1;
        state.started = true;
        state.hash = sidecar_label(args, "lunarwing.sandbox-tunnel-hash");
        state.instance = sidecar_label(args, "lunarwing.sandbox-tunnel-instance");
        state.listing = None;
        fake_output(true, String::new(), String::new())
    }

    #[cfg(unix)]
    fn inspect_fake_sidecar(state: &CountingRuntimeState) -> std::process::Output {
        let id = "2".repeat(64);
        let stdout = if state.started {
            format!("{id}|true|{}|{}\n", state.hash, state.instance)
        } else {
            format!("{id}|true|foreign|foreign\n")
        };
        fake_output(true, stdout, String::new())
    }

    #[cfg(unix)]
    fn exec_fake_sidecar(
        state: &mut CountingRuntimeState,
        args: &[String],
        stdin: Option<Vec<u8>>,
    ) -> std::process::Output {
        let interactive = args.get(1).map(String::as_str) == Some("-i");
        let command_index = if interactive { 3 } else { 2 };
        match args.get(command_index).map(String::as_str) {
            Some("netbird") => fake_output(state.started, String::new(), String::new()),
            Some("cat") => fake_output(true, "nameserver 9.9.9.9\n".into(), String::new()),
            Some("nft") if args.get(command_index + 1).map(String::as_str) == Some("-f") => {
                state.installs += 1;
                let script = stdin.and_then(|input| String::from_utf8(input).ok());
                let valid = interactive
                    && script.as_ref().is_some_and(|script| {
                        script.starts_with(
                            "add table inet lunarwing_sandbox_egress\ndelete table inet lunarwing_sandbox_egress\n",
                        ) && script.contains("chain input {")
                            && script.contains("chain output {")
                    });
                if valid {
                    state.listing =
                        script.map(|script| script.lines().skip(2).collect::<Vec<_>>().join("\n"));
                    fake_output(true, String::new(), String::new())
                } else {
                    state.unexpected.push(args.to_vec());
                    fake_output(false, String::new(), "invalid ruleset".into())
                }
            }
            Some("nft") => match &state.listing {
                Some(listing) if state.started => fake_output(true, listing.clone(), String::new()),
                _ => fake_output(false, String::new(), "table absent".into()),
            },
            _ => {
                state.unexpected.push(args.to_vec());
                fake_output(false, String::new(), "unexpected exec".into())
            }
        }
    }

    #[cfg(unix)]
    fn counting_runtime(state: SharedRuntimeState) -> TestRuntime {
        TestRuntime::new(move |args, stdin| {
            let state = state.clone();
            async move {
                if args.first().map(String::as_str) == Some("run") {
                    return Ok(run_fake_sidecar(&state, &args).await);
                }
                let mut state = lock_runtime_state(&state);
                let output = match args.first().map(String::as_str) {
                    Some("image") if args.iter().any(|arg| arg == "--format") => {
                        fake_output(true, format!("sha256:{}\n", "1".repeat(64)), String::new())
                    }
                    Some("image") | Some("volume") => {
                        fake_output(true, String::new(), String::new())
                    }
                    Some("inspect") => inspect_fake_sidecar(&state),
                    Some("container") => {
                        fake_output(true, format!("{}\n", "2".repeat(64)), String::new())
                    }
                    Some("exec") => exec_fake_sidecar(&mut state, &args, stdin),
                    Some("rm") => {
                        state.removals += 1;
                        if state.started && state.listing.is_some() {
                            state.live_removals += 1;
                        }
                        state.started = false;
                        state.listing = None;
                        fake_output(true, String::new(), String::new())
                    }
                    _ => {
                        state.unexpected.push(args.clone());
                        fake_output(false, String::new(), "unexpected runtime call".into())
                    }
                };
                Ok(output)
            }
        })
    }

    #[cfg(unix)]
    fn counting_manager(
        config: SandboxConfig,
        sidecar: TunnelSidecar,
        runtime: TestRuntime,
    ) -> SandboxManager {
        let mut manager = SandboxManager::new_with_tunnel_sidecar(config, sidecar);
        manager.test_runner = Some(Arc::new(CliRunner::for_test(
            ContainerRuntime::Podman,
            "localhost/sandbox-test:latest".into(),
            runtime,
        )));
        manager
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_initialize_starts_one_sidecar_and_installs_firewall_once() {
        let state = SharedRuntimeState::default();
        let runtime = counting_runtime(state.clone());
        let workdir = std::env::temp_dir().join(format!("lw-tunnel-init-{}", uuid::Uuid::new_v4()));
        let runtime_dir = workdir.join("runtime");
        std::fs::create_dir_all(&runtime_dir).unwrap();
        let tunnel_config = TunnelSidecarConfig {
            auto_pull_image: false,
            startup_timeout: Duration::from_secs(2),
            netbird_management_url: Some("https://netbird.example.com".into()),
            netbird_setup_key: Some(secrecy::SecretString::from("test-setup-key")),
            ..TunnelSidecarConfig::default()
        };
        let config = SandboxConfig {
            image: "localhost/sandbox-test:latest".into(),
            network: SandboxNetworkMode::Tunnel,
            tunnel: Some(tunnel_config.clone()),
            auto_pull_image: false,
            protected_paths: Vec::new(),
            tenant_logs: crate::sandbox::config::TenantLogsMode::Off,
            ..SandboxConfig::default()
        };
        let managers: Vec<Arc<SandboxManager>> = (0..2)
            .map(|_| {
                let sidecar = TunnelSidecar::for_instance_with_runtime_dir(
                    ContainerRuntime::Podman,
                    tunnel_config.clone(),
                    &workdir,
                    &runtime_dir,
                )
                .with_test_runtime(runtime.clone());
                Arc::new(counting_manager(config.clone(), sidecar, runtime.clone()))
            })
            .collect();

        let starts = (0..8).map(|index| {
            let manager = managers[index % managers.len()].clone();
            tokio::spawn(async move { manager.initialize().await })
        });
        for result in futures::future::join_all(starts).await {
            result
                .expect("initialization task")
                .expect("initialization");
        }

        {
            let state = lock_runtime_state(&state);
            assert_eq!(state.runs, 1, "concurrent starts ran the sidecar twice");
            assert_eq!(state.installs, 1, "firewall was installed more than once");
            assert_eq!(state.removals, 0, "a start removed a live container");
            assert_eq!(state.live_removals, 0);
            assert!(state.unexpected.is_empty(), "{:?}", state.unexpected);
        }
        for manager in &managers {
            manager.shutdown().await;
        }
        std::fs::remove_dir_all(workdir).unwrap();
    }

    #[test]
    fn transient_errors_are_retryable() {
        assert!(super::is_transient_sandbox_error(
            &SandboxError::DockerNotAvailable {
                reason: "daemon restarting".to_string()
            }
        ));
        assert!(super::is_transient_sandbox_error(
            &SandboxError::RuntimeNotAvailable {
                reason: "cli went missing".to_string()
            }
        ));
        assert!(super::is_transient_sandbox_error(
            &SandboxError::ImageUnavailable {
                image: "img".to_string(),
                reason: "registry flake".to_string()
            }
        ));
        assert!(super::is_transient_sandbox_error(
            &SandboxError::ContainerCreationFailed {
                reason: "image pull glitch".to_string()
            }
        ));
        assert!(super::is_transient_sandbox_error(
            &SandboxError::ContainerStartFailed {
                reason: "cgroup race".to_string()
            }
        ));
    }

    #[test]
    fn non_transient_errors_are_not_retryable() {
        assert!(!super::is_transient_sandbox_error(&SandboxError::Timeout(
            std::time::Duration::from_secs(30)
        )));
        assert!(!super::is_transient_sandbox_error(
            &SandboxError::ExecutionFailed {
                reason: "exit code 1".to_string()
            }
        ));
        assert!(!super::is_transient_sandbox_error(
            &SandboxError::NetworkBlocked {
                reason: "policy violation".to_string()
            }
        ));
        assert!(!super::is_transient_sandbox_error(
            &SandboxError::StorageUnavailable {
                path: "/var/lib/containers/storage".to_string(),
                reason: "no space left on device".to_string()
            }
        ));
        assert!(!super::is_transient_sandbox_error(&SandboxError::Config {
            reason: "bad config".to_string()
        }));
        assert!(!super::is_transient_sandbox_error(
            &SandboxError::TunnelUnavailable {
                reason: "provider disconnected".to_string()
            }
        ));
    }
}
