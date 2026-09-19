//! CLI-driven container runner for the generic sandbox.
//!
//! Drives `podman run` (preferred) or `docker run` directly — the same
//! rootless-CLI pattern the mt-admin external-worker tooling uses — so the
//! sandbox needs no daemon API socket. This keeps it usable on hosts where
//! no `podman system service` / docker socket is running (notably OpenRC).
//!
//! # BOUNDARY (do not cross)
//!
//! This runner belongs to the **generic sandbox** (`crate::sandbox`) whose
//! only consumer is the shell tool's fail-closed execution path. It is NOT
//! part of the orchestrator's live worker-container path — that is
//! [`crate::orchestrator::job_manager::ContainerJobManager`] (bollard over
//! the Docker-compatible API). Never wire `CliRunner`/`SandboxManager` into
//! the orchestrator, and never route shell execution through
//! `ContainerJobManager`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::AsyncReadExt;

use crate::sandbox::config::{ResourceLimits, SandboxNetworkMode, SandboxPolicy, SandboxRuntime};
use crate::sandbox::container::ContainerOutput;
use crate::sandbox::error::{Result, SandboxError};

/// Label key the generic sandbox stamps on every container it starts.
///
/// The value is the container creation time (RFC 3339); the reaper's CLI
/// sweep uses it to age out orphans. Deliberately distinct from the
/// orchestrator's `lunarwing.job_id` label namespace.
pub const SANDBOX_CONTAINER_LABEL: &str = "lunarwing.sandbox";

/// Container runtime CLI being driven.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerRuntime {
    Podman,
    Docker,
}

impl ContainerRuntime {
    /// CLI binary name.
    pub fn binary(&self) -> &'static str {
        match self {
            ContainerRuntime::Podman => "podman",
            ContainerRuntime::Docker => "docker",
        }
    }

    /// Host alias that reaches host-loopback services from inside a container.
    ///
    /// Rootless podman (pasta/slirp4netns) maps `host.containers.internal`
    /// to the host's loopback, so the sandbox proxy can keep binding
    /// 127.0.0.1. Docker needs the `host-gateway` alias instead.
    fn host_alias(&self) -> &'static str {
        match self {
            ContainerRuntime::Podman => "host.containers.internal",
            ContainerRuntime::Docker => "host.docker.internal",
        }
    }
}

/// `--userns` support level, resolved once from `podman --version`.
///
/// The ladder is conservative: an unparsable or ancient version steps DOWN
/// (no `--userns` flag), never up. Rootless execution then fails closed if
/// keep-id cannot be supported, rather than silently breaking mount ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepIdMode {
    /// podman >= 5.0: `--userns keep-id:uid=1000,gid=1000` pins the in-container
    /// UID so the mounted workspace is owned by the sandbox user regardless of
    /// the host user's actual UID (same constants mt-admin uses for workers).
    UidPinned,
    /// podman >= 4.3: `--userns keep-id`.
    Plain,
    /// docker, unknown/ancient podman: omit the flag entirely.
    Off,
}

/// Parse `podman --version` output into `(major, minor)`.
///
/// Accepts the shapes seen in the wild:
/// - `podman version 5.2.3`
/// - `podman version 4.9.4 (Debian)`
/// - `podman version 5` (minor defaults to 0)
///
/// Anything else — other tools' output, garbage, empty input — returns `None`.
pub(crate) fn parse_podman_version(output: &str) -> Option<(u32, u32)> {
    let rest = output.trim().strip_prefix("podman")?.trim_start();
    let rest = rest.strip_prefix("version")?.trim_start();
    let token = rest.split_whitespace().next()?;
    let mut parts = token.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next().unwrap_or("0").parse::<u32>().unwrap_or(0);
    Some((major, minor))
}

/// Choose the `--userns` mode for a runtime from its (possibly unknown) version.
pub(crate) fn keep_id_mode_for(
    runtime: ContainerRuntime,
    version: Option<(u32, u32)>,
) -> KeepIdMode {
    match runtime {
        ContainerRuntime::Docker => KeepIdMode::Off,
        ContainerRuntime::Podman => match version {
            Some((major, _)) if major >= 5 => KeepIdMode::UidPinned,
            Some((4, minor)) if minor >= 3 => KeepIdMode::Plain,
            _ => KeepIdMode::Off,
        },
    }
}

/// Probe order for runtime detection, honoring the configured preference.
pub(crate) fn runtime_probe_order(preferred: SandboxRuntime) -> &'static [ContainerRuntime] {
    match preferred {
        SandboxRuntime::Docker => &[ContainerRuntime::Docker],
        SandboxRuntime::Podman => &[ContainerRuntime::Podman],
        SandboxRuntime::Auto => &[ContainerRuntime::Podman, ContainerRuntime::Docker],
    }
}

