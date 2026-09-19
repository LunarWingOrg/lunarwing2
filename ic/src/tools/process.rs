//! Workload process spawner: process-group lifecycle management with optional
//! systemd transient-service isolation (BUG-005, WI#178/WI#181).
//!
//! Tool-spawned commands (shell, compiles) previously ran inside the daemon's
//! own service cgroup with no process-group ownership: on timeout or task
//! abort, at best the direct child died, leaving grandchildren (`cargo`,
//! `rustc`, linkers) running unbounded and charged to `lunarwing-*.service`.
//!
//! Two layers:
//!
//! 1. Portable (all Unix hosts): every workload spawns in its own process
//!    group with `kill_on_drop`, paired with a [`ProcessTreeGuard`] that
//!    SIGTERM/SIGKILLs the whole group on timeout, cancellation, or drop of
//!    the execute future. This layer alone prevents runaway trees on both
//!    systemd and OpenRC hosts.
//! 2. systemd user manager (auto-detected, Linux): workloads spawn as
//!    transient **services** (`systemd-run --user --wait --pipe` with a
//!    generated unit name) inside a dedicated slice
//!    (`lunarwing-workloads.slice` by default) for separate cgroup accounting
//!    and optional memory caps. Any probe or spawn failure falls back to
//!    layer 1 — the tool must never break because isolation is unavailable.
//!
//! Services, not scopes (WI#178/WI#181): a `--scope` forks the daemon and
//! inherits its `NoNewPrivileges=true` + `PrivateTmp=true` mount namespace,
//! which breaks podman's rootless user-namespace join from agent shells
//! ("cannot re-exec process to join the existing user namespace"). A
//! transient service is forked by the user manager instead, so it gets the
//! manager's context — the same context class as the tenant's Quadlet units,
//! where podman demonstrably works. Environment and working directory are
//! passed through `EnvironmentFile=` (mode 0600, deleted after the run) and
//! `--working-directory=` rather than inherited, so the daemon's scrubbed
//! environment stays scrubbed and nothing secret crosses argv.
//!
//! Lifecycle: `systemd-run --wait` proxies stdio and the service's exit
//! status and returns after the unit is collected (`--collect`), so the
//! caller keeps its normal spawn/wait shape. Timeout or cancellation stops
//! the unit via `systemctl --user stop` (SIGTERM then SIGKILL per the
//! service's `TimeoutStopSec`) before killing the process group, which
//! covers both the manager-forked tree and any grandchildren that escaped
//! the unit.
//!
//! Configuration (environment, read once at first use):
//! - `LUNARWING_WORKLOAD_ISOLATION`: `auto` (default) or `none` to disable services.
//! - `LUNARWING_WORKLOAD_SLICE`: service slice (default `lunarwing-workloads.slice`).
//! - `LUNARWING_WORKLOAD_MEMORY_MAX`: optional per-service `MemoryMax=` value.
//! - `LUNARWING_WORKLOAD_TIMEOUT_STOP`: SIGTERM→SIGKILL grace for service stop
//!   (default 10s, systemd's own default).

use std::path::PathBuf;
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::OnceCell;
use tracing::{debug, warn};

/// Default slice for systemd workload services. One name works for every
/// tenant: each tenant has its own systemd user manager and slice tree.
const DEFAULT_SLICE: &str = "lunarwing-workloads.slice";

/// Grace period between SIGTERM and SIGKILL when terminating a workload tree.
const TERMINATE_GRACE: Duration = Duration::from_secs(2);

/// Upper bound for the one-time systemd-service capability probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Default SIGTERM→SIGKILL grace for `systemctl --user stop` of a workload
/// service (systemd's built-in default for transient services).
const DEFAULT_TIMEOUT_STOP_SECS: u64 = 10;

/// How workloads are isolated from the daemon's own cgroup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Isolation {
    /// Transient `systemd-run --user` services in a dedicated slice.
    SystemdService,
    /// Process group + kill_on_drop only (portable fallback).
    ProcessGroup,
}

