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

use crate::sandbox::config::{
    ResourceLimits, SandboxNetworkMode, SandboxPolicy, SandboxRuntime, default_pids_limit,
};
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
/// (no `--userns` flag), never up — a wrong flag fails every execution,
/// while a missing userns only degrades mount ownership.
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

/// In-container `--user` value that owns the mounted workspace (WI#179 concern 4).
///
/// Podman's keep-id ladder maps the in-container UID onto the host user, so
/// the pinned `1000:1000` always owns the mount. Docker has no per-run userns
/// flag: rootless docker maps container root onto the host user (so `0:0` is
/// correct), while rootful docker maps container UIDs 1:1 onto host UIDs (so
/// the daemon's own euid/egid is correct).
pub(crate) fn user_for(runtime: ContainerRuntime, docker_rootless: bool) -> String {
    match runtime {
        ContainerRuntime::Podman => "1000:1000".to_string(),
        ContainerRuntime::Docker if docker_rootless => "0:0".to_string(),
        ContainerRuntime::Docker => {
            #[cfg(unix)]
            {
                format!("{}:{}", nix::unistd::geteuid(), nix::unistd::getegid())
            }
            #[cfg(not(unix))]
            {
                "1000:1000".to_string()
            }
        }
    }
}

/// Probe order for runtime detection, honoring the configured preference.
pub(crate) fn runtime_probe_order(preferred: SandboxRuntime) -> [ContainerRuntime; 2] {
    match preferred {
        SandboxRuntime::Docker => [ContainerRuntime::Docker, ContainerRuntime::Podman],
        SandboxRuntime::Auto | SandboxRuntime::Podman => {
            [ContainerRuntime::Podman, ContainerRuntime::Docker]
        }
    }
}

/// Throwaway empty directory bind-mounted over protected subdirectories of the
/// workspace to hide them (WI#205).
///
/// Removed on drop. The directory must be readable by the container's mapped
/// UID; its contents are irrelevant because it is empty by construction.
///
/// Created under the same shared-location parent the proxy relay uses
/// ([`crate::sandbox::proxy::relay::relay_parent_dir`]): the daemon may run
/// with `PrivateTmp=true`, and the container CLI is spawned as a
/// manager-forked transient service in a different mount namespace — a mask
/// dir under the daemon's private `/tmp` would not be resolvable at
/// `podman run` time ("statfs …: no such file or directory"). `/run/user/<uid>`
/// is shared across those namespaces; the process temp dir is the fallback.
pub(crate) struct EmptyMaskDir {
    path: PathBuf,
}