/// Inputs to [`build_run_args`]. Kept as a struct so the arg construction
/// stays a pure function (unit-testable without any runtime installed).
pub(crate) struct RunSpec<'a> {
    pub runtime: ContainerRuntime,
    pub keep_id: KeepIdMode,
    pub image: &'a str,
    pub name: &'a str,
    /// Value for the [`SANDBOX_CONTAINER_LABEL`] label (RFC 3339 timestamp).
    pub created_at_label: &'a str,
    pub command: &'a str,
    pub working_dir: &'a Path,
    pub policy: SandboxPolicy,
    pub limits: &'a ResourceLimits,
    pub env: &'a HashMap<String, String>,
    pub network: SandboxNetworkMode,
    pub proxy_port: u16,
    pub user: (u32, u32),
    pub proxy_dir: Option<&'a Path>,
}

/// Build the `<runtime> run ...` argument vector for a sandboxed command.
///
/// Hardening mirrors the old bollard `ContainerRunner`: non-root identity,
/// `--read-only` rootfs, tmpfs `/tmp` and cargo registry, all caps dropped
/// (CHOWN added back), `no-new-privileges`, memory/cpu limits, `--rm`
/// cleanup, plus the sandbox label for the reaper.
pub(crate) fn build_run_args(spec: &RunSpec<'_>) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--name".into(),
        spec.name.into(),
        "--label".into(),
        format!("{}={}", SANDBOX_CONTAINER_LABEL, spec.created_at_label),
        "--user".into(),
        format!("{}:{}", spec.user.0, spec.user.1),
        "--pids-limit".into(),
        "256".into(),
        "--workdir".into(),
        "/workspace".into(),
        "--entrypoint".into(),
        "sh".into(),
    ];

    match spec.keep_id {
        KeepIdMode::UidPinned => {
            args.push("--userns".into());
            args.push("keep-id:uid=1000,gid=1000".into());
        }
        KeepIdMode::Plain => {
            args.push("--userns".into());
            args.push("keep-id".into());
        }
        KeepIdMode::Off => {}
    }

    args.push("--memory".into());
    args.push(format!("{}m", spec.limits.memory_bytes / (1024 * 1024)));
    args.push("--cpu-shares".into());
    args.push(spec.limits.cpu_shares.to_string());

    // Security hardening (cross-runtime compatible forms).
    for flag in [
        "--cap-drop=ALL",
        "--cap-add=CHOWN",
        "--security-opt=no-new-privileges",
        "--read-only",
    ] {
        args.push(flag.into());
    }
    args.push("--tmpfs".into());
    args.push("/tmp:size=512m".into());
    args.push("--tmpfs".into());
    args.push("/home/sandbox/.cargo/registry:size=1g".into());

    // Workspace mount: policy decides ro/rw; `z` lets SELinux hosts relabel
    // the bind mount (harmless on non-SELinux hosts).
    let mode = if spec.policy.allows_writes() {
        "rw"
    } else {
        "ro"
    };
    args.push("-v".into());
    args.push(format!(
        "{}:/workspace:{mode},z",
        spec.working_dir.display()
    ));

    // Caller-provided env first (never secrets: the shell tool passes none).
    for (key, value) in spec.env {
        args.push("-e".into());
        args.push(format!("{key}={value}"));
    }

    append_network_args(spec, &mut args);
    args.push(spec.image.into());
    args.push("-c".into());
    if spec.network == SandboxNetworkMode::ProxyEnforced {
        args.extend([
            "command -v python3 >/dev/null || { echo 'lunarwing-proxy-relay: Python 3 required' >&2; exit 125; }; exec python3 -I -c \"$1\" \"$2\"".into(),
            "lunarwing-proxy-relay".into(),
            include_str!("proxy/relay.py").into(),
            spec.command.into(),
        ]);
    } else {
        args.push(spec.command.into());
    }
    args
}

fn append_network_args(spec: &RunSpec<'_>, args: &mut Vec<String>) {
    match spec.network {
        SandboxNetworkMode::None => {
            args.push("--network".into());
            args.push("none".into());
        }
        SandboxNetworkMode::Proxy => {
            if spec.proxy_port > 0 && spec.policy.is_sandboxed() {
                let host = spec.runtime.host_alias();
                let proxy_url = format!("http://{host}:{}", spec.proxy_port);
                for var in ["http_proxy", "https_proxy", "HTTP_PROXY", "HTTPS_PROXY"] {
                    args.push("-e".into());
                    args.push(format!("{var}={proxy_url}"));
                }
                if spec.runtime == ContainerRuntime::Docker {
                    // Docker's bridge gateway needs an explicit host alias.
                    args.push("--add-host".into());
                    args.push("host.docker.internal:host-gateway".into());
                }
            }
        }
        SandboxNetworkMode::ProxyEnforced => {
            args.extend(["--network".into(), "none".into()]);
            if let Some(dir) = spec.proxy_dir {
                args.extend([
                    "-v".into(),
                    format!("{}:/run/lunarwing-proxy:ro,z", dir.display()),
                ]);
            }
            for var in ["http_proxy", "https_proxy", "HTTP_PROXY", "HTTPS_PROXY"] {
                args.extend(["-e".into(), format!("{var}=http://127.0.0.1:3128")]);
            }
            // A caller-supplied bypass cannot accidentally disable proxy use.
            args.extend([
                "-e".into(),
                "NO_PROXY=".into(),
                "-e".into(),
                "no_proxy=".into(),
            ]);
        }
    }
}