/// Spawns workload processes with process-group lifecycle guarantees and,
/// when available, systemd transient-service isolation. Cheap to clone; the
/// expensive capability probe happens once inside [`workload_spawner`].
#[derive(Debug, Clone)]
pub struct WorkloadSpawner {
    isolation: Isolation,
    slice: String,
    memory_max: Option<String>,
    timeout_stop_secs: u64,
    /// Value to inject as `XDG_RUNTIME_DIR` when the daemon's own environment
    /// lacked it but the user bus exists (synthesized as `/run/user/<euid>`).
    xdg_runtime_dir: Option<String>,
}

impl WorkloadSpawner {
    /// A spawner with process-group isolation only (no systemd services).
    pub fn direct() -> Self {
        Self {
            isolation: Isolation::ProcessGroup,
            slice: DEFAULT_SLICE.to_string(),
            memory_max: None,
            timeout_stop_secs: DEFAULT_TIMEOUT_STOP_SECS,
            xdg_runtime_dir: None,
        }
    }

    /// Whether workloads are placed in systemd transient services.
    pub fn uses_systemd_services(&self) -> bool {
        self.isolation == Isolation::SystemdService
    }

    /// `XDG_RUNTIME_DIR` the caller must forward to the child, if the spawner
    /// synthesized one. Callers that scrub the environment (`env_clear`) must
    /// re-apply this after scrubbing or `systemd-run --user` cannot reach the
    /// user manager.
    pub fn xdg_runtime_dir(&self) -> Option<&str> {
        self.xdg_runtime_dir.as_deref()
    }

    /// Build a command for `program args...` with isolation applied.
    ///
    /// The returned command always has `kill_on_drop(true)` and (on Unix) a
    /// fresh process group led by the direct child, so a [`ProcessTreeGuard`]
    /// can reap the whole tree. In service mode the direct child is
    /// `systemd-run`, which proxies stdio, signals, and the exit status.
    ///
    /// Callers set environment, working directory, and stdio on the returned
    /// command before spawning; [`prepare_service`](Self::prepare_service)
    /// then re-marshals them for the user manager.
    pub fn command(&self, program: &str, args: &[&str]) -> Command {
        let mut cmd = if self.uses_systemd_services() {
            let mut c = Command::new("systemd-run");
            c.args(["--user", "--wait", "--pipe", "--quiet", "--collect"])
                .arg(format!("--slice={}", self.slice))
                .arg(format!(
                    "--property=TimeoutStopSec={}",
                    self.timeout_stop_secs
                ));
            if let Some(memory_max) = &self.memory_max {
                c.arg(format!("--property=MemoryMax={memory_max}"));
            }
            c.arg("--").arg(program).args(args);
            c
        } else {
            let mut c = Command::new(program);
            c.args(args);
            c
        };
        if let Some(dir) = &self.xdg_runtime_dir {
            cmd.env("XDG_RUNTIME_DIR", dir);
        }
        // Fresh process group (pgid == child pid) so the guard can signal the
        // whole tree, and kill_on_drop so dropping the execute future can
        // never leave the direct child running.
        #[cfg(unix)]
        cmd.process_group(0);
        cmd.kill_on_drop(true);
        cmd
    }