impl EmptyMaskDir {
    fn create() -> Result<Self> {
        let base = shared_mount_source_parent();
        let path = base.join(format!("lunarwing-sandbox-mask-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).map_err(|e| SandboxError::ExecutionFailed {
            reason: format!("failed to create sandbox mask dir {}: {e}", path.display()),
        })?;
        // Podman maps the in-container UID onto the invoking user, so a
        // user-owned 0700 directory is the tightest mode that still mounts.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
        }
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

/// Parent for bind-mount source paths that must stay resolvable from the
/// container CLI's mount namespace (daemon + `PrivateTmp=true` + manager-
/// forked service would hide `/tmp`). Same policy as the proxy relay.
#[cfg(target_os = "linux")]
fn shared_mount_source_parent() -> PathBuf {
    crate::sandbox::proxy::relay::relay_parent_dir().unwrap_or_else(|_| std::env::temp_dir())
}

#[cfg(not(target_os = "linux"))]
fn shared_mount_source_parent() -> PathBuf {
    std::env::temp_dir()
}

impl Drop for EmptyMaskDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Throwaway empty *file* bind-mounted over credential-bearing files found
/// inside the mounted tree (WI#211). Removed on drop.
pub(crate) struct EmptyMaskFile {
    path: PathBuf,
}

impl EmptyMaskFile {
    fn create() -> Result<Self> {
        let base = shared_mount_source_parent();
        let path = base.join(format!(
            "lunarwing-sandbox-mask-file-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::File::create(&path).map_err(|e| SandboxError::ExecutionFailed {
            reason: format!("failed to create sandbox mask file {}: {e}", path.display()),
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for EmptyMaskFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Inputs to [`build_run_args`]. Kept as a struct so the arg construction
/// stays a pure function (unit-testable without any runtime installed).
pub(crate) struct RunSpec<'a> {
    pub runtime: ContainerRuntime,
    pub keep_id: KeepIdMode,
    /// `--user` value for the container (WI#179 concern 4: must own the
    /// mounted workspace on the selected runtime).
    pub user: &'a str,
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
    /// Host directory holding the per-execution proxy relay socket
    /// (proxy-enforced mode only). Bind-mounted read-only at
    /// `/run/lunarwing-proxy`.
    pub proxy_dir: Option<&'a str>,
    /// Per-container process limit (`--pids-limit`).
    pub pids_limit: u32,
    /// Empty host directory used to shadow protected subdirectories of the
    /// workspace mount (WI#205), plus the container paths to shadow with it.
    pub masked: &'a [String],
    pub mask_dir: Option<&'a Path>,
    /// Empty host file used to shadow credential-bearing files found inside
    /// the mount (WI#211), plus the container paths to shadow with it.
    pub masked_files: &'a [String],
    pub mask_file: Option<&'a Path>,
}

/// Build the `<runtime> run ...` argument vector for a sandboxed command.
///
/// Hardening mirrors the old bollard `ContainerRunner`: non-root UID 1000,
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
        spec.user.into(),
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
    // Explicit process cap: runtime defaults vary, so pin the ceiling instead
    // of relying on them (WI#179 concern 5).
    args.push("--pids-limit".into());
    args.push(spec.pids_limit.to_string());
    // Kill grace on stop/rm so the reaper's `rm -f` and the runner's own
    // timeout cleanup terminate the container's PID 1 instead of orphaning it.
    args.push("--stop-timeout".into());
    args.push("10".into());

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

    // Shadow protected subdirectories of the mount (WI#205). A mount of a
    // legitimate parent (a repo root containing `env/`) keeps working, but the
    // secret directory inside it is replaced by an empty bind, so nothing under
    // it can be read through `/workspace`. The source is a fresh empty
    // directory, so even a label/permission mistake exposes nothing.
    if let Some(mask_source) = spec.mask_dir {
        for destination in spec.masked {
            args.push("-v".into());
            args.push(format!("{}:{destination}:ro,z", mask_source.display()));
        }
    }

    // Shadow credential-bearing files found inside the mount with an empty
    // read-only file bind (WI#211): a copied `lunarwing.env` in a workspace is
    // unreadable in-container while the surrounding tree still works.
    if let Some(mask_file) = spec.mask_file {
        for destination in spec.masked_files {
            args.push("-v".into());
            args.push(format!("{}:{destination}:ro,z", mask_file.display()));
        }
    }

    // Caller-provided env first (never secrets: the shell tool passes none).
    for (key, value) in spec.env {
        args.push("-e".into());
        args.push(format!("{key}={value}"));
    }

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
            args.push("--network".into());
            args.push("none".into());
            if let Some(dir) = spec.proxy_dir {
                // Read-only relay socket transport (WI#155). Only this
                // directory is visible besides the proxy env: no direct
                // egress path exists anywhere, including localhost.
                args.extend(["-v".into(), format!("{}:/run/lunarwing-proxy:ro,z", dir)]);
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

    // Run under `sh` regardless of image entrypoint (worker images bind
    // their own entrypoint) and always work from the mounted workspace.
    args.extend([
        "--entrypoint".into(),
        "sh".into(),
        "--workdir".into(),
        "/workspace".into(),
        spec.image.into(),
        "-c".into(),
    ]);
    if spec.network == SandboxNetworkMode::ProxyEnforced {
        // Python relay wrapper (argv-separated: the untrusted command is a
        // single argv value, never interpolated into the wrapper). Missing
        // Python 3 fails closed rather than dropping the command unsandboxed.
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

/// Runtime the sandbox runner actually selected at detect time (WI#179
/// concern 2). The reaper's CLI sweep prefers this so it scans the runtime
/// that really created the containers, instead of guessing from PATH.
static SELECTED_RUNTIME: std::sync::OnceLock<ContainerRuntime> = std::sync::OnceLock::new();

/// Snapshot of currently-executing sandbox container names.
pub fn inflight_sandbox_containers() -> Vec<String> {
    INFLIGHT
        .lock()
        .map(|names| names.clone())
        .unwrap_or_default()
}

/// The first available runtime CLI binary name (podman preferred), for
/// callers that only need a CLI handle — notably the reaper's sandbox sweep.
pub fn available_runtime_binary() -> Option<&'static str> {
    if binary_on_path("podman") {
        Some("podman")
    } else if binary_on_path("docker") {
        Some("docker")
    } else {
        None
    }
}

/// Runtime CLI binary for the reaper's sandbox sweep: the runtime the runner
/// selected (when the sandbox has been initialized), falling back to the
/// PATH probe (WI#179 concern 2).
pub fn selected_runtime_binary() -> Option<&'static str> {
    SELECTED_RUNTIME
        .get()
        .map(ContainerRuntime::binary)
        .or_else(available_runtime_binary)
}

fn register_inflight(name: &str) {
    if let Ok(mut names) = INFLIGHT.lock() {
        names.push(name.to_string());
    }
}

fn unregister_inflight(name: &str) {
    if let Ok(mut names) = INFLIGHT.lock() {
        names.retain(|n| n != name);
    }
}

/// RAII in-flight registration: unregisters on every exit path, including a
/// dropped (cancelled) execute future (WI#179 concern 1). Without this, an
/// outer timeout dropping the future would leave the container name in the
/// registry forever and the reaper would skip the orphan.
struct InflightGuard {
    name: String,
    armed: bool,
}

impl InflightGuard {
    fn new(name: String) -> Self {
        register_inflight(&name);
        Self { name, armed: true }
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        if self.armed {
            unregister_inflight(&self.name);
        }
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
    /// Directories that must never be readable inside the mount (WI#205).
    pub protected_paths: &'a [std::path::PathBuf],
}

/// Runs sandboxed commands via the podman/docker CLI.
pub struct CliRunner {
    runtime: ContainerRuntime,
    keep_id: KeepIdMode,
    /// Whether the selected docker runtime runs rootless (container root
    /// maps onto the host user — always false for podman and for non-unix
    /// builds that can't probe).
    docker_rootless: bool,
    /// `--user` value computed at detect time (WI#179 concern 4).
    user: String,
    image: String,
}

impl CliRunner {
    /// Detect an installed runtime CLI (honoring the configured preference)
    /// and probe its capabilities.
    pub async fn detect(preferred: SandboxRuntime, image: String) -> Result<Self> {
        for runtime in runtime_probe_order(preferred) {
            if !binary_on_path(runtime.binary()) {
                continue;
            }
            let (version, docker_rootless) = match runtime {
                ContainerRuntime::Podman => (probe_podman_version().await, false),
                ContainerRuntime::Docker => (None, probe_docker_rootless().await),
            };
            tracing::info!(
                runtime = runtime.binary(),
                podman_version = ?version,
                docker_rootless,
                keep_id = ?keep_id_mode_for(runtime, version),
                "Generic sandbox: using container runtime CLI"
            );
            let _ = SELECTED_RUNTIME.set(runtime);
            return Ok(Self {
                runtime,
                keep_id: keep_id_mode_for(runtime, version),
                docker_rootless,
                user: user_for(runtime, docker_rootless),
                image,
            });
        }
        Err(SandboxError::RuntimeNotAvailable {
            reason: "neither `podman` nor `docker` found on PATH".to_string(),
        })
    }

    /// Runtime this runner drives.
    pub fn runtime(&self) -> ContainerRuntime {
        self.runtime
    }

    /// Check whether the sandbox image exists in the local store.
    pub async fn image_exists(&self) -> bool {
        run_status_isolated(self.runtime.binary(), &["image", "inspect", &self.image])
            .await
            .is_some_and(|s| s.success())
    }

    /// Pull the sandbox image from its registry.
    pub async fn pull_image(&self) -> Result<()> {
        tracing::info!("Pulling sandbox image: {}", self.image);
        let output = output_isolated(self.runtime.binary(), &["pull", &self.image])
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
    ///
    /// Proxy-enforced setup (WI#155) fails closed before any container is
    /// created: the running proxy/relay prerequisites are verified up front
    /// so a misconfigured run can never degrade to direct egress.
    pub async fn execute(&self, params: ExecParams<'_>) -> Result<ContainerOutput> {
        self.execute_with_name(
            params,
            &format!("lunarwing-sandbox-{}", uuid::Uuid::new_v4()),
        )
        .await
    }

    /// Like [`execute`](Self::execute) with an explicit container name, so
    /// tests can assert cleanup of a known name.
    async fn execute_with_name(
        &self,
        params: ExecParams<'_>,
        name: &str,
    ) -> Result<ContainerOutput> {
        let ExecParams {
            command,
            working_dir,
            policy,
            limits,
            env,
            network,
            proxy_port,
            protected_paths,
        } = params;

        // Containment gate at the runner boundary (WI#205). `SandboxManager`
        // validates too; repeating it here keeps the runner safe for any
        // future direct caller, since `working_dir` becomes the single bind
        // mount and is frequently agent-supplied.
        let working_dir_owned =
            crate::sandbox::mount_guard::validate_mount_dir(working_dir, protected_paths)?;
        let working_dir = working_dir_owned.as_path();
        // Protected directories inside a legitimate mount are shadowed rather
        // than refused: an agent working at a repo root must keep working, but
        // must not be able to read `env/` through it. Planning is existence-
        // independent (WI#209): a protected dir that appears mid-run is still
        // covered by the empty bind, and planning failures are hard refusals.
        let masked =
            crate::sandbox::mount_guard::masked_dirs_for_mount(working_dir, protected_paths)?;
        let masked_destinations: Vec<String> = masked
            .iter()
            .filter_map(|dir| crate::sandbox::mount_guard::mask_destination(working_dir, dir))
            .collect();
        // Credential files outside the enumerated directories (a copied
        // `lunarwing.env` in a workspace) are shadowed with an empty file bind
        // (WI#211). Plan-time walk, bounded.
        let masked_files =
            crate::sandbox::mount_guard::masked_files_for_mount(working_dir, protected_paths);
        let mut masked_file_destinations: Vec<String> = Vec::new();
        for file in &masked_files {
            let Some(dest) = crate::sandbox::mount_guard::mask_destination(working_dir, file)
            else {
                continue;
            };
            masked_file_destinations.push(dest);
        }
        // A single empty file shadows every credential file; a directory mask
        // covers protected dirs. Both are host-side throwaway artifacts.
        let mask_file = if masked_file_destinations.is_empty() {
            None
        } else {
            Some(EmptyMaskFile::create()?)
        };
        let mask_dir = if masked_destinations.is_empty() && masked_file_destinations.is_empty() {
            None
        } else {
            Some(EmptyMaskDir::create()?)
        };

        if network != SandboxNetworkMode::None && proxy_port == 0 {
            return Err(SandboxError::ProxyError {
                reason: format!("{network:?} network mode requires a running proxy"),
            });
        }
        fail_if_writable_root_docker(self.runtime, policy, self.docker_rootless, &self.user)?;

        let relay = if network == SandboxNetworkMode::ProxyEnforced {
            // Start before container creation; a failed relay never degrades
            // to advisory or direct egress (fail closed).
            Some(crate::sandbox::proxy::relay::ProxyRelay::start(proxy_port).await?)
        } else {
            None
        };
        let start_time = std::time::Instant::now();
        let created_at = chrono::Utc::now().to_rfc3339();
        let proxy_dir = relay
            .as_ref()
            .map(|relay| relay.path().to_string_lossy().into_owned());
        let proxy_dir_ref = proxy_dir.as_deref();

        let spec = RunSpec {
            runtime: self.runtime,
            keep_id: self.keep_id,
            user: &self.user,
            image: &self.image,
            name,
            created_at_label: &created_at,
            command,
            working_dir,
            policy,
            limits,
            env,
            network,
            proxy_port,
            proxy_dir: proxy_dir_ref,
            pids_limit: default_pids_limit(),
            masked: &masked_destinations,
            mask_dir: mask_dir.as_ref().map(EmptyMaskDir::path),
            masked_files: &masked_file_destinations,
            mask_file: mask_file.as_ref().map(EmptyMaskFile::path),
        };
        let args = build_run_args(&spec);

        // Cancellation-safe in-flight registration (WI#179 concern 1): a
        // guard unregisters on every exit path — normal return, error, or the
        // outer shell-tool timeout dropping this future. `run_container` also
        // performs its own kill-by-name on cancellation, so a dropped future
        // can never leave a live container registered as in-flight (which
        // would make the reaper skip it forever).
        let _inflight_guard = InflightGuard::new(name.to_string());
        let result = self.run_container(name, &args, limits).await;
        // The relay lives at least as long as the container process; closing
        // it earlier (e.g. on an outer-task drop racing the detached cleanup)
        // only cuts in-flight proxy streams for a container the cleanup guard
        // is already killing.
        drop(relay);

        result.map(|mut output| {
            output.duration = start_time.elapsed();
            output
        })
    }

    /// Spawn the run command, stream output into capped buffers, enforce the
    /// timeout, and return the container's exit code and output.
    ///
    /// Cancellation-safe (WI#179 concern 1): [`ContainerCleanupGuard`] runs
    /// kill-by-name cleanup on every exit path, including a dropped future
    /// (the shell tool's outer per-call timeout expiring first, or a task
    /// abort). Killing the CLI client alone does not stop the container —
    /// conmon owns it — so the guard issues `kill` + `rm -f` directly.
    async fn run_container(
        &self,
        name: &str,
        args: &[String],
        limits: &ResourceLimits,
    ) -> Result<ContainerOutput> {
        // Spawn through the workload spawner: fresh process group +
        // kill_on_drop + systemd transient-service isolation when available
        // (BUG-005, WI#178/WI#181). The service context is required for the
        // podman CLI: a daemon-forked child inherits the daemon's
        // NoNewPrivileges/PrivateTmp context and cannot join the tenant's
        // rootless user namespace.
        let spawner = crate::tools::process::workload_spawner().await;
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut cmd = spawner.command(self.runtime.binary(), &arg_refs);

        // In service mode the manager forks the workload, so the environment
        // must be handed over via EnvironmentFile= (mode 0600, deleted after
        // the run) instead of process inheritance. Rebuilds the command, so
        // stdio must be set after this call.
        let spec = spawner.prepare_service(&mut cmd);
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
        let mut guard = crate::tools::process::ProcessTreeGuard::for_service(
            &child,
            spec.as_ref(),
            spawner.xdg_runtime_dir(),
        );
        // Detached kill-by-name on cancellation/drop (see struct docs).
        let mut cleanup = ContainerCleanupGuard::new(name.to_string(), self.runtime.binary());

        // Drain both pipes concurrently so the child never blocks on a full
        // pipe; collection continues past the cap until EOF.
        let half_max = limits.max_output_bytes / 2;
        let out_task = tokio::spawn(read_capped(stdout_pipe, half_max));
        let err_task = tokio::spawn(read_capped(stderr_pipe, half_max));

        let waited = tokio::time::timeout(limits.timeout, child.wait()).await;

        match waited {
            Err(_) => {
                // Killing the CLI client does not stop the container (conmon
                // owns it) — kill by name too; `--rm` then cleans it up.
                guard.kill_tree().await;
                self.kill_by_name(name).await;
                cleanup.disarm();
                Err(SandboxError::Timeout(limits.timeout))
            }
            Ok(Err(e)) => {
                guard.kill_tree().await;
                self.kill_by_name(name).await;
                cleanup.disarm();
                Err(SandboxError::ExecutionFailed {
                    reason: format!("container wait failed: {e}"),
                })
            }
            Ok(Ok(status)) => {
                guard.disarm();
                cleanup.disarm();
                let exit_code = status.code().unwrap_or(-1) as i64;

                let (out_buf, err_buf) = match tokio::try_join!(out_task, err_task) {
                    Ok(bufs) => bufs,
                    Err(e) => {
                        return Err(SandboxError::ExecutionFailed {
                            reason: format!("output collection failed: {e}"),
                        });
                    }
                };
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

    /// Best-effort `kill` + `rm -f` of a sandbox container by name.
    async fn kill_by_name(&self, name: &str) {
        let bin = self.runtime.binary();
        for args in [vec!["kill", name], vec!["rm", "-f", name]] {
            if let Some(status) = run_status_isolated(bin, &args).await
                && !status.success()
            {
                tracing::debug!(container = %name, "Sandbox cleanup command failed (may already be gone)");
            }
        }
    }
}

/// Cancellation-safe container cleanup (WI#179 concern 1).
///
/// When the future owning the run is dropped — the shell tool's outer
/// per-call timeout expiring before the manager's own timeout, or a task
/// abort — the CLI child dies via `kill_on_drop`, but the container keeps
/// running under conmon. This guard spawns a detached `kill` + `rm -f`
/// sequence on drop so the container is stopped even though no one awaits
/// the cleanup. `disarm()` is called on every normal exit path.
struct ContainerCleanupGuard {
    name: String,
    bin: &'static str,
    armed: bool,
}

impl ContainerCleanupGuard {
    fn new(name: String, bin: &'static str) -> Self {
        Self {
            name,
            bin,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ContainerCleanupGuard {
    /// Known window (!1046 review finding 3): the detached cleanup is a
    /// `tokio::spawn`, so a daemon shutdown racing an in-flight execution can
    /// exit the process before the spawned cleanup runs (detached tasks do
    /// not outlive runtime shutdown). That is a bounded gap, backstopped by
    /// the independent CLI reaper, which sweeps labeled orphans on its own
    /// cadence. A synchronous cleanup here is not possible (Drop is sync and
    /// spawning within an aborting runtime can itself fail).
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let name = self.name.clone();
        let bin = self.bin;
        tokio::spawn(async move {
            for args in [vec!["kill", name.as_str()], vec!["rm", "-f", name.as_str()]] {
                if let Some(status) = run_status_isolated(bin, &args).await
                    && !status.success()
                {
                    tracing::debug!(container = %name, "Sandbox cancellation cleanup failed (may already be gone)");
                }
            }
        });
    }
}

/// Execute-time container-root guard (!1046 review finding 2).
///
/// Only ROOTFUL + root-daemon Docker is refused: rootless Docker also maps
/// `--user 0:0` but container root there maps onto the host user via the
/// userns (safe; `user_for` documents the mapping). `CliRunner` stores the
/// rootless flag detected at `detect()` time, so the guard refuses only
/// `!docker_rootless && allows_writes && user == 0:0` — rootless writable
/// workspaces keep working with their userns identity.
#[cfg(unix)]
fn fail_if_writable_root_docker(
    runtime: ContainerRuntime,
    policy: SandboxPolicy,
    docker_rootless: bool,
    user: &str,
) -> Result<()> {
    if runtime == ContainerRuntime::Docker
        && !docker_rootless
        && policy.allows_writes()
        && user == "0:0"
    {
        return Err(SandboxError::Config {
            reason: "rootful Docker under a root daemon would run a writable workspace as container root; use rootless podman or a non-root daemon (rootless Docker is allowed — container root there maps onto the host user via userns)".into(),
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn fail_if_writable_root_docker(
    _runtime: ContainerRuntime,
    _policy: SandboxPolicy,
    _docker_rootless: bool,
    _user: &str,
) -> Result<()> {
    Ok(())
}

/// Map a failed run's stderr to a typed error when it indicates a missing
/// image (e.g. the image vanished between the initialize-time check and the
/// run, or auto-pull is off), a relay setup failure (proxy-enforced), or a
/// runtime setup failure (mount/userns/resource-controller errors reported by
/// the runtime CLI, exit 125).
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
    // Runtime setup failures are sandbox errors, not a successful invocation
    // with an ordinary command exit code.
    let runtime_error = exit_code == 125
        && lower
            .lines()
            .any(|line| line.starts_with("error:") || line.starts_with("docker: error"));
    runtime_error.then(|| SandboxError::ExecutionFailed {
        reason: stderr.trim().into(),
    })
}

/// Run a quick status probe through the workload spawner (WI#181: image
/// inspection/pull and cleanup must execute in the same manager-forked
/// context as `run` — the hardened daemon's context breaks podman's
/// rootless user-namespace join), returning `None` when spawn fails.
async fn run_status_isolated(bin: &str, args: &[&str]) -> Option<std::process::ExitStatus> {
    let spawner = crate::tools::process::workload_spawner().await;
    let mut cmd = spawner.command(bin, args);
    let spec = spawner.prepare_service(&mut cmd);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let mut guard = crate::tools::process::ProcessTreeGuard::for_service(
        &child,
        spec.as_ref(),
        spawner.xdg_runtime_dir(),
    );
    let status = child.wait().await;
    guard.disarm();
    status.ok()
}

/// Capture a command's output through the workload spawner (same isolation
/// rationale as [`run_status_isolated`]).
async fn output_isolated(bin: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    let spawner = crate::tools::process::workload_spawner().await;
    let mut cmd = spawner.command(bin, args);
    let spec = spawner.prepare_service(&mut cmd);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let child = cmd.spawn()?;
    let mut guard = crate::tools::process::ProcessTreeGuard::for_service(
        &child,
        spec.as_ref(),
        spawner.xdg_runtime_dir(),
    );
    let output = child.wait_with_output().await;
    guard.disarm();
    output
}

/// Capture and parse `podman --version`.
async fn probe_podman_version() -> Option<(u32, u32)> {
    let output = tokio::process::Command::new("podman")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    parse_podman_version(&String::from_utf8_lossy(&output.stdout))
}

/// Detect whether docker is running rootless (`docker info` reports a
/// rootless security option). Rootless docker maps container root onto the
/// host user, so the sandbox must run as `--user 0:0` for bind mounts to be
/// writable (WI#179 concern 4).
///
/// The probe is bounded (!1046 review finding 1): a present-but-wedged daemon
/// must not hang sandbox initialization forever. On timeout or probe failure
/// we conservatively report "not rootless" (rootful mapping), which makes
/// read-only policy work and the execute-time guard fail writable workspaces
/// closed under a root daemon.
async fn probe_docker_rootless() -> bool {
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new("docker")
            .args(["info", "--format", "{{.SecurityOptions}}"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await;
    match output {
        Ok(Ok(output)) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout).to_lowercase();
            text.contains("rootless")
        }
        _ => false,
    }
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
            user: "1000:1000",
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
            proxy_dir: None,
            pids_limit: 256,
            masked: &[],
            mask_dir: None,
            masked_files: &[],
            mask_file: None,
        }
    }

    /// WI#205: protected subdirectories of the workspace are shadowed by an
    /// empty read-only bind, and no mask mount appears when nothing needs
    /// masking.
    #[test]
    fn protected_subdirs_are_shadow_mounted() {
        static MASKED: std::sync::LazyLock<Vec<String>> =
            std::sync::LazyLock::new(|| vec!["/workspace/env".to_string()]);
        let mask_dir = Path::new("/tmp/lunarwing-sandbox-mask-test");
        let mut masked_spec = spec(
            ContainerRuntime::Podman,
            KeepIdMode::UidPinned,
            SandboxPolicy::WorkspaceWrite,
            SandboxNetworkMode::None,
            0,
        );
        masked_spec.masked = &MASKED;
        masked_spec.mask_dir = Some(mask_dir);

        let args = build_run_args(&masked_spec);
        let joined = args.join(" ");
        assert!(
            joined.contains("/tmp/lunarwing-sandbox-mask-test:/workspace/env:ro,z"),
            "mask mount missing from: {joined}"
        );
        // The workspace mount itself must still be present and precede the mask
        // so the shadow consistently wins for overlapping paths.
        let workspace_pos = joined
            .find("/tmp/proj:/workspace")
            .expect("workspace mount");
        let mask_pos = joined.find(":/workspace/env:ro,z").expect("mask mount");
        assert!(
            workspace_pos < mask_pos,
            "workspace mount must be established before the mask: {joined}"
        );

        let plain = build_run_args(&spec(
            ContainerRuntime::Podman,
            KeepIdMode::UidPinned,
            SandboxPolicy::WorkspaceWrite,
            SandboxNetworkMode::None,
            0,
        ));
        assert!(
            !plain.join(" ").contains("lunarwing-sandbox-mask"),
            "no mask mount expected without protected subdirectories"
        );
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
            [ContainerRuntime::Podman, ContainerRuntime::Docker]
        );
        assert_eq!(
            runtime_probe_order(SandboxRuntime::Docker),
            [ContainerRuntime::Docker, ContainerRuntime::Podman]
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
            "--pids-limit",
            "--stop-timeout",
        ] {
            assert!(args.contains(&flag.to_string()), "missing flag {flag}");
        }
        let tmpfs_idx = args.iter().position(|a| a == "--tmpfs").unwrap();
        assert_eq!(args[tmpfs_idx + 1], "/tmp:size=512m");
        let memory_idx = args.iter().position(|a| a == "--memory").unwrap();
        assert_eq!(args[memory_idx + 1], "2048m");
        let pids_idx = args.iter().position(|a| a == "--pids-limit").unwrap();
        assert_eq!(args[pids_idx + 1], "256");
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
        assert_eq!(
            args.windows(2)
                .find(|w| w[0] == "--entrypoint")
                .map(|w| w[1].as_str()),
            Some("sh"),
            "image entrypoint must not intercept sandbox commands"
        );
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
        assert!(
            classify_run_failure("Error: command rejected input", "img", 1).is_none(),
            "exit 125 marker in unrelated contexts must not mask ordinary errors"
        );
    }

    #[test]
    fn proxy_relay_and_runtime_setup_failures_are_structured() {
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

    #[test]
    fn enforced_proxy_args_are_confined() {
        for runtime in [ContainerRuntime::Podman, ContainerRuntime::Docker] {
            let args = build_run_args(&RunSpec {
                proxy_dir: Some("/private/channel"),
                ..spec(
                    runtime,
                    KeepIdMode::Off,
                    SandboxPolicy::WorkspaceWrite,
                    SandboxNetworkMode::ProxyEnforced,
                    1234,
                )
            });
            for pair in [
                ["--user", "1000:1000"],
                ["--pids-limit", "256"],
                ["--network", "none"],
                ["--entrypoint", "sh"],
                ["--workdir", "/workspace"],
            ] {
                assert!(
                    args.windows(2).any(|w| w == pair),
                    "missing pair {pair:?} in {args:?}"
                );
            }
            assert!(args.contains(&"/private/channel:/run/lunarwing-proxy:ro,z".to_string()));
            assert!(args.contains(&"http_proxy=http://127.0.0.1:3128".to_string()));
            assert!(args.contains(&"NO_PROXY=".to_string()));
            // The untrusted command is a separate argv value behind the
            // Python relay wrapper, never interpolated into sh -c directly.
            let image_idx = args
                .iter()
                .position(|a| a == "lunarwing-worker:latest")
                .unwrap();
            let tail = &args[image_idx + 1..];
            assert_eq!(tail[0], "-c");
            assert_eq!(tail[2], "lunarwing-proxy-relay");
            assert!(tail[3].contains("lunarwing-proxy-relay:"));
            assert_eq!(tail.last(), Some(&"echo hi".to_string()));
            assert!(
                !args
                    .iter()
                    .any(|a| a.contains("host-gateway") || a.contains("unconfined"))
            );
        }
    }

    #[test]
    fn inflight_registry_tracks_names() {
        let before = inflight_sandbox_containers().len();
        register_inflight("lunarwing-sandbox-x");
        assert!(inflight_sandbox_containers().contains(&"lunarwing-sandbox-x".to_string()));
        unregister_inflight("lunarwing-sandbox-x");
        assert_eq!(inflight_sandbox_containers().len(), before);
        // Unregistering an unknown name is a no-op.
        unregister_inflight("lunarwing-sandbox-never-existed");
        assert_eq!(inflight_sandbox_containers().len(), before);
    }

    #[test]
    fn empty_mask_dir_lives_in_the_shared_namespace_visible_parent() {
        // WI#205 fix (review of !1073): the mask source is bind-mounted by the
        // container CLI, which may run in a different mount namespace than the
        // daemon (PrivateTmp=true + manager-forked transient service). The
        // directory must therefore live under the same shared parent the proxy
        // relay uses — never unconditionally under the process temp dir.
        let mask = super::EmptyMaskDir::create().expect("mask dir created");
        let path = mask.path().to_path_buf();
        assert!(path.exists(), "mask dir must exist while armed");
        assert!(
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("lunarwing-sandbox-mask-")),
            "unexpected mask dir name: {}",
            path.display()
        );
        #[cfg(target_os = "linux")]
        {
            let expected = super::shared_mount_source_parent();
            assert_eq!(
                path.parent(),
                Some(expected.as_path()),
                "mask dir must live under the shared (XDG-runtime-dir) parent so it stays \
                 visible to the service-forked container CLI"
            );
        }
        #[cfg(all(unix, not(target_os = "linux")))]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        drop(mask);
        assert!(!path.exists(), "mask dir must be removed on drop");
    }

    // ── lifecycle operations share the isolated execution context (WI#181) ──

    /// Image inspection must succeed through the same manager-forked context
    /// that `run` uses: a daemon-forked podman call breaks rootless
    /// user-namespace joins under the hardened unit, which would make
    /// `SandboxManager::initialize()` fail on an existing image.
    #[tokio::test]
    async fn image_exists_through_isolated_context() {
        let spawner = crate::tools::process::workload_spawner().await;
        if !spawner.uses_systemd_services() {
            eprintln!("skipping: systemd user transient services unavailable");
            return;
        }
        let probe = tokio::process::Command::new("podman")
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
        if !matches!(probe, Ok(status) if status.success()) {
            eprintln!("skipping: podman unavailable");
            return;
        }
        let runner = CliRunner::detect(SandboxRuntime::Podman, "busybox:latest".to_string())
            .await
            .expect("podman runner detect");
        let result = run_status_isolated(
            runner.runtime.binary(),
            &["image", "inspect", "busybox:latest"],
        )
        .await;
        // Whatever the answer (present or absent), the probe must have RUN in
        // the isolated context: a broken context surfaces as a spawn/wait
        // failure, which run_status_isolated maps to None.
        assert!(
            result.is_some(),
            "image inspection did not complete through the isolated context"
        );
    }

    /// Cleanup commands share the isolated context: a timed-out container run
    /// leaves the container behind, and `kill`/`rm` from the daemon's own
    /// context fails under the hardened unit.
    #[tokio::test]
    async fn timeout_cleanup_removes_container_through_isolated_context() {
        let spawner = crate::tools::process::workload_spawner().await;
        if !spawner.uses_systemd_services() {
            eprintln!("skipping: systemd user transient services unavailable");
            return;
        }
        let probe = tokio::process::Command::new("podman")
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
        if !matches!(probe, Ok(status) if status.success()) {
            eprintln!("skipping: podman unavailable");
            return;
        }
        let runner = CliRunner::detect(SandboxRuntime::Podman, "busybox:latest".to_string())
            .await
            .expect("podman runner detect");
        let limits = ResourceLimits {
            timeout: Duration::from_millis(1500),
            max_output_bytes: 64 * 1024,
            ..*LIMITS
        };
        let params = ExecParams {
            command: "sleep 10",
            working_dir: std::path::Path::new("/tmp"),
            policy: SandboxPolicy::WorkspaceWrite,
            limits: &limits,
            env: &HashMap::new(),
            network: SandboxNetworkMode::None,
            proxy_port: 0,
            protected_paths: &[],
        };
        let name_probe = uuid::Uuid::new_v4();
        // `image_exists` must pass first or the timeout run itself fails for
        // the wrong reason (missing image).
        if !runner.image_exists().await {
            eprintln!("skipping: busybox:latest not in local store");
            return;
        }
        let err = runner
            .execute_with_name(params, &format!("lunarwing-sandbox-timeout-{name_probe}"))
            .await
            .expect_err("timeout run must fail");
        assert!(
            matches!(err, SandboxError::Timeout(_)),
            "expected timeout, got: {err}"
        );
        // The timeout path's kill/rm already ran through the isolated
        // context; assert the container is gone as the end state.
        let leftover = run_status_isolated(
            runner.runtime.binary(),
            &[
                "container",
                "exists",
                &format!("lunarwing-sandbox-timeout-{name_probe}"),
            ],
        )
        .await
        .map(|s| s.success())
        .unwrap_or(false);
        assert!(!leftover, "container survived timeout cleanup");
    }

    #[test]
    fn inflight_guard_unregisters_on_drop() {
        let before = inflight_sandbox_containers().len();
        {
            let _guard = InflightGuard::new("lunarwing-sandbox-guard".to_string());
            assert!(inflight_sandbox_containers().contains(&"lunarwing-sandbox-guard".to_string()));
        }
        assert_eq!(inflight_sandbox_containers().len(), before);
    }

    #[cfg(unix)]
    #[test]
    fn root_docker_guard_only_refuses_rootful_under_root() {
        // Rootless Docker: container root maps onto the host user via the
        // userns, so `--user 0:0` is SAFE. The guard must not refuse it.
        assert!(
            fail_if_writable_root_docker(
                ContainerRuntime::Docker,
                SandboxPolicy::WorkspaceWrite,
                true,
                "0:0",
            )
            .is_ok(),
            "rootless Docker writable workspaces must not be refused"
        );

        // Rootful Docker under a NON-root daemon never resolves to 0:0, so
        // nothing to refuse here either.
        assert!(
            fail_if_writable_root_docker(
                ContainerRuntime::Docker,
                SandboxPolicy::WorkspaceWrite,
                false,
                "1234:1234",
            )
            .is_ok(),
            "rootful Docker under a non-root daemon is fine"
        );

        // THE REFUSAL: rootful Docker under a root daemon.
        let refuse = fail_if_writable_root_docker(
            ContainerRuntime::Docker,
            SandboxPolicy::WorkspaceWrite,
            false,
            "0:0",
        );
        assert!(
            matches!(refuse, Err(SandboxError::Config { reason }) if reason.contains("rootful Docker under a root daemon")),
        );
    }

    #[cfg(unix)]
    #[test]
    fn root_docker_guard_ignores_readonly_and_non_docker() {
        assert!(
            fail_if_writable_root_docker(
                ContainerRuntime::Docker,
                SandboxPolicy::ReadOnly,
                false,
                "0:0",
            )
            .is_ok(),
            "read-only policy under rootful-Docker-as-root is documented accepted risk"
        );
        assert!(
            fail_if_writable_root_docker(
                ContainerRuntime::Podman,
                SandboxPolicy::WorkspaceWrite,
                false,
                "0:0",
            )
            .is_ok(),
            "podman never hits the docker guard"
        );
    }

    #[test]
    fn docker_user_mapping_follows_rootless_detection() {
        // Rootless docker maps container root onto the host user: run as 0:0.
        assert_eq!(user_for(ContainerRuntime::Docker, true), "0:0");
        // Rootful docker maps UIDs 1:1: run as the daemon's own uid/gid.
        #[cfg(unix)]
        assert_eq!(
            user_for(ContainerRuntime::Docker, false),
            format!("{}:{}", nix::unistd::geteuid(), nix::unistd::getegid())
        );
        // Podman uses the keep-id ladder and the pinned sandbox user.
        assert_eq!(user_for(ContainerRuntime::Podman, false), "1000:1000");
    }
}