/// Byte-capped output accumulator: keeps the first `cap` bytes and flags
/// truncation, so a runaway command cannot exhaust daemon memory. UTF-8 is
/// recovered losslessly up to a char boundary at the very end.
struct CappedBuf {
    data: Vec<u8>,
    cap: usize,
    truncated: bool,
}

impl CappedBuf {
    fn new(cap: usize) -> Self {
        Self {
            data: Vec::new(),
            cap,
            truncated: false,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if self.data.len() >= self.cap {
            self.truncated = true;
            return;
        }
        let remaining = self.cap - self.data.len();
        if bytes.len() <= remaining {
            self.data.extend_from_slice(bytes);
        } else {
            self.data.extend_from_slice(&bytes[..remaining]);
            self.truncated = true;
        }
    }

    /// Convert to a String, trimming an incomplete trailing UTF-8 sequence
    /// (a cap can split a multi-byte character). Returns the string and
    /// whether content was dropped.
    fn into_string(self) -> (String, bool) {
        let truncated = self.truncated;
        match String::from_utf8(self.data) {
            Ok(s) => (s, truncated),
            Err(e) => {
                let valid = e.utf8_error().valid_up_to();
                let s = String::from_utf8_lossy(&e.into_bytes()[..valid]).into_owned();
                (s, true)
            }
        }
    }
}

/// Drain a child pipe into a [`CappedBuf`]. Reads until EOF regardless of
/// the cap so the child never blocks on a full pipe.
async fn read_capped<R: tokio::io::AsyncRead + Unpin>(pipe: Option<R>, cap: usize) -> CappedBuf {
    let mut buf = CappedBuf::new(cap);
    let mut chunk = [0u8; 8192];
    let Some(mut pipe) = pipe else {
        return buf;
    };
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.push(&chunk[..n]),
        }
    }
    buf
}

/// Names of sandbox containers currently executing; the reaper's CLI sweep
/// consults this so it never kills a live execution.
static INFLIGHT: std::sync::LazyLock<std::sync::Mutex<Vec<String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(Vec::new()));

/// Snapshot of currently-executing sandbox container names.
pub fn inflight_sandbox_containers() -> Vec<String> {
    INFLIGHT
        .lock()
        .map(|names| names.clone())
        .unwrap_or_default()
}

fn register_inflight(name: &str) {
    if let Ok(mut names) = INFLIGHT.lock() {
        names.push(name.to_string());
    }
}

/// Owned by the supervisor, including cleanup. Dropping any execution future
/// (or the Tokio runtime itself) cannot permanently poison the in-flight set.
struct InflightGuard(String);

impl InflightGuard {
    fn new(name: String) -> Self {
        register_inflight(&name);
        Self(name)
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        unregister_inflight(&self.0);
    }
}

fn unregister_inflight(name: &str) {
    if let Ok(mut names) = INFLIGHT.lock() {
        names.retain(|n| n != name);
    }
}

/// Per-execution parameters for [`CliRunner::execute`].
pub struct ExecParams<'a> {
    pub command: &'a str,
    pub working_dir: &'a Path,
    pub policy: SandboxPolicy,
    pub limits: &'a ResourceLimits,
    pub env: &'a HashMap<String, String>,
    pub network: SandboxNetworkMode,
    pub proxy_port: u16,
}

/// Runs sandboxed commands via the podman/docker CLI.
#[derive(Clone)]
pub struct CliRunner {
    runtime: ContainerRuntime,
    keep_id: KeepIdMode,
    image: String,
    pub(crate) binary: PathBuf,
}

impl CliRunner {
    /// Detect an installed runtime CLI (honoring the configured preference)
    /// and probe its capabilities.
    pub async fn detect(preferred: SandboxRuntime, image: String) -> Result<Self> {
        for &runtime in runtime_probe_order(preferred) {
            if !binary_on_path(runtime.binary()) {
                continue;
            }
            let version = if runtime == ContainerRuntime::Podman {
                probe_podman_version().await
            } else {
                None
            };
            let keep_id = keep_id_mode_for(runtime, version);
            #[cfg(unix)]
            let keep_id = if nix::unistd::geteuid().is_root() {
                KeepIdMode::Off
            } else {
                keep_id
            };
            tracing::info!(
                runtime = runtime.binary(),
                podman_version = ?version,
                keep_id = ?keep_id,
                "Generic sandbox: using container runtime CLI"
            );
            return Ok(Self {
                runtime,
                keep_id,
                image,
                binary: PathBuf::from(runtime.binary()),
            });
        }
        Err(SandboxError::RuntimeNotAvailable {
            reason: format!("configured runtime {preferred:?} not found on PATH"),
        })
    }

    /// Runtime this runner drives.
    pub fn runtime(&self) -> ContainerRuntime {
        self.runtime
    }

    /// Check whether the sandbox image exists in the local store.
    pub async fn image_exists(&self) -> bool {
        run_status(&self.binary, &["image", "inspect", &self.image])
            .await
            .is_some_and(|s| s.success())
    }

