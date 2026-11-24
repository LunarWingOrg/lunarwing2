//! Proactive Docker detection with platform-specific guidance.
//!
//! Checks whether Docker is both installed (binary on PATH) and running
//! (daemon responding to ping), and provides platform-appropriate
//! installation or startup instructions when it is not.
//!
//! # Detection Limitations
//!
//! - **macOS**: High confidence. Detects both standard Docker Desktop socket
//!   (`~/.docker/run/docker.sock`) and the default `/var/run/docker.sock`.
//!
//! - **Linux**: High confidence for standard installs. Rootless Docker uses
//!   a different socket path (`/run/user/$UID/docker.sock`) which is now
//!   checked by the fallback in `connect_docker()`. If `DOCKER_HOST` is set,
//!   bollard's default connection still takes precedence.
//!
//! - **Windows**: Medium confidence. Binary detection uses `where.exe` which
//!   works reliably. Daemon detection relies on bollard's default named pipe
//!   connection (`//./pipe/docker_engine`) which works with Docker Desktop.
//!   The Unix socket fallback in `connect_docker()` is a no-op on Windows,
//!   so detection also probes `docker version`/`docker info` via CLI if the
//!   named pipe is unavailable.

use crate::sandbox::cli_runner::{ContainerRuntime, runtime_probe_order};
use crate::sandbox::config::SandboxRuntime;
use crate::sandbox::runtime_path;

/// Docker daemon availability status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockerStatus {
    /// Docker binary found on PATH and daemon responding to ping.
    Available,
    /// `docker` binary not found on PATH.
    NotInstalled,
    /// Binary found but daemon not responding.
    NotRunning,
    /// Sandbox feature not enabled (no check performed).
    Disabled,
}

impl DockerStatus {
    /// Returns true if Docker is available and ready.
    pub fn is_ok(&self) -> bool {
        matches!(self, DockerStatus::Available)
    }

    /// Human-readable status string.
    pub fn as_str(&self) -> &'static str {
        match self {
            DockerStatus::Available => "available",
            DockerStatus::NotInstalled => "not installed",
            DockerStatus::NotRunning => "not running",
            DockerStatus::Disabled => "disabled",
        }
    }
}

/// Host platform for install guidance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOS,
    Linux,
    Windows,
}

impl Platform {
    /// Detect the current platform.
    pub fn current() -> Self {
        match std::env::consts::OS {
            "macos" => Platform::MacOS,
            "windows" => Platform::Windows,
            _ => Platform::Linux,
        }
    }

    /// Installation instructions for Docker on this platform.
    pub fn install_hint(&self) -> &'static str {
        match self {
            Platform::MacOS => {
                "Install Docker Desktop: https://docs.docker.com/desktop/install/mac-install/"
            }
            Platform::Linux => "Install Docker Engine: https://docs.docker.com/engine/install/",
            Platform::Windows => {
                "Install Docker Desktop: https://docs.docker.com/desktop/install/windows-install/"
            }
        }
    }

    /// Instructions to start the Docker daemon on this platform.
    pub fn start_hint(&self) -> &'static str {
        match self {
            Platform::MacOS => {
                "Start Docker Desktop from Applications, or run: open -a Docker\n\n  To auto-start at login: System Settings > General > Login Items > add Docker.app"
            }
            Platform::Linux => "Start the Docker daemon: sudo systemctl start docker",
            Platform::Windows => "Start Docker Desktop from the Start menu",
        }
    }
}

/// Result of a Docker detection check.
#[derive(Debug, Clone, Copy)]
pub struct DockerDetection {
    pub status: DockerStatus,
    pub platform: Platform,
}

/// Result of selecting the runtime CLI the generic sandbox will drive.
///
/// The generic sandbox shells out to `podman` or `docker` directly, so
/// Docker's daemon is not the only way to have a working sandbox. This
/// selection mirrors [`crate::sandbox::cli_runner::CliRunner::detect`]'s
/// probe order, which is what the sandbox actually uses.
#[derive(Debug, Clone)]
pub struct SandboxRuntimeDetection {
    /// Runtime CLI the sandbox will use; `None` when none is usable.
    pub runtime: Option<ContainerRuntime>,
    /// Configured preference that produced this selection.
    pub configured: SandboxRuntime,
    /// Every runtime probed, in probe order, with its detection result.
    /// Probes stop at the first usable runtime, so this is usually shorter
    /// than the full order.
    pub tried: Vec<(ContainerRuntime, DockerDetection)>,
}

impl SandboxRuntimeDetection {
    /// Startup diagnostic naming the runtime actually selected.
    ///
    /// Only reports `sandbox disabled` when every runtime in the probe
    /// order was unusable (WI#871): a healthy podman-only host must not
    /// warn about Docker's absence.
    pub fn summary(&self) -> String {
        if let Some(runtime) = self.runtime {
            return format!("sandbox runtime: {}", runtime.binary());
        }
        let details: Vec<String> = self
            .tried
            .iter()
            .map(|(runtime, detection)| describe_unusable_runtime(*runtime, detection))
            .collect();
        format!(
            "no usable container runtime -- sandbox disabled for this session: {}",
            details.join("; ")
        )
    }