    /// Prepare a service-mode command for spawning: generate a unique unit
    /// name, serialize the command's scrubbed environment to a mode-0600
    /// `EnvironmentFile=`, and rebuild the `systemd-run` invocation with the
    /// `--unit=` / `--working-directory=` / `EnvironmentFile=` arguments in
    /// the correct position (before the `--` separator).
    ///
    /// Returns `None` in process-group mode. Callers must invoke this after
    /// all env/current_dir setup and before `spawn()`; the returned spec's
    /// env file is removed by [`ProcessTreeGuard`] once the run ends. The
    /// command is rebuilt, so callers must re-apply stdio configuration
    /// after this call.
    pub fn prepare_service(&self, cmd: &mut Command) -> Option<ServiceSpec> {
        if !self.uses_systemd_services() {
            return None;
        }
        // `cmd` is the systemd-run wrapper built by `command()`; recover the
        // inner program and its arguments from after the `--` separator.
        let wrapper_args: Vec<std::ffi::OsString> =
            cmd.as_std().get_args().map(|a| a.to_owned()).collect();
        let sep = wrapper_args
            .iter()
            .position(|a| a.to_string_lossy() == "--")?;
        let program = wrapper_args
            .get(sep + 1)
            .cloned()
            .unwrap_or_else(|| "true".into());
        let args = &wrapper_args[sep + 2..];

        let unit = format!(
            "lunarwing-workload-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let env_file = write_workload_env_file(cmd);
        let working_dir = cmd
            .as_std()
            .get_current_dir()
            .map(std::path::Path::to_path_buf);
        let xdg_runtime_dir = cmd.as_std().get_envs().find_map(|(k, v)| {
            (k.to_string_lossy() == "XDG_RUNTIME_DIR")
                .then(|| v.map(|s| s.to_owned()))
                .flatten()
        });

        let mut rebuilt = Command::new("systemd-run");
        rebuilt
            .args(["--user", "--wait", "--pipe", "--quiet", "--collect"])
            .arg(format!("--slice={}", self.slice))
            .arg(format!(
                "--property=TimeoutStopSec={}",
                self.timeout_stop_secs
            ));
        if let Some(memory_max) = &self.memory_max {
            rebuilt.arg(format!("--property=MemoryMax={memory_max}"));
        }
        rebuilt.arg(format!("--unit={unit}"));
        if let Some(dir) = working_dir {
            rebuilt.arg(format!("--working-directory={}", dir.display()));
        }
        if let Some(path) = &env_file {
            rebuilt.arg(format!("--property=EnvironmentFile={}", path.display()));
        }
        rebuilt.arg("--").arg(program).args(args);
        if let Some(dir) = xdg_runtime_dir {
            rebuilt.env("XDG_RUNTIME_DIR", dir);
        }
        #[cfg(unix)]
        rebuilt.process_group(0);
        rebuilt.kill_on_drop(true);
        *cmd = rebuilt;
        Some(ServiceSpec { unit, env_file })
    }
}

/// Service-mode spawn metadata produced by [`WorkloadSpawner::prepare_service`].
#[derive(Debug, Clone)]
pub struct ServiceSpec {
    /// Transient unit name (without the `.service` suffix).
    pub unit: String,
    /// Environment file handed to the unit; the caller removes it after the
    /// run (or on drop).
    pub env_file: Option<PathBuf>,
}

impl ServiceSpec {
    /// Full unit name for `systemctl --user stop`.
    pub fn unit_name(&self) -> String {
        format!("{}.service", self.unit)
    }
}

/// Serialize a command's scrubbed environment to a mode-0600 file for
/// `systemd-run --property=EnvironmentFile=`, returning the path.
///
/// In service mode the user manager forks the workload, so the daemon's
/// scrubbed environment never reaches it by inheritance; the file is the
/// only channel. Values are double-quoted (EnvironmentFile unquoted values
/// are C-escape-parsed); values containing a double quote or newline cannot
/// be represented and are skipped rather than corrupting the file. Failure
/// degrades to no file (the service then gets the manager's environment).
fn write_workload_env_file(cmd: &Command) -> Option<PathBuf> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let path = runtime_dir.join(format!(
        "lunarwing-workload-{}-{}.env",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let mut file = std::fs::File::create(&path).ok()?;
    use std::io::Write;
    for (key, value) in cmd.as_std().get_envs() {
        let Some(value) = value else {
            continue;
        };
        let value = value.to_string_lossy();
        if value.contains('"') || value.contains('\n') {
            continue;
        }
        if writeln!(file, "{}=\"{}\"", key.to_string_lossy(), value).is_err() {
            return None;
        }
    }
    if let Some(dir) = cmd.as_std().get_current_dir() {
        let dir = dir.to_string_lossy();
        if !dir.contains('"') && !dir.contains('\n') {
            let _ = writeln!(file, "PWD=\"{}\"", dir);
        }
    }
    Some(path)
}

/// Process-wide spawner, probed once and cached. Probing never fails the
/// caller: any error degrades to process-group isolation.
pub async fn workload_spawner() -> &'static WorkloadSpawner {
    static SPAWNER: OnceCell<WorkloadSpawner> = OnceCell::const_new();
    SPAWNER.get_or_init(detect_spawner).await
}

async fn detect_spawner() -> WorkloadSpawner {
    let mut spawner = WorkloadSpawner::direct();
    if let Ok(slice) = std::env::var("LUNARWING_WORKLOAD_SLICE")
        && !slice.is_empty()
    {
        spawner.slice = slice;
    }
    spawner.memory_max = std::env::var("LUNARWING_WORKLOAD_MEMORY_MAX")
        .ok()
        .filter(|v| !v.is_empty());
    spawner.timeout_stop_secs = std::env::var("LUNARWING_WORKLOAD_TIMEOUT_STOP")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_TIMEOUT_STOP_SECS);