    /// Pull the sandbox image from its registry.
    pub async fn pull_image(&self) -> Result<()> {
        tracing::info!("Pulling sandbox image: {}", self.image);
        let output = tokio::process::Command::new(&self.binary)
            .args(["pull", &self.image])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| SandboxError::ImageUnavailable {
                image: self.image.clone(),
                reason: format!("failed to spawn pull: {e}"),
            })?;

        if output.status.success() {
            tracing::info!("Successfully pulled sandbox image: {}", self.image);
            Ok(())
        } else {
            Err(SandboxError::ImageUnavailable {
                image: self.image.clone(),
                reason: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            })
        }
    }

    /// Execute a command in a fresh hardened container and collect its output.
    pub async fn execute(&self, params: ExecParams<'_>) -> Result<ContainerOutput> {
        // Ownership setup can fail without disabling orphan recovery on this
        // runtime. The CLI reaper needs neither an image nor a UID mapping.
        if self.runtime == ContainerRuntime::Docker {
            validate_docker_user_mapping(&self.binary).await?;
        }
        let ExecParams {
            command,
            working_dir,
            policy,
            limits,
            env,
            network,
            proxy_port,
        } = params;
        let start_time = std::time::Instant::now();
        let name = format!("lunarwing-sandbox-{}", uuid::Uuid::new_v4());
        let created_at = chrono::Utc::now().to_rfc3339();
        if network != SandboxNetworkMode::None && proxy_port == 0 {
            return Err(SandboxError::ProxyError {
                reason: "proxy network mode requires a running proxy".into(),
            });
        }
        let relay = if network == SandboxNetworkMode::ProxyEnforced {
            Some(crate::sandbox::proxy::relay::ProxyRelay::start(proxy_port).await?)
        } else {
            None
        };

        let spec = RunSpec {
            runtime: self.runtime,
            keep_id: self.keep_id,
            image: &self.image,
            name: &name,
            created_at_label: &created_at,
            command,
            working_dir,
            policy,
            limits,
            env,
            network,
            proxy_port,
            user: container_user(self.runtime, self.keep_id, working_dir, policy)?,
            proxy_dir: relay.as_ref().map(|relay| relay.path()),
        };
        let args = build_run_args(&spec);

        let inflight = InflightGuard::new(name);
        let runner = self.clone();
        let limits = limits.clone();
        // Closing this sender on outer timeout/task abort notifies an owned
        // supervisor. Cleanup is never inside the caller's droppable future.
        let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        let result = tokio::spawn(async move {
            let result = tokio::select! {
                biased;
                _ = cancel_rx => Err(SandboxError::ExecutionFailed {
                    reason: "sandbox execution cancelled; cleanup requested".into(),
                }),
                result = runner.run_container(&args, &limits) => result,
            };
            // The run future (including its process-tree guard and pipe reads)
            // has ended before removal starts. Retry to catch a runtime that
            // was still finishing container creation when its CLI was killed.
            runner.remove_container(&inflight.0).await;
            drop(relay);
            drop(inflight);
            result
        })
        .await
        .map_err(|e| SandboxError::ExecutionFailed {
            reason: format!("sandbox supervisor failed: {e}"),
        })?;

        result.map(|mut output| {
            output.duration = start_time.elapsed();
            output
        })
    }

    /// Spawn the run command, stream output into capped buffers, enforce the
    /// timeout, and return the container's exit code and output.
    async fn run_container(
        &self,
        args: &[String],
        limits: &ResourceLimits,
    ) -> Result<ContainerOutput> {
        // Spawn through the workload spawner: fresh process group +
        // kill_on_drop + systemd scope isolation when available (BUG-005).
        let spawner = crate::tools::process::workload_spawner().await;
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let binary = self.binary.to_str().ok_or_else(|| SandboxError::Config {
            reason: "runtime path is not UTF-8".into(),
        })?;
        let mut cmd = spawner.command(binary, &arg_refs);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SandboxError::RuntimeNotAvailable {
                    reason: format!("`{}` binary no longer on PATH: {e}", self.runtime.binary()),
                }
            } else {
                SandboxError::ExecutionFailed {
                    reason: format!("failed to spawn {}: {e}", self.runtime.binary()),
                }
            }
        })?;

        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let mut guard = crate::tools::process::ProcessTreeGuard::new(&child);

        // Drain both pipes concurrently so the child never blocks on a full
        // pipe; collection continues past the cap until EOF.
        let half_max = limits.max_output_bytes / 2;
        let waited = tokio::time::timeout(limits.timeout, async {
            tokio::join!(
                child.wait(),
                read_capped(stdout_pipe, half_max),
                read_capped(stderr_pipe, half_max),
            )
        })
        .await;

        match waited {
            Err(_) => {
                // Killing the CLI client does not stop the container (conmon
                // owns it). The supervisor removes it by name next.
                guard.kill_tree();
                Err(SandboxError::Timeout(limits.timeout))
            }
            Ok((Err(e), _, _)) => {
                guard.kill_tree();
                Err(SandboxError::ExecutionFailed {
                    reason: format!("container wait failed: {e}"),
                })
            }
            Ok((Ok(status), out_buf, err_buf)) => {
                guard.disarm();
                let exit_code = status.code().unwrap_or(-1) as i64;

                let (stdout, stdout_trunc) = out_buf.into_string();
                let (stderr, stderr_trunc) = err_buf.into_string();

                if exit_code != 0
                    && let Some(err) = classify_run_failure(&stderr, &self.image, exit_code)
                {
                    return Err(err);
                }

                Ok(ContainerOutput {
                    exit_code,
                    stdout,
                    stderr,
                    duration: Duration::ZERO, // Set by caller.
                    truncated: stdout_trunc || stderr_trunc,
                })
            }
        }
    }

    /// Bounded best-effort removal. Failed removal remains eligible for the
    /// independent reaper; timeout errors deliberately do not promise a kill.
    pub(crate) async fn remove_container(&self, name: &str) {
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            let status = run_status(&self.binary, &["rm", "-f", name]).await;
            if status.is_some_and(|s| s.success()) {
                return;
            }
        }
        tracing::debug!(container = %name, "Sandbox removal not confirmed (may already be gone); reaper will retry leftovers");
    }
}