    /// The user-visible warning for a host where the sandbox works but full-job
    /// (worker) containers cannot start (WI#876).
    ///
    /// Full-job routines dispatch over a Docker-compatible API, which the
    /// generic sandbox does not need: a podman-only host has a healthy sandbox
    /// and still cannot run a worker container. That host used to get only an
    /// info log. `None` when either side is fine, or when nothing works at all
    /// (that case has its own warning).
    pub fn full_job_warning(&self, docker_status: DockerStatus) -> Option<String> {
        if self.runtime.is_none() || docker_status.is_ok() {
            return None;
        }
        let runtime = self.runtime?;
        let hint = match docker_status {
            DockerStatus::NotRunning => {
                "Start Docker (or point DOCKER_HOST at a compatible socket)."
            }
            DockerStatus::NotInstalled => {
                "Install Docker, or point DOCKER_HOST at a compatible socket."
            }
            _ => "Point DOCKER_HOST at a Docker-compatible socket.",
        };
        Some(format!(
            "Sandbox runs on {} but full-job (worker) containers are unavailable -- \
             no Docker-compatible API is reachable, so full-job routines will fail \
             at dispatch. {hint}",
            runtime.binary()
        ))
    }
}

/// One runtime's failure, with the platform-specific hint that fixes it.
fn describe_unusable_runtime(runtime: ContainerRuntime, detection: &DockerDetection) -> String {
    match (runtime, detection.status) {
        (_, DockerStatus::Available) => format!("{} is available", runtime.binary()),
        (ContainerRuntime::Podman, DockerStatus::NotInstalled) => {
            format!("podman is not installed ({})", podman_install_hint())
        }
        (ContainerRuntime::Podman, _) => {
            "podman is installed but `podman version` failed".to_string()
        }
        (ContainerRuntime::Docker, DockerStatus::NotInstalled) => format!(
            "docker is not installed ({})",
            detection.platform.install_hint()
        ),
        (ContainerRuntime::Docker, _) => format!(
            "docker is installed but not running ({})",
            detection.platform.start_hint()
        ),
    }
}

/// Select the container runtime the generic sandbox will drive.
///
/// Probes in [`crate::sandbox::cli_runner::runtime_probe_order`] and stops at
/// the first usable runtime, so callers can log what the sandbox will run on
/// instead of reporting a missing Docker on a podman-backed host (WI#871).
pub async fn check_sandbox_runtime(preferred: SandboxRuntime) -> SandboxRuntimeDetection {
    let mut tried = Vec::new();
    for runtime in runtime_probe_order(preferred) {
        let detection = match runtime {
            ContainerRuntime::Podman => check_podman().await,
            ContainerRuntime::Docker => check_docker().await,
        };
        let available = detection.status.is_ok();
        tried.push((runtime, detection));
        if available {
            return SandboxRuntimeDetection {
                runtime: Some(runtime),
                configured: preferred,
                tried,
            };
        }
    }
    SandboxRuntimeDetection {
        runtime: None,
        configured: preferred,
        tried,
    }
}

/// Check whether Docker is installed and running.
///
/// 1. Checks if `docker` binary exists on PATH
/// 2. If found, tries to connect and ping the Docker daemon via `connect_docker()`
/// 3. Returns `Available`, `NotInstalled`, or `NotRunning`
pub async fn check_docker() -> DockerDetection {
    let platform = Platform::current();

    // Step 1: Check if docker binary is on PATH
    if !docker_binary_exists() {
        return DockerDetection {
            status: DockerStatus::NotInstalled,
            platform,
        };
    }

    // Step 2: Try to connect to the daemon
    if crate::sandbox::connect_docker().await.is_ok() {
        return DockerDetection {
            status: DockerStatus::Available,
            platform,
        };
    }

    // Windows fallback: if the named pipe probe fails but docker CLI can still
    // reach the daemon/server, treat Docker as available.
    #[cfg(windows)]
    if docker_cli_daemon_reachable() {
        return DockerDetection {
            status: DockerStatus::Available,
            platform,
        };
    }

    DockerDetection {
        status: DockerStatus::NotRunning,
        platform,
    }
}