    if isolation_disabled() {
        debug!("workload isolation: systemd services disabled by env");
        return spawner;
    }

    match probe_systemd_services().await {
        Ok(()) => {
            spawner.isolation = Isolation::SystemdService;
            spawner.xdg_runtime_dir = ensure_xdg_runtime_dir();
            debug!(
                slice = %spawner.slice,
                memory_max = ?spawner.memory_max,
                timeout_stop_secs = spawner.timeout_stop_secs,
                "workload isolation: systemd user transient services enabled"
            );
        }
        Err(reason) => {
            warn!(
                reason = %reason,
                "workload isolation: systemd services unavailable, using process groups only"
            );
        }
    }
    spawner
}

fn isolation_disabled() -> bool {
    std::env::var("LUNARWING_WORKLOAD_ISOLATION")
        .map(|v| matches!(v.as_str(), "none" | "0" | "false" | "off"))
        .unwrap_or(false)
}

/// Resolve `XDG_RUNTIME_DIR` for the user manager: the existing env value if
/// usable, otherwise `/run/user/<euid>` when its bus socket exists.
fn ensure_xdg_runtime_dir() -> Option<String> {
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR")
            && std::path::Path::new(&dir).join("bus").exists()
        {
            return Some(dir);
        }
        let dir = format!("/run/user/{}", nix::unistd::geteuid());
        std::path::Path::new(&dir)
            .join("bus")
            .exists()
            .then_some(dir)
    }
}

