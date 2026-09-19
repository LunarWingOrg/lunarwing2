//! Live acceptance for the generic sandbox on explicit Podman and Docker lanes
//! (combined !1046 + !1044 single implementation).
//!
//! Opt in with `LUNARWING_E2E_PODMAN_TESTS=1` and/or `LUNARWING_E2E_DOCKER_TESTS=1`.
//! Requires a locally loaded sandbox image with Python 3 and curl; override
//! with `LUNARWING_E2E_SANDBOX_IMAGE` (default `lunarwing-sandbox:latest`).
//! Allowlist-connectivity (example.com) requires outbound network access;
//! isolation and relay-transport checks work fully offline.
//!
//! Use a dedicated runtime store: sweep assertions look at every
//! `lunarwing.sandbox`-labeled container of the runtime.

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use lunarwing::orchestrator::{ReaperConfig, sweep_cli_sandboxes_once};
use lunarwing::sandbox::{
    SandboxConfig, SandboxManager, SandboxNetworkMode, SandboxPolicy, SandboxRuntime,
};

const IMAGE: &str = "lunarwing-sandbox:latest";

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

async fn sandbox_names(runtime: &str) -> Vec<String> {
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
    output
        .stdout
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| String::from_utf8_lossy(line).into_owned())
        .collect()
}

fn config(runtime: SandboxRuntime, network: SandboxNetworkMode) -> SandboxConfig {
    SandboxConfig {
        runtime,
        network,
        policy: SandboxPolicy::WorkspaceWrite,
        timeout: Duration::from_secs(60),
        auto_pull_image: false,
        image: std::env::var("LUNARWING_E2E_SANDBOX_IMAGE").unwrap_or_else(|_| IMAGE.to_string()),
        ..SandboxConfig::default()
    }
}

/// Every accepted runtime lane must advertise a hardening contract: writable
/// non-root workspace, pinned pids cap, and pids.max readback.
async fn hardening_contract(runtime: SandboxRuntime, bin: &'static str, dir: &Path) {
    let manager = SandboxManager::new(config(runtime, SandboxNetworkMode::None));
    manager.initialize().await.expect("initialize must succeed");

    let out = manager
        .execute(
            "echo writable > /workspace/marker; id -u; cat /sys/fs/cgroup/pids.max 2>/dev/null || cat /sys/fs/cgroup/pids/pids.max",
            dir,
            HashMap::new(),
        )
        .await
        .expect("execute hardening probe");
    assert_eq!(out.exit_code, 0, "hardening probe failed: {}", out.stderr);
    assert!(
        dir.join("marker").exists(),
        "workspace mount must be writable"
    );
    assert_ne!(
        out.stdout.lines().next().map(str::trim),
        Some("0"),
        "container must never run as root: {}",
        out.stdout
    );
    assert!(
        out.stdout.lines().any(|line| line.trim() == "256"),
        "explicit --pids-limit must apply: {}",
        out.stdout
    );

    // Cleanup + socketless reaper sweep: even an intentionally orphaned
    // container (label forged with an old timestamp) disappears without any
    // Docker-compatible API while a live one survives.
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
            "sh",
            "-c",
            "sleep 300",
        ],
    )
    .await;
    assert!(
        created.status.success(),
        "orphan creation failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let cfg = ReaperConfig {
        scan_interval: Duration::from_secs(1),
        orphan_threshold: Duration::from_secs(0),
        ..ReaperConfig::default()
    };
    sweep_cli_sandboxes_once(&cfg).await;
    assert!(
        !sandbox_names(bin).await.iter().any(|n| n == &orphan),
        "socketless reaper must remove the forged orphan"
    );

    manager.shutdown().await;
}