/// Check if the `docker` binary exists on PATH.
fn docker_binary_exists() -> bool {
    #[cfg(unix)]
    {
        runtime_path::binary_on_path("docker")
    }
    #[cfg(windows)]
    {
        std::process::Command::new("where")
            .arg("docker")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}

// The `version` subcommand bypasses Podman's rootless namespace constructor.
// `--version` does not: under a hardened user unit, Podman 6.1.2 can mistake a
// live but unreadable pause process for a recycled PID and unlink pause.pid.
// Keep the familiar output shape for the keep-id version parser.
pub(super) const PODMAN_VERSION_ARGS: [&str; 3] =
    ["version", "--format", "podman version {{.Client.Version}}"];

/// Check whether the podman CLI is installed and usable.
///
/// Podman has no daemon, so "running" means the CLI itself responds
/// (the client-only `podman version` probe succeeds — the generic sandbox drives the CLI
/// directly, mirroring the mt-admin external-worker pattern). Status
/// meanings reuse [`DockerStatus`]: `Available` = CLI usable,
/// `NotRunning` = installed but broken/uninvokable.
pub async fn check_podman() -> DockerDetection {
    let platform = Platform::current();

    if !runtime_path::binary_on_path("podman") {
        return DockerDetection {
            status: DockerStatus::NotInstalled,
            platform,
        };
    }

    // A runtime installed only in an sbin directory must still answer its
    // version probe; the daemon's service PATH can omit sbin (WI#663).
    let mut probe = tokio::process::Command::new("podman");
    probe.args(PODMAN_VERSION_ARGS);
    runtime_path::apply(&mut probe);
    let output = probe
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .await;

    match output {
        Ok(output) if output.status.success() => DockerDetection {
            status: DockerStatus::Available,
            platform,
        },
        _ => DockerDetection {
            status: DockerStatus::NotRunning,
            platform,
        },
    }
}

/// Installation guidance for podman.
pub fn podman_install_hint() -> &'static str {
    "Install podman: https://podman.io/docs/installation (Gentoo: emerge app-containers/podman)"
}

#[cfg(windows)]
fn docker_cli_daemon_reachable() -> bool {
    let stdout = std::process::Stdio::null();
    let stderr = std::process::Stdio::null();

    // `docker version` requires daemon reachability for server fields.
    let version_ok = std::process::Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .stdout(stdout)
        .stderr(stderr)
        .status()
        .is_ok_and(|s| s.success());

    if version_ok {
        return true;
    }

    // Fallback for environments where `docker version --format` behaves differently.
    std::process::Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_platform() {
        let platform = Platform::current();
        match platform {
            Platform::MacOS | Platform::Linux | Platform::Windows => {}
        }
    }

    #[test]
    fn test_install_hint_not_empty() {
        for platform in [Platform::MacOS, Platform::Linux, Platform::Windows] {
            assert!(!platform.install_hint().is_empty());
            assert!(!platform.start_hint().is_empty());
        }
    }

    #[test]
    fn test_docker_status_display() {
        assert_eq!(DockerStatus::Available.as_str(), "available");
        assert_eq!(DockerStatus::NotInstalled.as_str(), "not installed");
        assert_eq!(DockerStatus::NotRunning.as_str(), "not running");
        assert_eq!(DockerStatus::Disabled.as_str(), "disabled");
    }

    #[test]
    fn test_docker_status_is_ok() {
        assert!(DockerStatus::Available.is_ok());
        assert!(!DockerStatus::NotInstalled.is_ok());
        assert!(!DockerStatus::NotRunning.is_ok());
        assert!(!DockerStatus::Disabled.is_ok());
    }

    #[tokio::test]
    async fn test_check_docker_returns_valid_status() {
        let result = check_docker().await;
        match result.status {
            DockerStatus::Available | DockerStatus::NotInstalled | DockerStatus::NotRunning => {}
            DockerStatus::Disabled => panic!("check_docker should never return Disabled"),
        }
    }

    #[tokio::test]
    async fn test_check_podman_returns_valid_status() {
        let result = check_podman().await;
        match result.status {
            DockerStatus::Available | DockerStatus::NotInstalled | DockerStatus::NotRunning => {}
            DockerStatus::Disabled => panic!("check_podman should never return Disabled"),
        }
    }

    fn detection(status: DockerStatus) -> DockerDetection {
        DockerDetection {
            status,
            platform: Platform::Linux,
        }
    }

    /// WI#871: a selected runtime is reported by name, never as a Docker
    /// problem — a podman-only host must not read as "sandbox disabled".
    #[test]
    fn sandbox_runtime_summary_names_the_selected_runtime() {
        let selection = SandboxRuntimeDetection {
            runtime: Some(ContainerRuntime::Podman),
            configured: SandboxRuntime::Auto,
            tried: vec![(ContainerRuntime::Podman, detection(DockerStatus::Available))],
        };
        let summary = selection.summary();
        assert_eq!(summary, "sandbox runtime: podman");
        assert!(!summary.contains("sandbox disabled"), "{summary}");
        assert!(!summary.contains("Docker"), "{summary}");
    }

    #[test]
    fn sandbox_runtime_summary_only_says_disabled_when_every_runtime_failed() {
        let selection = SandboxRuntimeDetection {
            runtime: None,
            configured: SandboxRuntime::Auto,
            tried: vec![
                (
                    ContainerRuntime::Podman,
                    detection(DockerStatus::NotInstalled),
                ),
                (
                    ContainerRuntime::Docker,
                    detection(DockerStatus::NotRunning),
                ),
            ],
        };
        let summary = selection.summary();
        assert!(summary.contains("sandbox disabled"), "{summary}");
        assert!(summary.contains("podman is not installed"), "{summary}");
        assert!(
            summary.contains("docker is installed but not running"),
            "{summary}"
        );
    }
}
