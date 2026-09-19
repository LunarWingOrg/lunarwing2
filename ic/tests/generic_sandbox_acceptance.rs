//! Live WI#179 acceptance on explicit Podman and Docker lanes. Opt in with
//! LUNARWING_E2E_PODMAN_TESTS=1 and/or LUNARWING_E2E_DOCKER_TESTS=1.
//! Requires a locally loaded sandbox image with Python 3 and curl; override
//! LUNARWING_E2E_SANDBOX_IMAGE (default lunarwing-sandbox:latest).
//! Use a dedicated runtime account/store: the orphan test performs a full sweep.

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use lunarwing::sandbox::{
    CliRunner, SandboxConfig, SandboxManager, SandboxNetworkMode, SandboxPolicy, SandboxRuntime,
    inflight_sandbox_containers,
};
use lunarwing::tools::{Tool, builtin::ShellTool};

async fn cli(runtime: &str, args: &[&str]) -> std::process::Output {
    tokio::time::timeout(
        Duration::from_secs(20),
        tokio::process::Command::new(runtime)
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap()
}

async fn names(runtime: &str, workspace: &Path) -> Vec<String> {
    let output = cli(
        runtime,
        &[
            "ps",
            "-a",
            "--filter",
            "label=lunarwing.sandbox",
            "--format",
            "{{.Names}}",
        ],
    )
    .await;
    assert!(
        output.status.success(),
        "ps failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut names = Vec::new();
    for name in String::from_utf8_lossy(&output.stdout).lines() {
        let inspect = cli(runtime, &["inspect", name]).await;
        if !inspect.status.success() {
            continue;
        } // --rm may win this race.
        let value: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
        if value[0]["Mounts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["Source"].as_str() == workspace.to_str())
        {
            names.push(name.to_owned());
        }
    }
    names
}

async fn wait_names(runtime: &str, dir: &Path, present: bool) -> Vec<String> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let found = names(runtime, dir).await;
            if found.is_empty() != present {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("container presence reached expected state")
}

async fn wait_removed(runtime: &str, dir: &Path, containers: &[String]) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let inflight = inflight_sandbox_containers();
            if names(runtime, dir).await.is_empty()
                && containers.iter().all(|name| !inflight.contains(name))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("containers removed and in-flight registrations released");
}

fn config(runtime: SandboxRuntime, network: SandboxNetworkMode) -> SandboxConfig {
    SandboxConfig {
        runtime,
        network,
        policy: SandboxPolicy::WorkspaceWrite,
        timeout: Duration::from_secs(60),
        auto_pull_image: false,
        image: std::env::var("LUNARWING_E2E_SANDBOX_IMAGE")
            .unwrap_or_else(|_| "lunarwing-sandbox:latest".into()),
        ..SandboxConfig::default()
    }
}

async fn lifecycle(runtime: SandboxRuntime, bin: &str, dir: &Path) {
    let cfg = config(runtime, SandboxNetworkMode::None);
    let runner = CliRunner::detect(runtime, cfg.image.clone()).await.unwrap();
    let manager = Arc::new(SandboxManager::new(cfg));
    manager.initialize().await.unwrap();
    let m = manager.clone();
    let workspace = dir.to_owned();
    let execution =
        tokio::spawn(async move { m.execute("sleep 60", &workspace, HashMap::new()).await });
    let active = wait_names(bin, dir, true).await;
    let orphan = format!("lunarwing-sandbox-{}", uuid::Uuid::new_v4());
    let created = cli(
        bin,
        &[
            "create",
            "--name",
            &orphan,
            "--network",
            "none",
            "--label",
            "lunarwing.sandbox=2000-01-01T00:00:00Z",
            &manager.config().image,
        ],
    )
    .await;
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    runner.reap_orphans(Duration::ZERO).await; // No API socket involved.
    assert_eq!(
        names(bin, dir).await,
        active,
        "reaper must preserve live execution"
    );
    let orphans = cli(
        bin,
        &[
            "ps",
            "-a",
            "--filter",
            &format!("name={orphan}"),
            "--format",
            "{{.Names}}",
        ],
    )
    .await;
    assert!(orphans.status.success());
    assert!(
        orphans.stdout.is_empty(),
        "orphan must disappear, not just stop"
    );
    execution.abort();
    assert!(execution.await.unwrap_err().is_cancelled());
    wait_removed(bin, dir, &active).await;

    let shell = ShellTool::new()
        .with_sandbox(manager.clone())
        .with_working_dir(dir.to_owned());
    let timed = tokio::spawn(async move {
        shell
            .execute(
                serde_json::json!({"command":"sleep 60", "timeout":5}),
                &lunarwing::context::JobContext::default(),
            )
            .await
    });
    let active = wait_names(bin, dir, true).await;
    let error = timed.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("[sandbox:timeout]"), "{error}");
    wait_removed(bin, dir, &active).await;
    let result = manager.execute(
        "echo writable > /workspace/marker; id -u; grep -E '^(NoNewPrivs|Seccomp):' /proc/self/status; cat /sys/fs/cgroup/pids.max",
        dir, HashMap::new()).await.unwrap();
    assert_eq!(result.exit_code, 0, "{}", result.stderr);
    assert!(
        dir.join("marker").exists(),
        "non-1000-owned workspace must be writable"
    );
    assert!(
        result.stdout.contains("Seccomp:\t2"),
        "runtime must supply its default seccomp filter"
    );
    assert!(result.stdout.contains("NoNewPrivs:\t1"));
    assert!(result.stdout.lines().any(|line| line == "256"));
    assert_ne!(
        result.stdout.lines().next(),
        Some("0"),
        "never container root"
    );
    manager.shutdown().await;
}

async fn network(runtime: SandboxRuntime, dir: &Path) {
    let host = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = host.local_addr().unwrap().port();
    for mode in [SandboxNetworkMode::None, SandboxNetworkMode::ProxyEnforced] {
        let mut cfg = config(runtime, mode);
        cfg.network_allowlist = vec!["example.com".into()];
        let manager = SandboxManager::new(cfg);
        manager.initialize().await.unwrap();
        // All actual connect attempts use numeric addresses; missing DNS/tooling
        // cannot masquerade as successful network isolation.
        let command = format!(
            r#"python3 -I -c '
import socket
assert all(name == "lo" for _, name in socket.if_nameindex()), "external network interface present"
for host, port in [("127.0.0.1", {port}), ("::1", {port}), ("10.0.2.2", {port}), ("169.254.1.2", {port}), ("172.17.0.1", {port}), ("1.1.1.1", 80)]:
    try:
        socket.create_connection((host, port), timeout=1).close()
    except OSError:
        continue
    raise SystemExit("direct connection unexpectedly succeeded")
print("direct-blocked")'"#
        );
        let result = manager
            .execute(&command, dir, HashMap::new())
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0, "{}", result.stderr);
        assert!(result.stdout.contains("direct-blocked"));
        if mode == SandboxNetworkMode::ProxyEnforced {
            let allowed = manager
                .execute(
                    "curl --noproxy '' -fsS --max-time 15 http://example.com/",
                    dir,
                    HashMap::new(),
                )
                .await
                .unwrap();
            assert_eq!(
                allowed.exit_code, 0,
                "allowlisted HTTP failed: {}",
                allowed.stderr
            );
            assert!(allowed.stdout.contains("Example Domain"));
            let denied = manager
                .execute(
                    "curl --noproxy '' -sS --max-time 5 http://denied.invalid/",
                    dir,
                    HashMap::new(),
                )
                .await
                .unwrap();
            assert!(denied.stdout.contains("sandbox-proxy denied:"));
        }
        manager.shutdown().await;
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(100), host.accept())
            .await
            .is_err(),
        "host-loopback listener must receive no connections"
    );
}