/// Docker bind mounts preserve host numeric ownership only without daemon
/// userns remapping. Do not silently run as container root or chown a user's
/// workspace to compensate for a different mapping.
async fn validate_docker_user_mapping(binary: &Path) -> Result<()> {
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(binary)
            .args(["info", "--format", "{{json .SecurityOptions}}"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()
    .and_then(std::result::Result::ok)
    .filter(|o| o.status.success())
    .ok_or_else(|| SandboxError::RuntimeNotAvailable {
        reason: "cannot query Docker UID mapping; a local Linux daemon is required".into(),
    })?;
    let options: Vec<String> =
        serde_json::from_slice(&output.stdout).map_err(|e| SandboxError::Config {
            reason: format!("cannot parse Docker security options: {e}"),
        })?;
    if options
        .iter()
        .any(|o| o.contains("rootless") || o.contains("userns"))
    {
        return Err(SandboxError::Config {
            reason: "Docker rootless/userns-remap bind ownership is unsupported; use rootless Podman keep-id or a local Docker daemon without UID remapping".into(),
        });
    }
    Ok(())
}

fn container_user(
    runtime: ContainerRuntime,
    keep_id: KeepIdMode,
    workspace: &Path,
    policy: SandboxPolicy,
) -> Result<(u32, u32)> {
    if keep_id == KeepIdMode::UidPinned {
        return Ok((1000, 1000));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if keep_id == KeepIdMode::Plain {
            return Ok((
                nix::unistd::geteuid().as_raw(),
                nix::unistd::getegid().as_raw(),
            ));
        }
        if runtime == ContainerRuntime::Podman && !nix::unistd::geteuid().is_root() {
            return Err(SandboxError::Config {
                reason: "rootless Podman 4.3+ with keep-id is required for sandbox bind mounts"
                    .into(),
            });
        }
        if !nix::unistd::geteuid().is_root() {
            // A non-root daemon must never select another host user's identity
            // merely because a requested directory belongs to that user.
            return Ok((
                nix::unistd::geteuid().as_raw(),
                nix::unistd::getegid().as_raw(),
            ));
        }
        let meta = std::fs::metadata(workspace).map_err(|e| SandboxError::Config {
            reason: format!("cannot read workspace ownership: {e}"),
        })?;
        if meta.uid() != 0 && meta.gid() != 0 {
            return Ok((meta.uid(), meta.gid()));
        }
        if policy.allows_writes() {
            return Err(SandboxError::Config {
                reason: "writable sandbox workspace must be owned by a non-root UID/GID".into(),
            });
        }
    }
    #[cfg(not(unix))]
    let _ = (runtime, workspace, policy);
    Ok((1000, 1000))
}

/// Map a failed run's stderr to a typed error when it indicates a missing
/// image (e.g. the image vanished between the initialize-time check and the
/// run, or auto-pull is off).
fn classify_run_failure(stderr: &str, image: &str, exit_code: i64) -> Option<SandboxError> {
    if stderr.contains("lunarwing-proxy-relay:") {
        return Some(SandboxError::ProxyError {
            reason: stderr.trim().into(),
        });
    }
    let lower = stderr.to_lowercase();
    let image_missing = lower.contains("image not known")
        || lower.contains("no such image")
        || lower.contains("unable to find image")
        || lower.contains("pull access denied");
    if image_missing {
        return Some(SandboxError::ImageUnavailable {
            image: image.to_string(),
            reason: stderr.trim().to_string(),
        });
    }
    // Runtime setup failures (mounts, userns, resource controllers) are sandbox
    // errors, not a successful invocation with an ordinary command exit code.
    let runtime_error = exit_code == 125
        && lower
            .lines()
            .any(|line| line.starts_with("error:") || line.starts_with("docker: error"));
    runtime_error.then(|| SandboxError::ExecutionFailed {
        reason: stderr.trim().into(),
    })
}

/// Run a quick status probe, returning `None` when the binary can't spawn.
async fn run_status(bin: &Path, args: &[&str]) -> Option<std::process::ExitStatus> {
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(bin)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await
    .ok()
    .and_then(std::result::Result::ok)
}

/// Capture and parse `podman --version`.
async fn probe_podman_version() -> Option<(u32, u32)> {
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new("podman")
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    parse_podman_version(&String::from_utf8_lossy(&output.stdout))
}

/// Check a binary exists on PATH (`which` on Unix, `where` on Windows).
fn binary_on_path(bin: &str) -> bool {
    #[cfg(unix)]
    let lookup = "which";
    #[cfg(windows)]
    let lookup = "where";
    std::process::Command::new(lookup)
        .arg(bin)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    static LIMITS: std::sync::LazyLock<ResourceLimits> =
        std::sync::LazyLock::new(|| ResourceLimits {
            memory_bytes: 2048 * 1024 * 1024,
            cpu_shares: 1024,
            timeout: Duration::from_secs(30),
            max_output_bytes: 64 * 1024,
        });
    static EMPTY_ENV: std::sync::LazyLock<HashMap<String, String>> =
        std::sync::LazyLock::new(HashMap::new);

    fn spec<'a>(
        runtime: ContainerRuntime,
        keep_id: KeepIdMode,
        policy: SandboxPolicy,
        network: SandboxNetworkMode,
        proxy_port: u16,
    ) -> RunSpec<'a> {
        RunSpec {
            runtime,
            keep_id,
            image: "lunarwing-worker:latest",
            name: "lunarwing-sandbox-test",
            created_at_label: "2026-09-04T00:00:00Z",
            command: "echo hi",
            working_dir: Path::new("/tmp/proj"),
            policy,
            limits: &LIMITS,
            env: &EMPTY_ENV,
            network,
            proxy_port,
            user: (1000, 1000),
            proxy_dir: None,
        }
    }

    // ── podman version parsing (behavior-pinned) ──────────────────────

    #[test]
    fn parse_podman_version_table() {
        let cases: Vec<(&str, Option<(u32, u32)>)> = vec![
            ("podman version 5.2.3", Some((5, 2))),
            ("podman version 5.0.0\n", Some((5, 0))),
            ("podman version 5", Some((5, 0))),
            ("podman version 4.9.4 (Debian)", Some((4, 9))),
            ("podman version 4.3.1", Some((4, 3))),
            ("podman version 6.1.2", Some((6, 1))),
            ("podman version 4.2.0", Some((4, 2))),
            // Wrong tool / garbage / empty: never guess.
            ("docker version 24.0.7", None),
            ("podman version abc.def", None),
            ("hello world", None),
            ("", None),
            ("version 5.0.0", None),
        ];
        for (input, expected) in cases {
            assert_eq!(parse_podman_version(input), expected, "input: {input:?}");
        }
    }

    #[test]
    fn keep_id_ladder_is_conservative() {
        let podman = ContainerRuntime::Podman;
        let docker = ContainerRuntime::Docker;
        assert_eq!(
            keep_id_mode_for(podman, Some((5, 0))),
            KeepIdMode::UidPinned
        );
        assert_eq!(
            keep_id_mode_for(podman, Some((7, 2))),
            KeepIdMode::UidPinned
        );
        assert_eq!(keep_id_mode_for(podman, Some((4, 9))), KeepIdMode::Plain);
        assert_eq!(keep_id_mode_for(podman, Some((4, 3))), KeepIdMode::Plain);
        // Unknown or pre-4.3: step DOWN, never up.
        assert_eq!(keep_id_mode_for(podman, None), KeepIdMode::Off);
        assert_eq!(keep_id_mode_for(podman, Some((4, 2))), KeepIdMode::Off);
        assert_eq!(keep_id_mode_for(docker, Some((24, 0))), KeepIdMode::Off);
        assert_eq!(keep_id_mode_for(docker, None), KeepIdMode::Off);
    }

    #[test]
    fn runtime_probe_order_honors_preference() {
        assert_eq!(
            runtime_probe_order(SandboxRuntime::Auto),
            [ContainerRuntime::Podman, ContainerRuntime::Docker]
        );
        assert_eq!(
            runtime_probe_order(SandboxRuntime::Podman),
            [ContainerRuntime::Podman]
        );
        assert_eq!(
            runtime_probe_order(SandboxRuntime::Docker),
            [ContainerRuntime::Docker]
        );
    }

    // ── run arg construction ───────────────────────────────────────────

    #[test]
    fn args_readonly_policy_mounts_workspace_readonly() {
        let args = build_run_args(&spec(
            ContainerRuntime::Podman,
            KeepIdMode::UidPinned,
            SandboxPolicy::ReadOnly,
            SandboxNetworkMode::None,
            0,
        ));
        assert!(args.contains(&"/tmp/proj:/workspace:ro,z".to_string()));
        assert!(args.contains(&"--read-only".to_string()));
        assert!(!args.contains(&"/tmp/proj:/workspace:rw,z".to_string()));
    }

    #[test]
    fn args_workspace_write_mounts_rw() {
        let args = build_run_args(&spec(
            ContainerRuntime::Podman,
            KeepIdMode::UidPinned,
            SandboxPolicy::WorkspaceWrite,
            SandboxNetworkMode::Proxy,
            0,
        ));
        assert!(args.contains(&"/tmp/proj:/workspace:rw,z".to_string()));
    }

    #[test]
    fn args_hardening_flags_present() {
        let args = build_run_args(&spec(
            ContainerRuntime::Podman,
            KeepIdMode::Off,
            SandboxPolicy::ReadOnly,
            SandboxNetworkMode::None,
            0,
        ));
        for flag in [
            "--rm",
            "--user",
            "1000:1000",
            "--cap-drop=ALL",
            "--cap-add=CHOWN",
            "--security-opt=no-new-privileges",
            "--read-only",
        ] {
            assert!(args.contains(&flag.to_string()), "missing flag {flag}");
        }
        let tmpfs_idx = args.iter().position(|a| a == "--tmpfs").unwrap();
        assert_eq!(args[tmpfs_idx + 1], "/tmp:size=512m");
        let memory_idx = args.iter().position(|a| a == "--memory").unwrap();
        assert_eq!(args[memory_idx + 1], "2048m");
        let label_idx = args.iter().position(|a| a == "--label").unwrap();
        assert_eq!(
            args[label_idx + 1],
            "lunarwing.sandbox=2026-09-04T00:00:00Z"
        );
        // No keep-id on Off.
        assert!(!args.contains(&"--userns".to_string()));
    }

    #[test]
    fn args_userns_ladder() {
        let uid_pinned = build_run_args(&spec(
            ContainerRuntime::Podman,
            KeepIdMode::UidPinned,
            SandboxPolicy::ReadOnly,
            SandboxNetworkMode::None,
            0,
        ));
        let idx = uid_pinned.iter().position(|a| a == "--userns").unwrap();
        assert_eq!(uid_pinned[idx + 1], "keep-id:uid=1000,gid=1000");

        let plain = build_run_args(&spec(
            ContainerRuntime::Podman,
            KeepIdMode::Plain,
            SandboxPolicy::ReadOnly,
            SandboxNetworkMode::None,
            0,
        ));
        let idx = plain.iter().position(|a| a == "--userns").unwrap();
        assert_eq!(plain[idx + 1], "keep-id");
    }

    #[test]
    fn args_network_none_and_no_proxy_env() {
        let args = build_run_args(&spec(
            ContainerRuntime::Podman,
            KeepIdMode::Plain,
            SandboxPolicy::ReadOnly,
            SandboxNetworkMode::None,
            8080, // proxy port set but must be ignored in none mode
        ));
        let idx = args.iter().position(|a| a == "--network").unwrap();
        assert_eq!(args[idx + 1], "none");
        assert!(!args.iter().any(|a| a.starts_with("http_proxy=")));
        assert!(!args.iter().any(|a| a.starts_with("https_proxy=")));
    }

    #[test]
    fn args_proxy_mode_sets_podman_host_alias_env() {
        let args = build_run_args(&spec(
            ContainerRuntime::Podman,
            KeepIdMode::Plain,
            SandboxPolicy::WorkspaceWrite,
            SandboxNetworkMode::Proxy,
            3128,
        ));
        for var in ["http_proxy=", "https_proxy=", "HTTP_PROXY=", "HTTPS_PROXY="] {
            let entry = args
                .iter()
                .find(|a| a.starts_with(var))
                .unwrap_or_else(|| panic!("missing {var}"));
            assert!(
                entry.ends_with("http://host.containers.internal:3128"),
                "{entry}"
            );
        }
        assert!(!args.contains(&"--add-host".to_string()));
    }

    #[test]
    fn args_proxy_mode_docker_uses_host_gateway_alias() {
        let args = build_run_args(&spec(
            ContainerRuntime::Docker,
            KeepIdMode::Off,
            SandboxPolicy::WorkspaceWrite,
            SandboxNetworkMode::Proxy,
            3128,
        ));
        let entry = args
            .iter()
            .find(|a| a.starts_with("http_proxy="))
            .expect("proxy env");
        assert!(
            entry.ends_with("http://host.docker.internal:3128"),
            "{entry}"
        );
        let idx = args.iter().position(|a| a == "--add-host").unwrap();
        assert_eq!(args[idx + 1], "host.docker.internal:host-gateway");
    }

    #[test]
    fn args_env_passed_and_command_last() {
        let mut env = HashMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        let s = RunSpec {
            env: &env,
            ..spec(
                ContainerRuntime::Podman,
                KeepIdMode::Off,
                SandboxPolicy::ReadOnly,
                SandboxNetworkMode::None,
                0,
            )
        };
        let args = build_run_args(&s);
        assert!(args.contains(&"FOO=bar".to_string()));

        let image_idx = args
            .iter()
            .position(|a| a == "lunarwing-worker:latest")
            .unwrap();
        assert_eq!(&args[image_idx + 1..], &["-c", "echo hi"]);
    }

    // ── capped buffer ──────────────────────────────────────────────────

    #[test]
    fn capped_buf_under_limit_keeps_everything() {
        let mut buf = CappedBuf::new(10);
        buf.push(b"hello");
        let (s, truncated) = buf.into_string();
        assert_eq!(s, "hello");
        assert!(!truncated);
    }

    #[test]
    fn capped_buf_over_limit_truncates_and_flags() {
        let mut buf = CappedBuf::new(4);
        buf.push(b"hello world");
        let (s, truncated) = buf.into_string();
        assert_eq!(s, "hell");
        assert!(truncated);
    }

    #[test]
    fn capped_buf_flags_truncation_when_full_before_more_input() {
        let mut buf = CappedBuf::new(4);
        buf.push(b"abcd");
        assert!(!buf.truncated);
        buf.push(b"e");
        assert!(buf.truncated);
    }

    #[test]
    fn capped_buf_trims_incomplete_trailing_utf8() {
        // "🙂" is 4 bytes; cap 3 splits it.
        let mut buf = CappedBuf::new(3);
        buf.push("🙂".as_bytes());
        let (s, truncated) = buf.into_string();
        assert_eq!(s, "");
        assert!(truncated);
    }

    #[test]
    fn capped_buf_multi_chunk_truncation() {
        let mut buf = CappedBuf::new(6);
        buf.push(b"abc");
        buf.push(b"def");
        buf.push(b"ghi");
        let (s, truncated) = buf.into_string();
        assert_eq!(s, "abcdef");
        assert!(truncated);
    }

    // ── run failure classification ─────────────────────────────────────

    #[test]
    fn image_failures_classified() {
        let cases = [
            "Error: lunarwing-worker:latest: image not known",
            "Unable to find image 'lunarwing-worker:latest' locally",
            "docker: No such image: lunarwing-worker:latest.",
            "pull access denied for lunarwing-worker, repository does not exist",
        ];
        for stderr in cases {
            assert!(
                classify_run_failure(stderr, "lunarwing-worker:latest", 125).is_some(),
                "should classify: {stderr}"
            );
        }
        assert!(classify_run_failure("sh: foo: command not found", "img", 127).is_none());
        assert!(classify_run_failure("Error: command rejected input", "img", 1).is_none());
    }

    #[test]
    fn inflight_registry_tracks_names() {
        let name = format!("lunarwing-sandbox-{}", uuid::Uuid::new_v4());
        let guard = InflightGuard::new(name.clone());
        assert!(inflight_sandbox_containers().contains(&name));
        drop(guard);
        assert!(!inflight_sandbox_containers().contains(&name));
    }

    #[test]
    fn enforced_proxy_and_nonstandard_uid_args_are_confined() {
        for runtime in [ContainerRuntime::Podman, ContainerRuntime::Docker] {
            let args = build_run_args(&RunSpec {
                user: (2345, 3456),
                proxy_dir: Some(Path::new("/private/channel")),
                ..spec(
                    runtime,
                    KeepIdMode::Off,
                    SandboxPolicy::WorkspaceWrite,
                    SandboxNetworkMode::ProxyEnforced,
                    1234,
                )
            });
            for pair in [
                ["--user", "2345:3456"],
                ["--pids-limit", "256"],
                ["--network", "none"],
                ["--entrypoint", "sh"],
                ["--workdir", "/workspace"],
            ] {
                assert!(args.windows(2).any(|w| w == pair));
            }
            assert!(args.contains(&"/private/channel:/run/lunarwing-proxy:ro,z".into()));
            assert!(args.contains(&"http_proxy=http://127.0.0.1:3128".into()));
            assert!(
                !args
                    .iter()
                    .any(|a| a.contains("host-gateway") || a.contains("unconfined"))
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn docker_identity_uses_actual_workspace_owner() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().metadata().unwrap();
        let result = container_user(
            ContainerRuntime::Docker,
            KeepIdMode::Off,
            dir.path(),
            SandboxPolicy::WorkspaceWrite,
        );
        if meta.uid() == 0 || meta.gid() == 0 {
            assert!(result.is_err(), "never run writable workspace as root");
        } else {
            assert_eq!(result.unwrap(), (meta.uid(), meta.gid()));
            assert_eq!(
                container_user(
                    ContainerRuntime::Docker,
                    KeepIdMode::Off,
                    Path::new("/"),
                    SandboxPolicy::ReadOnly
                )
                .unwrap(),
                (
                    nix::unistd::geteuid().as_raw(),
                    nix::unistd::getegid().as_raw()
                ),
                "requesting another owner's directory must not change the caller identity",
            );
        }
    }

    #[test]
    fn proxy_and_runtime_setup_failures_are_structured() {
        for (stderr, kind) in [
            ("lunarwing-proxy-relay: Python 3 required", "proxy_error"),
            (
                "Error: statfs /private/channel: permission denied",
                "execution_failed",
            ),
            (
                "docker: Error response from daemon: invalid mount config",
                "execution_failed",
            ),
        ] {
            assert_eq!(
                classify_run_failure(stderr, "image", 125).unwrap().kind(),
                kind
            );
        }
    }
}

#[cfg(all(test, unix))]
#[path = "cli_runner_tests.rs"]
pub(super) mod lifecycle_tests;
