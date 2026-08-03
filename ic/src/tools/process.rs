//! Workload process spawner: process-group lifecycle management with optional
//! systemd transient-scope isolation (BUG-005).
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
//! 2. systemd user manager (auto-detected, Linux): workloads spawn via
//!    `systemd-run --user --scope` inside a dedicated slice
//!    (`lunarwing-workloads.slice` by default) for separate cgroup accounting
//!    and optional memory caps. Any probe or spawn failure falls back to
//!    layer 1 — the tool must never break because isolation is unavailable.
//!
//! Configuration (environment, read once at first use):
//! - `LUNARWING_WORKLOAD_ISOLATION`: `auto` (default) or `none` to disable scopes.
//! - `LUNARWING_WORKLOAD_SLICE`: scope slice (default `lunarwing-workloads.slice`).
//! - `LUNARWING_WORKLOAD_MEMORY_MAX`: optional per-scope `MemoryMax=` value.

use std::time::Duration;

use tokio::process::Command;
use tokio::sync::OnceCell;
use tracing::{debug, warn};

/// Default slice for systemd workload scopes. One name works for every
/// tenant: each tenant has its own systemd user manager and slice tree.
const DEFAULT_SLICE: &str = "lunarwing-workloads.slice";

/// Grace period between SIGTERM and SIGKILL when terminating a workload tree.
const TERMINATE_GRACE: Duration = Duration::from_secs(2);

/// Upper bound for the one-time systemd-scope capability probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How workloads are isolated from the daemon's own cgroup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Isolation {
    /// `systemd-run --user --scope` into a dedicated slice.
    SystemdScope,
    /// Process group + kill_on_drop only (portable fallback).
    ProcessGroup,
}

/// Spawns workload processes with process-group lifecycle guarantees and,
/// when available, systemd scope isolation. Cheap to clone; the expensive
/// capability probe happens once inside [`workload_spawner`].
#[derive(Debug, Clone)]
pub struct WorkloadSpawner {
    isolation: Isolation,
    slice: String,
    memory_max: Option<String>,
    /// Value to inject as `XDG_RUNTIME_DIR` when the daemon's own environment
    /// lacked it but the user bus exists (synthesized as `/run/user/<euid>`).
    xdg_runtime_dir: Option<String>,
}

impl WorkloadSpawner {
    /// A spawner with process-group isolation only (no systemd scopes).
    pub fn direct() -> Self {
        Self {
            isolation: Isolation::ProcessGroup,
            slice: DEFAULT_SLICE.to_string(),
            memory_max: None,
            xdg_runtime_dir: None,
        }
    }

    /// Whether workloads are placed in systemd transient scopes.
    pub fn uses_systemd_scopes(&self) -> bool {
        self.isolation == Isolation::SystemdScope
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
    /// can reap the whole tree. In scope mode the direct child is
    /// `systemd-run`, which proxies stdio, signals, and the exit status.
    pub fn command(&self, program: &str, args: &[&str]) -> Command {
        let mut cmd = if self.uses_systemd_scopes() {
            let mut c = Command::new("systemd-run");
            c.args(["--user", "--scope", "--quiet", "--collect"])
                .arg(format!("--slice={}", self.slice));
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

    if isolation_disabled() {
        debug!("workload isolation: systemd scopes disabled by env");
        return spawner;
    }

    match probe_systemd_scopes().await {
        Ok(()) => {
            spawner.isolation = Isolation::SystemdScope;
            spawner.xdg_runtime_dir = ensure_xdg_runtime_dir();
            debug!(
                slice = %spawner.slice,
                memory_max = ?spawner.memory_max,
                "workload isolation: systemd user scopes enabled"
            );
        }
        Err(reason) => {
            warn!(
                reason = %reason,
                "workload isolation: systemd scopes unavailable, using process groups only"
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
        std::path::Path::new(&dir).join("bus").exists().then_some(dir)
    }
}

/// Verify `systemd-run --user --scope` works from this process. Done once;
/// the result is cached by [`workload_spawner`].
#[cfg(target_os = "linux")]
async fn probe_systemd_scopes() -> Result<(), String> {
    let mut probe = Command::new("systemd-run");
    probe.args(["--user", "--scope", "--quiet", "--collect", "--", "/usr/bin/true"]);
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
        Err(format!("probe scope exited with {status}"))
    }
}

#[cfg(not(target_os = "linux"))]
async fn probe_systemd_scopes() -> Result<(), String> {
    Err("systemd scopes are Linux-only".to_string())
}

/// Reaps an entire workload process tree.
///
/// Created from a freshly spawned child (the process-group leader, thanks to
/// [`WorkloadSpawner::command`]). [`terminate`](Self::terminate) gives the
/// group a SIGTERM grace period before SIGKILL; `Drop` SIGKILLs the group
/// immediately, which is what makes outer timeouts, scheduler aborts, and
/// `JoinSet` drops reap the tree without any cooperation from the caller.
/// Call [`disarm`](Self::disarm) once the child has been reaped normally so a
/// recycled pgid is never signaled.
#[derive(Debug)]
pub struct ProcessTreeGuard {
    pgid: Option<i32>,
}

impl ProcessTreeGuard {
    /// Arm the guard for `child` (must have been spawned in its own process
    /// group, as [`WorkloadSpawner::command`] does).
    pub fn new(child: &tokio::process::Child) -> Self {
        #[cfg(unix)]
        {
            Self {
                pgid: child.id().map(|pid| pid as i32),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = child;
            Self { pgid: None }
        }
    }

    /// Stop signaling on drop (child already exited and was reaped).
    pub fn disarm(&mut self) {
        self.pgid = None;
    }

    #[cfg(unix)]
    fn signal_group(pgid: i32, signal: nix::sys::signal::Signal) {
        // Negative pid == process group. ESRCH (group already gone) is fine.
        let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(-pgid), signal);
    }

    /// SIGKILL the whole group immediately, for call sites where the child
    /// handle was already consumed (e.g. a dropped `wait_with_output`
    /// future, whose `kill_on_drop` reaps the direct child separately).
    pub fn kill_tree(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.take() {
            Self::signal_group(pgid, nix::sys::signal::Signal::SIGKILL);
        }
    }

    /// SIGTERM the tree, wait up to [`TERMINATE_GRACE`], then SIGKILL and reap.
    pub async fn terminate(&mut self, child: &mut tokio::process::Child) {
        let Some(pgid) = self.pgid.take() else {
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
                    Ok(Some(_)) => return,
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
    }
}

impl Drop for ProcessTreeGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.take() {
            Self::signal_group(pgid, nix::sys::signal::Signal::SIGKILL);
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
        assert!(remaining.is_empty(), "survivors after terminate: {remaining:?}");
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
        assert!(remaining.is_empty(), "survivors after guard drop: {remaining:?}");
    }

    #[tokio::test]
    async fn disarm_prevents_late_signal() {
        let spawner = WorkloadSpawner::direct();
        let mut child = spawner
            .command("true", &[])
            .spawn()
            .expect("spawn true");
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
}