#[tokio::test]
async fn generic_sandbox_lifecycle_and_network_acceptance() {
    let mut ran = false;
    for (flag, runtime, bin) in [
        (
            "LUNARWING_E2E_PODMAN_TESTS",
            SandboxRuntime::Podman,
            "podman",
        ),
        (
            "LUNARWING_E2E_DOCKER_TESTS",
            SandboxRuntime::Docker,
            "docker",
        ),
    ] {
        if std::env::var(flag).as_deref() != Ok("1") {
            continue;
        }
        ran = true;
        let existing = cli(bin, &["ps", "-aq", "--filter", "label=lunarwing.sandbox"]).await;
        assert!(existing.status.success(), "runtime ps preflight failed");
        assert!(
            existing.stdout.is_empty(),
            "acceptance needs an isolated runtime store without existing sandbox containers"
        );
        let dir = tempfile::tempdir().unwrap();
        // Rootful Docker test lane explicitly exercises a non-1000 owner.
        if nix::unistd::geteuid().is_root() {
            nix::unistd::chown(
                dir.path(),
                Some(nix::unistd::Uid::from_raw(23456)),
                Some(nix::unistd::Gid::from_raw(23456)),
            )
            .unwrap();
        }
        if runtime == SandboxRuntime::Docker {
            use std::os::unix::fs::MetadataExt;
            assert_ne!(
                dir.path().metadata().unwrap().uid(),
                1000,
                "run Docker acceptance as root or a non-1000 workspace owner"
            );
        }
        lifecycle(runtime, bin, dir.path()).await;
        network(runtime, dir.path()).await;
    }
    if !ran {
        eprintln!("SKIP live acceptance: enable a Podman/Docker E2E lane");
    }
}