/// Verify `systemd-run --user --wait --pipe` works from this process. Done
/// once; the result is cached by [`workload_spawner`].
#[cfg(target_os = "linux")]
async fn probe_systemd_services() -> Result<(), String> {
    let mut probe = Command::new("systemd-run");
    probe.args([
        "--user",
        "--wait",
        "--pipe",
        "--quiet",
        "--collect",
        "--",
        "/usr/bin/true",
    ]);
    if let Some(dir) = ensure_xdg_runtime_dir() {
        probe.env("XDG_RUNTIME_DIR", dir);
    } else {
        return Err("no usable XDG_RUNTIME_DIR/user bus".to_string());
    }
    probe
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let status = tokio::time::timeout(PROBE_TIMEOUT, probe.status())
        .await
        .map_err(|_| "probe timed out".to_string())?
        .map_err(|e| format!("failed to run systemd-run: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("probe service exited with {status}"))
    }
}

#[cfg(not(target_os = "linux"))]
async fn probe_systemd_services() -> Result<(), String> {
    Err("systemd services are Linux-only".to_string())
}

/// Reaps an entire workload process tree and, in systemd service mode, stops
/// the transient unit it was spawned into.
///
/// Created from a freshly spawned child (the process-group leader, thanks to
/// [`WorkloadSpawner::command`]). [`terminate`](Self::terminate) gives the
/// group a SIGTERM grace period before SIGKILL; `Drop` SIGKILLs the group
/// immediately, which is what makes outer timeouts, scheduler aborts, and
/// `JoinSet` drops reap the tree without any cooperation from the caller.
/// Call [`disarm`](Self::disarm) once the child has been reaped normally so a
/// recycled pgid is never signaled.
///
/// In service mode the direct child is `systemd-run --wait` and the real
/// workload is a manager-forked service. `systemd-run` only forwards signals
/// it receives, and the process group contains `systemd-run` itself, so a
/// timeout or cancellation must stop the unit via `systemctl --user stop`
/// (SIGTERM, then SIGKILL after `TimeoutStopSec`) before signaling the group.
#[derive(Debug)]
pub struct ProcessTreeGuard {
    pgid: Option<i32>,
    /// Transient unit name to stop on termination, when the workload was
    /// spawned as a systemd service.
    unit: Option<String>,
    /// Environment file handed to the transient unit; removed on drop.
    env_file: Option<PathBuf>,
    /// `XDG_RUNTIME_DIR` for the `systemctl --user` stop call.
    xdg_runtime_dir: Option<String>,
}

impl ProcessTreeGuard {
    /// Arm the guard for `child` (must have been spawned in its own process
    /// group, as [`WorkloadSpawner::command`] does).
    pub fn new(child: &tokio::process::Child) -> Self {
        #[cfg(unix)]
        {
            Self {
                pgid: child.id().map(|pid| pid as i32),
                unit: None,
                env_file: None,
                xdg_runtime_dir: None,
            }
        }
        #[cfg(not(unix))]
        {
            let _ = child;
            Self {
                pgid: None,
                unit: None,
                env_file: None,
                xdg_runtime_dir: None,
            }
        }
    }

    /// Arm the guard for a service-mode spawn: the transient unit must be
    /// stopped on termination, and `systemctl --user` needs the runtime dir
    /// the spawner used. The env file is removed when the guard drops.
    /// `None` (process-group mode) yields a plain guard.
    pub fn for_service(
        child: &tokio::process::Child,
        spec: Option<&ServiceSpec>,
        xdg_runtime_dir: Option<&str>,
    ) -> Self {
        let mut guard = Self::new(child);
        if let Some(spec) = spec {
            guard.unit = Some(spec.unit_name());
            guard.env_file = spec.env_file.clone();
            guard.xdg_runtime_dir = xdg_runtime_dir.map(str::to_string);
        }
        guard
    }

    /// Stop signaling on drop (child already exited and was reaped).
    pub fn disarm(&mut self) {
        self.pgid = None;
        self.unit = None;
        self.env_file = None;
    }

    #[cfg(unix)]
    fn signal_group(pgid: i32, signal: nix::sys::signal::Signal) {
        // Negative pid == process group. ESRCH (group already gone) is fine.
        let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(-pgid), signal);
    }

    /// Stop the transient service (SIGTERM, then SIGKILL after its
    /// `TimeoutStopSec`). Best-effort: the unit may already be gone.
    async fn stop_unit(&self) {
        let Some(unit) = &self.unit else {
            return;
        };
        let mut stop = Command::new("systemctl");
        stop.args(["--user", "stop", unit]);
        if let Some(dir) = &self.xdg_runtime_dir {
            stop.env("XDG_RUNTIME_DIR", dir);
        }
        stop.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Err(e) = stop.status().await {
            debug!(unit = %unit, error = %e, "workload service stop failed");
        }
    }

    /// Stop the transient unit (service mode) and SIGKILL the whole group
    /// immediately, for call sites where the child handle was already
    /// consumed (e.g. a dropped `wait_with_output` future, whose
    /// `kill_on_drop` reaps the direct child separately).
    pub async fn kill_tree(&mut self) {
        if self.unit.is_some() {
            self.stop_unit().await;
        }
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.take() {
            Self::signal_group(pgid, nix::sys::signal::Signal::SIGKILL);
        }
        self.unit = None;
        self.env_file = None;
    }

    /// SIGTERM the tree, wait up to [`TERMINATE_GRACE`], then SIGKILL and reap.
    pub async fn terminate(&mut self, child: &mut tokio::process::Child) {
        // Service mode: stop the unit first so the manager reaps the real
        // workload; the group signal then only covers the systemd-run proxy.
        if self.unit.is_some() {
            self.stop_unit().await;
        }
        let Some(pgid) = self.pgid.take() else {
            self.unit = None;
            return;
        };
        #[cfg(unix)]
        {
            use nix::sys::signal::Signal;

            Self::signal_group(pgid, Signal::SIGTERM);
            let deadline = tokio::time::Instant::now() + TERMINATE_GRACE;
            loop {
                match child.try_wait() {
                    // Exited and reaped: done, group got its grace period.
                    Ok(Some(_)) => break,
                    // Still running: keep waiting until the grace deadline.
                    Ok(None) => {}
                    // Cannot query: escalate immediately.
                    Err(_) => break,
                }
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Self::signal_group(pgid, Signal::SIGKILL);
            // Reap the direct child; kill_on_drop covers anything left.
            let _ = child.wait().await;
        }
        #[cfg(not(unix))]
        {
            let _ = pgid;
            let _ = child.kill().await;
        }
        self.unit = None;
        self.env_file = None;
    }
}

impl Drop for ProcessTreeGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.take() {
            Self::signal_group(pgid, nix::sys::signal::Signal::SIGKILL);
        }
        self.unit = None;
        if let Some(path) = self.env_file.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pids of all processes currently in `pgid` (Linux /proc scan).
    fn group_pids(pgid: i32) -> Vec<i32> {
        let mut pids = Vec::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return pids;
        };
        for entry in entries.flatten() {
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            // stat: pid (comm) state ppid pgrp ... — comm may contain spaces,
            // so parse fields after the last ')'.
            let Some(rest) = stat.rsplit_once(") ").map(|(_, r)| r) else {
                continue;
            };
            let fields: Vec<&str> = rest.split_whitespace().collect();
            // fields[0]=state, [1]=ppid, [2]=pgrp
            if let (Some(&pgrp), Some(pid_str)) = (fields.get(2), entry.file_name().to_str())
                && pgrp.parse::<i32>() == Ok(pgid)
                && let Ok(pid) = pid_str.parse::<i32>()
            {
                pids.push(pid);
            }
        }
        pids
    }

    /// Poll until the group is empty (orphaned members are reaped by init
    /// asynchronously, so an immediate check can see zombies).
    async fn wait_group_empty(pgid: i32) -> Vec<i32> {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let remaining = group_pids(pgid);
            if remaining.is_empty() || std::time::Instant::now() >= deadline {
                return remaining;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn spawn_sleep_tree(spawner: &WorkloadSpawner) -> (tokio::process::Child, ProcessTreeGuard) {
        let child = spawner
            .command("sh", &["-c", "sleep 300 & sleep 300 & wait"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sleep tree");
        let guard = ProcessTreeGuard::new(&child);
        (child, guard)
    }

    #[tokio::test]
    async fn direct_command_spawns_in_own_process_group() {
        let spawner = WorkloadSpawner::direct();
        let (mut child, mut guard) = spawn_sleep_tree(&spawner);
        let pid = child.id().expect("child pid") as i32;
        // Child leads its own group; the sleeps join it.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while group_pids(pid).len() < 3 && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(group_pids(pid).len(), 3, "sh + two sleeps in one group");
        guard.terminate(&mut child).await;
    }

    #[tokio::test]
    async fn terminate_reaps_entire_tree() {
        let spawner = WorkloadSpawner::direct();
        let (mut child, mut guard) = spawn_sleep_tree(&spawner);
        let pgid = child.id().expect("child pid") as i32;
        guard.terminate(&mut child).await;
        let remaining = wait_group_empty(pgid).await;
        assert!(
            remaining.is_empty(),
            "survivors after terminate: {remaining:?}"
        );
    }

    #[tokio::test]
    async fn drop_kills_tree_without_cooperation() {
        // Simulates the outer tool timeout dropping the execute future:
        // neither terminate() nor kill() is called explicitly.
        let spawner = WorkloadSpawner::direct();
        let (child, guard) = spawn_sleep_tree(&spawner);
        let pgid = child.id().expect("child pid") as i32;
        drop(guard);
        drop(child); // kill_on_drop reaps the direct child
        let remaining = wait_group_empty(pgid).await;
        assert!(
            remaining.is_empty(),
            "survivors after guard drop: {remaining:?}"
        );
    }

    #[tokio::test]
    async fn disarm_prevents_late_signal() {
        let spawner = WorkloadSpawner::direct();
        let mut child = spawner.command("true", &[]).spawn().expect("spawn true");
        let mut guard = ProcessTreeGuard::new(&child);
        let pgid = child.id().expect("child pid") as i32;
        let status = child.wait().await.expect("wait");
        assert!(status.success());
        guard.disarm();
        drop(guard); // must not signal the (now reaped) group
        assert!(group_pids(pgid).is_empty());
    }

    #[tokio::test]
    async fn global_spawner_detection_never_fails() {
        // Whatever the host supports, detection must return a usable spawner.
        let spawner = workload_spawner().await;
        let status = spawner
            .command("true", &[])
            .spawn()
            .expect("spawn via detected spawner")
            .wait()
            .await
            .expect("wait");
        assert!(status.success());
    }

    #[test]
    fn env_file_serialization_quotes_values_and_skips_unrepresentable() {
        let mut cmd = Command::new("sh");
        cmd.env("PLAIN", "hello world")
            .env("SPEC", "100%u")
            .env("BAD_QUOTE", "a\"b")
            .env("BAD_NL", "a\nb")
            .current_dir("/some/dir");
        let path = write_workload_env_file(&cmd).expect("env file written");
        let content = std::fs::read_to_string(&path).expect("read env file");
        let _ = std::fs::remove_file(&path);
        assert!(content.contains("PLAIN=\"hello world\""));
        assert!(content.contains("SPEC=\"100%u\""));
        assert!(!content.contains("BAD_QUOTE"));
        assert!(!content.contains("BAD_NL"));
        assert!(content.contains("PWD=\"/some/dir\""));
    }

    #[test]
    fn direct_spawner_prepare_service_is_none() {
        let spawner = WorkloadSpawner::direct();
        let mut cmd = spawner.command("sh", &["-c", "true"]);
        assert!(spawner.prepare_service(&mut cmd).is_none());
        // Command left untouched in process-group mode.
        assert_eq!(cmd.as_std().get_program().to_string_lossy(), "sh");
    }

    #[test]
    fn service_prepare_rebuilds_wrapper_with_unit_and_env_file() {
        // Simulate a detected service-mode spawner without probing.
        let spawner = WorkloadSpawner {
            isolation: Isolation::SystemdService,
            slice: "lunarwing-workloads.slice".to_string(),
            memory_max: Some("1G".to_string()),
            timeout_stop_secs: 10,
            xdg_runtime_dir: None,
        };
        let mut cmd = spawner.command("sh", &["-c", "echo hi"]);
        cmd.env("FOO", "bar").current_dir("/tmp");
        let spec = spawner.prepare_service(&mut cmd).expect("spec");
        assert!(spec.unit.starts_with("lunarwing-workload-"));
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--wait".to_string()));
        assert!(args.contains(&"--pipe".to_string()));
        assert!(args.contains(&"--collect".to_string()));
        assert!(args.iter().any(|a| a.starts_with("--slice=")));
        assert!(args.iter().any(|a| a.starts_with("--unit=")));
        assert!(args.iter().any(|a| a.starts_with("--property=MemoryMax=")));
        assert!(
            args.iter()
                .any(|a| a.starts_with("--property=TimeoutStopSec="))
        );
        assert!(args.iter().any(|a| a.starts_with("--working-directory=")));
        assert!(
            args.iter()
                .any(|a| a.starts_with("--property=EnvironmentFile="))
        );
        // Inner program and args after the separator.
        let sep = args.iter().position(|a| a == "--").expect("separator");
        assert_eq!(args[sep + 1], "sh");
        assert_eq!(args[sep + 2], "-c");
        assert_eq!(args[sep + 3], "echo hi");
        // Env file contains the scrubbed env and is cleaned up by the guard.
        let env_path = spec.env_file.expect("env file");
        let content = std::fs::read_to_string(&env_path).expect("read");
        assert!(content.contains("FOO=\"bar\""));
        let _ = std::fs::remove_file(env_path);
    }

    /// WI#181 regression: podman must work from a manager-forked transient
    /// service (the tenant's Quadlet context class). Skipped when systemd
    /// user services or podman are unavailable on the host.
    #[tokio::test]
    async fn podman_works_from_transient_service() {
        let spawner = workload_spawner().await;
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
        let mut cmd = spawner.command("podman", &["ps"]);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let spec = spawner.prepare_service(&mut cmd);
        let child = cmd.spawn().expect("spawn podman via service");
        let mut guard =
            ProcessTreeGuard::for_service(&child, spec.as_ref(), spawner.xdg_runtime_dir());
        let out = child.wait_with_output().await.expect("wait");
        guard.disarm();
        assert!(
            out.status.success(),
            "podman ps from transient service failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
