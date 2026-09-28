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
//! Exact workload environment (WI#181): `EnvironmentFile=` only adds to the
//! environment the user manager passes down, and `UnsetEnvironment=` takes
//! explicit variable names (no wildcard form — `*` would be a literal name).
//! The spawner therefore records the manager's environment once at
//! detection time by running `env -0` inside the capability probe (a real
//! manager-forked service, so the capture includes variables the manager
//! itself injects), and each `prepare_service` adds one
//! `--property=UnsetEnvironment=<name1> <name2> ...` covering every
//! captured name the caller does not set itself. systemd applies the unset
//! list as the final step, so the unit's environment is exactly the
//! caller's allowlisted assignments from the file: a secret a tenant places
//! in the manager's environment (e.g. via `systemctl --user
//! set-environment`) reaches only workloads whose caller explicitly
//! forwarded it. Names added to the manager after daemon start are not in
//! the snapshot and still inherit (documented drift; the probe runs once
//! per daemon by design).
//!
//! Lifecycle: `systemd-run --wait` proxies stdio and the service's exit
//! status and returns after the unit is collected (`--collect`), so the
//! caller keeps its normal spawn/wait shape. Timeout or cancellation stops
//! the unit via `systemctl --user stop` (SIGTERM then SIGKILL per the
//! service's `TimeoutStopSec`) before killing the process group, which
//! covers both the manager-forked tree and any grandchildren that escaped
//! the unit.
//!
//! Drop safety: `Drop` cannot await, and the workload lives outside the
//! client's process group (the manager forks it). The guard therefore hands
//! any still-owned unit to a process-wide background task that stops it and
//! blocks on a semaphore bounded by `LUNARWING_WORKLOAD_DROP_STOPS`
//! (default 16) — no stop future is ever left unpolled, so a dropped
//! execute future can never leave the manager-owned workload running.
//! `ProcessTreeGuard::finish()` waits for that stop to complete and is the
//! cancellation-safe finalization call sites should use after dropping a
//! workload's execution future.
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
    /// The user manager's environment, captured once at detection time by
    /// running `env -0` inside the capability probe (a real manager-forked
    /// service, so the capture includes manager-injected variables). Every
    /// name here becomes an `UnsetEnvironment=<name>` property on the
    /// workload unit, so the `EnvironmentFile=` is the unit's exact
    /// environment — see the module docs on exact workload environments.
    manager_env: std::sync::Arc<Vec<(std::ffi::OsString, std::ffi::OsString)>>,
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
            manager_env: std::sync::Arc::new(Vec::new()),
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
        let env_file = write_workload_env_file(cmd, self.xdg_runtime_dir.as_deref());
        let working_dir = cmd
            .as_std()
            .get_current_dir()
            .map(std::path::Path::to_path_buf);
        let xdg_runtime_dir = cmd.as_std().get_envs().find_map(|(k, v)| {
            (k.to_string_lossy() == "XDG_RUNTIME_DIR")
                .then(|| v.map(|s| s.to_owned()))
                .flatten()
        });
        // Names the caller sets itself (including PWD synthesized for the
        // working directory): they must survive, so they are excluded from
        // the unset list — systemd applies UnsetEnvironment last and would
        // otherwise strip them right back out of the env file.
        let mut caller_names: std::collections::HashSet<String> = cmd
            .as_std()
            .get_envs()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        if working_dir.is_some() {
            caller_names.insert("PWD".to_string());
        }

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
        // The user manager hands its environment to the service, and
        // `EnvironmentFile=` only adds to it: without an unset list, a
        // manager-borne credential would reach the workload despite the
        // caller's `env_clear()`. Unset every captured manager variable the
        // caller does not set itself, so the env file is the unit's exact
        // environment (systemd applies UnsetEnvironment after all other
        // sources, including the file). Only emitted with an env file:
        // unsetting without additions would leave the service with no
        // environment at all.
        if env_file.is_some()
            && let Some(property) = unset_environment_property(&self.manager_env, &caller_names)
        {
            rebuilt.arg(property);
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

/// Serialize the command's scrubbed environment to a mode-0600 file for
/// `systemd-run --property=EnvironmentFile=`, returning the path.
///
/// In service mode the user manager forks the workload, so the daemon's
/// environment never reaches it by inheritance; the file is the channel.
/// The manager's own environment is cleared separately via the captured
/// `UnsetEnvironment=` list (see [`unset_environment_property`]), so this
/// file's assignments are the unit's exact environment. Values are
/// double-quoted (EnvironmentFile unquoted values are C-escape-parsed);
/// values containing a double quote or newline cannot be represented and
/// are skipped rather than corrupting the file. The file lives in the
/// spawner's `XDG_RUNTIME_DIR` when available (the same runtime dir
/// `systemd-run` uses, so the manager can read it) and falls back to the
/// temp dir otherwise. Failure degrades to no file, and the caller must
/// then also skip the unset property (an unset property with no env file
/// would empty the service's environment entirely).
fn write_workload_env_file(cmd: &Command, runtime_dir: Option<&str>) -> Option<PathBuf> {
    let dir = runtime_dir
        .map(PathBuf::from)
        .or_else(|| std::env::var("XDG_RUNTIME_DIR").ok().map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    let path = dir.join(format!(
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

/// Build the `--property=UnsetEnvironment=` argument that clears the user
/// manager's environment from a workload unit: one space-separated entry
/// per captured variable name (name-only entries match any value, so no
/// assignment text needs quoting), minus names the caller sets itself —
/// systemd applies `UnsetEnvironment` last, so including those would strip
/// the workload's own allowlisted assignments. Returns `None` when there is
/// nothing to unset. The list is capped at [`UNSET_ENV_MAX_VARS`] entries
/// so a pathological manager environment cannot produce an unbounded argv.
const UNSET_ENV_MAX_VARS: usize = 512;

fn unset_environment_property(
    manager_env: &[(std::ffi::OsString, std::ffi::OsString)],
    caller_names: &std::collections::HashSet<String>,
) -> Option<String> {
    let mut names: Vec<String> = Vec::with_capacity(manager_env.len().min(UNSET_ENV_MAX_VARS));
    for (key, _) in manager_env.iter().take(UNSET_ENV_MAX_VARS) {
        let key = key.to_string_lossy();
        // Environment variable names cannot contain spaces; a captured
        // entry that does cannot be a valid name — skip it.
        if key.contains(' ') {
            continue;
        }
        if caller_names.contains(key.as_ref()) {
            continue;
        }
        names.push(key.into_owned());
    }
    if names.is_empty() {
        return None;
    }
    Some(format!("--property=UnsetEnvironment={}", names.join(" ")))
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
        Ok(captured_env) => {
            spawner.isolation = Isolation::SystemdService;
            spawner.xdg_runtime_dir = ensure_xdg_runtime_dir();
            spawner.manager_env = std::sync::Arc::new(split_env_entries(&captured_env));
            debug!(
                slice = %spawner.slice,
                memory_max = ?spawner.memory_max,
                timeout_stop_secs = spawner.timeout_stop_secs,
                manager_env_vars = spawner.manager_env.len(),
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

/// Verify `systemd-run --user --wait --pipe` works from this process, and
/// capture the environment a manager-forked service inherits (the output of
/// `env -0` run inside the probe service — the manager's own environment at
/// capture time). Done once; the result is cached by [`workload_spawner`].
/// The capture is what makes workload environments exact: it is the
/// inherited layer every service would see, serialized into each
/// `EnvironmentFile=` so caller additions are the only deltas.
#[cfg(target_os = "linux")]
async fn probe_systemd_services() -> Result<Vec<std::ffi::OsString>, String> {
    let mut probe = Command::new("systemd-run");
    probe.args([
        "--user",
        "--wait",
        "--pipe",
        "--quiet",
        "--collect",
        "--",
        "env",
        "-0",
    ]);
    if let Some(dir) = ensure_xdg_runtime_dir() {
        probe.env("XDG_RUNTIME_DIR", dir);
    } else {
        return Err("no usable XDG_RUNTIME_DIR/user bus".to_string());
    }
    probe
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());

    let output = tokio::time::timeout(PROBE_TIMEOUT, probe.output())
        .await
        .map_err(|_| "probe timed out".to_string())?
        .map_err(|e| format!("failed to run systemd-run: {e}"))?;
    if !output.status.success() {
        return Err(format!("probe service exited with {}", output.status));
    }
    Ok(parse_env_nul(&output.stdout))
}

/// Parse NUL-delimited `env -0` output into variable entries (order
/// preserved; entries without `=` are ignored).
fn parse_env_nul(bytes: &[u8]) -> Vec<std::ffi::OsString> {
    use std::os::unix::ffi::OsStrExt;
    bytes
        .split(|b| *b == 0)
        .filter(|entry| !entry.is_empty() && entry.contains(&b'='))
        .map(|entry| std::ffi::OsStr::from_bytes(entry).to_os_string())
        .collect()
}

#[cfg(not(target_os = "linux"))]
async fn probe_systemd_services() -> Result<Vec<std::ffi::OsString>, String> {
    Err("systemd services are Linux-only".to_string())
}

/// Parse captured `env -0` entries into (name, value) pairs.
fn split_env_entries(
    entries: &[std::ffi::OsString],
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    use std::os::unix::ffi::OsStrExt;
    entries
        .iter()
        .filter_map(|entry| {
            let bytes = entry.as_bytes();
            let eq = bytes.iter().position(|b| *b == b'=')?;
            Some((
                std::ffi::OsStr::from_bytes(&bytes[..eq]).to_os_string(),
                std::ffi::OsStr::from_bytes(&bytes[eq + 1..]).to_os_string(),
            ))
        })
        .collect()
}

/// Per-process stop queue for workload units orphaned by a dropped guard.
///
/// `Drop` cannot await, and the manager-forked workload survives the SIGKILL
/// of the `systemd-run` client, so the guard hands the unit here instead of
/// forgetting it. The queue task polls every stop future to completion — the
/// stop is never left dangling, which is what makes guard-drop cleanup
/// cancellation-safe. The queue is unbounded and each stop is a cheap
/// `systemctl --user stop`, so backpressure is not needed; the semaphore only
/// bounds how many stop processes run at once.
static STOP_QUEUE: std::sync::OnceLock<tokio::sync::mpsc::UnboundedSender<StopUnit>> =
    std::sync::OnceLock::new();

struct StopUnit {
    unit: String,
    xdg_runtime_dir: Option<String>,
}

/// Upper bound for concurrent drop-enqueued unit stops.
const DEFAULT_DROP_STOPS: usize = 16;

fn stop_queue() -> Option<&'static tokio::sync::mpsc::UnboundedSender<StopUnit>> {
    // The queue task needs a reactor; without one there is nothing that can
    // run the stop (daemon-shutdown edge case — the unit then dies with the
    // user manager or at its own TimeoutStopSec on next stop).
    tokio::runtime::Handle::try_current().ok()?;
    Some(STOP_QUEUE.get_or_init(spawn_stop_queue))
}

fn spawn_stop_queue() -> tokio::sync::mpsc::UnboundedSender<StopUnit> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<StopUnit>();
    let max = std::env::var("LUNARWING_WORKLOAD_DROP_STOPS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_DROP_STOPS);
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(max));
    tokio::spawn(async move {
        while let Some(request) = rx.recv().await {
            let permit = semaphore.clone().acquire_owned().await;
            tokio::spawn(async move {
                stop_workload_unit(&request.unit, request.xdg_runtime_dir.as_deref()).await;
                drop(permit);
            });
        }
    });
    tx
}

/// Stop a workload unit from any context, including synchronous `Drop`
/// (no runtime handle required at the call site): dispatches the stop to the
/// process-wide background queue, which runs it to completion.
pub fn stop_unit_background(unit: String, xdg_runtime_dir: Option<&str>) {
    let request = StopUnit {
        unit: unit.clone(),
        xdg_runtime_dir: xdg_runtime_dir.map(str::to_string),
    };
    match stop_queue() {
        Some(tx) if tx.send(request).is_ok() => {}
        _ => warn!(
            unit = %unit,
            "workload stop queue unavailable; unit stop not dispatched from drop"
        ),
    }
}

/// Stop a transient workload unit (SIGTERM, then SIGKILL after its
/// `TimeoutStopSec`). Best-effort: the unit may already be gone.
async fn stop_workload_unit(unit: &str, xdg_runtime_dir: Option<&str>) {
    let mut stop = Command::new("systemctl");
    stop.args(["--user", "stop", unit]);
    if let Some(dir) = xdg_runtime_dir {
        stop.env("XDG_RUNTIME_DIR", dir);
    }
    stop.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Err(e) = stop.status().await {
        debug!(unit = %unit, error = %e, "workload service stop failed");
    }
}

/// Reaps an entire workload process tree and, in systemd service mode, stops
/// the transient unit it was spawned into.
///
/// Created from a freshly spawned child (the process-group leader, thanks to
/// [`WorkloadSpawner::command`]). [`terminate`](Self::terminate) gives the
/// group a SIGTERM grace period before SIGKILL; `Drop` SIGKILLs the group
/// immediately and — in service mode — hands the unit to the background stop
/// queue (drop cannot await, and the manager-forked workload lives outside
/// the group). This is what makes outer timeouts, scheduler aborts, and
/// `JoinSet` drops reap the tree without any cooperation from the caller.
/// Call [`finish`](Self::finish) after dropping a workload's execution
/// future to wait for the unit stop to complete, or
/// [`disarm`](Self::disarm) once the child has been reaped normally so a
/// recycled pgid is never signaled and no stop is dispatched.
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

    /// Remove the transient unit's environment file, if any. Idempotent.
    fn cleanup_env_file(&mut self) {
        if let Some(path) = self.env_file.take() {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Stop signaling on drop (child already exited and was reaped).
    pub fn disarm(&mut self) {
        self.pgid = None;
        self.unit = None;
        self.cleanup_env_file();
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
        stop_workload_unit(unit, self.xdg_runtime_dir.as_deref()).await;
    }

    /// Cancellation-safe finalization for call sites that dropped a
    /// workload's execution future: if a unit stop was dispatched from
    /// `Drop` (or not yet), wait for it to complete. Returns immediately
    /// when no service unit is armed.
    pub async fn finish(&mut self) {
        let Some(unit) = self.unit.take() else {
            self.cleanup_env_file();
            return;
        };
        // The drop path may have already handed this unit to the background
        // queue; stopping again is harmless (systemd's stop is idempotent on
        // inactive units) and guarantees the workload is gone when we
        // return, whatever path armed the stop.
        stop_workload_unit(&unit, self.xdg_runtime_dir.as_deref()).await;
        self.cleanup_env_file();
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
        self.cleanup_env_file();
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
            self.cleanup_env_file();
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
        self.cleanup_env_file();
    }
}

impl Drop for ProcessTreeGuard {
    fn drop(&mut self) {
        // Service mode: the real workload is forked by the user manager and
        // is NOT in the client's process group, so the SIGKILL below only
        // reaps the `systemd-run` proxy. Hand the unit to the background
        // stop queue (drop cannot await) so the manager-owned workload is
        // still stopped on every abandoned path — outer timeouts, task
        // aborts, and `JoinSet` drops.
        if let Some(unit) = self.unit.take() {
            stop_unit_background(unit, self.xdg_runtime_dir.as_deref());
        }
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.take() {
            Self::signal_group(pgid, nix::sys::signal::Signal::SIGKILL);
        }
        self.cleanup_env_file();
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
        let path = write_workload_env_file(&cmd, None).expect("env file written");
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
        // Simulate a detected service-mode spawner without probing. The
        // manager-env capture contains a name the caller also sets (PATH)
        // and two manager-borne names it does not: only the latter two may
        // end up in the unit's UnsetEnvironment= list (systemd applies the
        // unset step last, so caller names must be excluded or the env file
        // values would be stripped right back out).
        let spawner = WorkloadSpawner {
            isolation: Isolation::SystemdService,
            slice: "lunarwing-workloads.slice".to_string(),
            memory_max: Some("1G".to_string()),
            timeout_stop_secs: 10,
            xdg_runtime_dir: None,
            manager_env: std::sync::Arc::new(vec![
                ("HOME".into(), "/root".into()),
                ("MANAGER_SECRET".into(), "hunter2".into()),
                ("PATH".into(), "/usr/local/bin".into()),
            ]),
        };
        let mut cmd = spawner.command("sh", &["-c", "echo hi"]);
        cmd.env("FOO", "bar")
            .env("PATH", "/usr/bin")
            .current_dir("/tmp");
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
        // The manager's environment is cleared — minus names the caller sets
        // itself (systemd applies UnsetEnvironment last) — so the env file
        // is the workload's exact environment (WI#181: a manager-borne
        // credential must not leak into a service-mode workload).
        assert!(
            args.contains(&"--property=UnsetEnvironment=HOME MANAGER_SECRET".to_string()),
            "service mode must unset exactly the manager-only names, got: {args:?}"
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

    #[test]
    fn service_prepare_without_env_file_keeps_manager_environment() {
        // No env file (write failure fallback): unsetting everything would
        // leave the service with an empty environment, so the property is
        // dropped along with the file.
        let spawner = WorkloadSpawner {
            isolation: Isolation::SystemdService,
            slice: "lunarwing-workloads.slice".to_string(),
            memory_max: None,
            timeout_stop_secs: 10,
            // /dev/null is a file, not a directory: File::create fails,
            // write_workload_env_file returns None.
            xdg_runtime_dir: Some("/dev/null".to_string()),
            manager_env: std::sync::Arc::new(vec![("HOME".into(), "/root".into())]),
        };
        let mut cmd = spawner.command("sh", &["-c", "true"]);
        let spec = spawner.prepare_service(&mut cmd).expect("spec");
        assert!(spec.env_file.is_none());
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            !args
                .iter()
                .any(|a| a.starts_with("--property=UnsetEnvironment=")),
            "unset property must be dropped when no env file exists"
        );
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
            .args(["version", "--format", "{{.Client.Version}}"])
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

    /// WI#181 review (MR !1042): in service mode the workload is forked by
    /// the user manager, OUTSIDE the client's process group. Dropping the
    /// execute future (outer timeout / task abort) must still stop the unit
    /// and reap the workload — the drop path hands the stop to the
    /// background queue, so the unit must disappear even when neither
    /// `terminate()` nor `kill_tree()` was ever called.
    #[tokio::test]
    async fn service_drop_stops_manager_forked_workload() {
        let spawner = workload_spawner().await;
        if !spawner.uses_systemd_services() {
            eprintln!("skipping: systemd user transient services unavailable");
            return;
        }
        let mut cmd = spawner.command("sleep", &["317"]);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let spec = spawner.prepare_service(&mut cmd).expect("service spec");
        let unit = spec.unit_name();
        let child = cmd.spawn().expect("spawn sleep via service");
        let guard = ProcessTreeGuard::for_service(&child, Some(&spec), spawner.xdg_runtime_dir());

        // Wait until the unit is active, then simulate a cancelled future:
        // the guard and the child drop without any explicit stop call.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if unit_exists(&unit).await {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "unit {unit} never became active"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        drop(guard);
        drop(child);

        // The background stop queue must clear the unit (and the workload it
        // owns) without the caller awaiting anything beyond this check.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if !unit_exists(&unit).await {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "service {unit} survived guard+child drop"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let survivors = tokio::process::Command::new("pgrep")
            .args(["-f", "sleep 317"])
            .output()
            .await
            .expect("pgrep");
        assert!(
            survivors.status.code() == Some(1),
            "workload survived drop: pgrep exit {:?}",
            survivors.status.code()
        );
    }

    /// WI#181 review (MR !1042): `EnvironmentFile=` only adds assignments;
    /// without the per-name `UnsetEnvironment=` list the service inherits
    /// the user manager's environment, and a non-allowlisted manager
    /// credential would reach the workload despite the caller's
    /// `env_clear()`. Asserting the unit's environment is EXACTLY the
    /// caller-allowlisted set is stronger than a single sentinel: any
    /// manager-borne variable (HOME, USER, LANG, and any credential the
    /// manager carries) leaking through fails the test, and the two
    /// intended values must still survive.
    #[tokio::test]
    async fn service_mode_workload_gets_exact_allowlisted_environment() {
        let spawner = workload_spawner().await;
        if !spawner.uses_systemd_services() {
            eprintln!("skipping: systemd user transient services unavailable");
            return;
        }
        let mut cmd = spawner.command("sh", &["-c", "env"]);
        cmd.env_clear();
        cmd.env("PATH", "/usr/bin:/bin");
        cmd.env("LW2_TEST_SAFE", "safe-value");
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let spec = spawner.prepare_service(&mut cmd);
        let child = cmd.spawn().expect("spawn env dump via service");
        let mut guard =
            ProcessTreeGuard::for_service(&child, spec.as_ref(), spawner.xdg_runtime_dir());
        let out = child.wait_with_output().await.expect("wait");
        guard.disarm();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "env dump failed: {stdout}");
        // The unit's environment must be exactly the allowlisted set: any
        // manager-inherited variable appearing here means UnsetEnvironment
        // did not apply.
        let mut keys: Vec<&str> = stdout
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, _)| k)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["LW2_TEST_SAFE", "PATH"],
            "workload environment is not exactly the caller's allowlist:\n{stdout}"
        );
        assert!(
            stdout.contains("LW2_TEST_SAFE=safe-value"),
            "allowed env lost:\n{stdout}"
        );
        assert!(
            stdout.contains("PATH=/usr/bin:/bin"),
            "scrubbed base PATH lost:\n{stdout}"
        );
    }

    /// Whether a transient unit is still known to the user manager.
    async fn unit_exists(unit: &str) -> bool {
        let mut probe = Command::new("systemctl");
        probe.args(["--user", "--quiet", "is-active", unit]);
        probe
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // is-active exits 0 for active, 3 for inactive; a non-zero exit with
        // no bus (--user unreachable) is treated as "gone" so the skip path
        // of these tests degrades gracefully.
        probe.status().await.map(|s| s.success()).unwrap_or(false)
    }
}