/// `none` mode isolation plus proxy-enforced egress acceptance (WI#155):
/// direct connections never work anywhere; enforced mode reaches only the
/// allowlist proxy through the Unix relay.
async fn network_acceptance(runtime: SandboxRuntime, dir: &Path) {
    let host = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let port = host.local_addr().unwrap().port();

    for mode in [SandboxNetworkMode::None, SandboxNetworkMode::ProxyEnforced] {
        let mut cfg = config(runtime, mode);
        cfg.network_allowlist = vec!["example.com".into()];
        let manager = SandboxManager::new(cfg);
        manager.initialize().await.expect("initialize must succeed");

        // Every actual connect attempt uses numeric details — a missing DNS
        // resolver or missing tooling cannot masquerade as isolation.
        let command = format!(
            r#"python3 -I -c '
import socket
names = [name for _, name in socket.if_nameindex()]
assert names == ["lo"], f"external network interface present: {{names}}"
for host, port in [("127.0.0.1", {port}), ("::1", {port}), ("10.0.2.2", {port}), ("169.254.1.2", {port}), ("172.17.0.1", {port}), ("1.1.1.1", 80)]:
    try:
        socket.create_connection((host, port), timeout=1).close()
    except OSError:
        continue
    raise SystemExit(f"direct connection unexpectedly succeeded: {{host}}:{{port}}")
print("direct-blocked")'"#
        );
        let out = manager
            .execute(&command, dir, HashMap::new())
            .await
            .expect("isolation probe executes");
        assert_eq!(out.exit_code, 0, "isolation probe failed: {}", out.stderr);
        assert!(
            out.stdout.contains("direct-blocked"),
            "stdout: {}",
            out.stdout
        );

        if mode == SandboxNetworkMode::ProxyEnforced {
            // Denied target: comes back through the relay with the stable
            // deny prefix — proves the relay + proxy chain works offline.
            let denied = manager
                .execute(
                    "curl --noproxy '' -sS --max-time 10 http://denied.invalid/",
                    dir,
                    HashMap::new(),
                )
                .await
                .expect("denied fetch executes");
            assert!(
                denied.stdout.contains("sandbox-proxy denied:"),
                "deny body must come back through the relay: stdout={} stderr={}",
                denied.stdout,
                denied.stderr
            );

            if std::env::var("LUNARWING_E2E_PROXY_CONNECTIVITY")
                .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            {
                let allowed = manager
                    .execute(
                        "curl --noproxy '' -fsS --max-time 15 http://example.com/",
                        dir,
                        HashMap::new(),
                    )
                    .await
                    .expect("allowlisted fetch executes");
                assert_eq!(
                    allowed.exit_code, 0,
                    "allowlisted HTTP failed: {}",
                    allowed.stderr
                );
                assert!(
                    allowed.stdout.contains("Example Domain"),
                    "unexpected payload: {}",
                    allowed.stdout
                );
            } else {
                eprintln!(
                    "skipping live allowlist fetch: set LUNARWING_E2E_PROXY_CONNECTIVITY=1 on a host with internet access"
                );
            }
        }
    }

    assert!(
        tokio::time::timeout(Duration::from_millis(100), host.accept())
            .await
            .is_err(),
        "host-loopback listener must receive no connections"
    );
}

fn bin(runtime: SandboxRuntime) -> &'static str {
    match runtime {
        SandboxRuntime::Podman => "podman",
        SandboxRuntime::Docker => "docker",
        SandboxRuntime::Auto => unreachable!("acceptance lanes pick a concrete runtime"),
    }
}

#[tokio::test]
async fn generic_sandbox_hardening_and_egress_acceptance() {
    let mut ran = false;
    for (flag, runtime) in [
        ("LUNARWING_E2E_PODMAN_TESTS", SandboxRuntime::Podman),
        ("LUNARWING_E2E_DOCKER_TESTS", SandboxRuntime::Docker),
    ] {
        if std::env::var(flag).as_deref() != Ok("1")
            && !std::env::var(flag).is_ok_and(|v| v.eq_ignore_ascii_case("true"))
        {
            continue;
        }
        ran = true;
        let existing = cli(
            bin(runtime),
            &["ps", "-aq", "--filter", "label=lunarwing.sandbox"],
        )
        .await;
        assert!(
            existing.status.success() && existing.stdout.is_empty(),
            "acceptance needs an isolated runtime store without existing sandbox containers"
        );
        let dir = tempfile::tempdir().expect("scratch dir");
        hardening_contract(runtime, bin(runtime), dir.path()).await;
        network_acceptance(runtime, dir.path()).await;
    }
    if !ran {
        eprintln!("SKIP live acceptance: enable a Podman/Docker E2E lane");
    }
}
