//! End-to-end tests for the generic podman sandbox (CLI runner).
//!
//! Gated behind `LUNARWING_E2E_PODMAN_TESTS=1` (mirroring the reaper's
//! `LUNARWING_E2E_DOCKER_TESTS` gate) because they create and kill real
//! containers. The image defaults to `quay.io/libpod/alpine:latest`
//! (override with `LUNARWING_E2E_PODMAN_IMAGE`); it is auto-pulled.
//!
//! Run with:
//! ```text
//! LUNARWING_E2E_PODMAN_TESTS=1 \
//!   cargo test --test podman_sandbox_integration -- --nocapture
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lunarwing::context::JobContext;
use lunarwing::orchestrator::{ReaperConfig, sweep_cli_sandboxes_once};
use lunarwing::sandbox::{
    SandboxConfig, SandboxManager, SandboxNetworkMode, SandboxPolicy,
    cli_runner::inflight_sandbox_containers,
};
use lunarwing::tools::Tool;
use lunarwing::tools::builtin::ShellTool;

fn gate_open() -> bool {
    if std::env::var("LUNARWING_E2E_PODMAN_TESTS")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    {
        true
    } else {
        eprintln!("skipping: set LUNARWING_E2E_PODMAN_TESTS=1 to run podman sandbox e2e tests");
        false
    }
}

fn test_image() -> String {
    std::env::var("LUNARWING_E2E_PODMAN_IMAGE")
        .unwrap_or_else(|_| "quay.io/libpod/alpine:latest".to_string())
}

/// Runtime CLI binary for direct container assertions (podman on this host).
fn runtime_bin() -> &'static str {
    std::env::var("LUNARWING_E2E_PODMAN_RUNTIME")
        .ok()
        .filter(|v| v == "docker")
        .map(|_| "docker")
        .unwrap_or("podman")
}

/// Whether a named container currently exists (any state).
async fn container_exists(name: &str) -> bool {
    tokio::process::Command::new(runtime_bin())
        .args(["container", "exists", name])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}

/// Create a labeled sandbox container directly via the CLI (for reaper tests).
async fn create_labeled_container(name: &str, created_at: &str) -> bool {
    tokio::process::Command::new(runtime_bin())
        .args([
            "run",
            "-d",
            "--name",
            name,
            "--label",
            &format!("lunarwing.sandbox={created_at}"),
            "--entrypoint",
            "sleep",
            test_image().as_str(),
            "300",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}

fn scratch_dir(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("lunarwing-sbx-e2e-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn manager(
    policy: SandboxPolicy,
    network: SandboxNetworkMode,
    timeout: Duration,
) -> SandboxManager {
    SandboxManager::new(SandboxConfig {
        image: test_image(),
        policy,
        network,
        timeout,
        auto_pull_image: true,
        ..SandboxConfig::default()
    })
}

#[tokio::test]
async fn podman_echo_executes_in_container() {
    if !gate_open() {
        return;
    }
    let dir = scratch_dir("echo");
    let mgr = manager(
        SandboxPolicy::WorkspaceWrite,
        SandboxNetworkMode::None,
        Duration::from_secs(120),
    );
    mgr.initialize().await.expect("initialize");

    let out = mgr
        .execute("echo hello-from-sandbox", &dir, HashMap::new())
        .await
        .expect("execute echo");

    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stdout.trim().contains("hello-from-sandbox"),
        "stdout: {}",
        out.stdout
    );

    mgr.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn podman_shell_reports_output_truncation() {
    if !gate_open() {
        return;
    }
    let dir = scratch_dir("truncation");
    let mgr = Arc::new(manager(
        SandboxPolicy::ReadOnly,
        SandboxNetworkMode::None,
        Duration::from_secs(60),
    ));
    mgr.initialize().await.expect("initialize");
    let tool = ShellTool::new()
        .with_sandbox(mgr.clone())
        .with_working_dir(dir.clone());

    for (bytes, redirect, exit_code, truncated) in [
        (32768, "", 0, false),
        (40000, "", 0, true),
        (32768, ">&2", 7, false),
        (40000, ">&2", 7, true),
    ] {
        let command =
            format!("head -c {bytes} /dev/zero | tr '\\000' A {redirect}; exit {exit_code}");
        let result = tool
            .execute(
                serde_json::json!({"command": command}),
                &JobContext::default(),
            )
            .await
            .expect("execute shell")
            .result;
        assert_eq!(result["exit_code"], exit_code);
        assert_eq!(result["success"], exit_code == 0);
        assert_eq!(result["truncated"], truncated);
        let output = result["output"].as_str().expect("output");
        if truncated {
            assert!(output.starts_with("[output truncated]"));
            assert!(result["warning"].as_str().unwrap().contains("incomplete"));
        } else {
            assert_eq!(output, "A".repeat(bytes));
            assert!(result["warning"].is_null());
        }
    }

    mgr.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn podman_timeout_kills_container() {
    if !gate_open() {
        return;
    }
    let dir = scratch_dir("timeout");
    let mgr = manager(
        SandboxPolicy::WorkspaceWrite,
        SandboxNetworkMode::None,
        Duration::from_secs(5),
    );
    mgr.initialize().await.expect("initialize");

    let err = mgr
        .execute("sleep 30", &dir, HashMap::new())
        .await
        .expect_err("sleep 30 must time out");
    assert_eq!(err.kind(), "timeout", "error: {err}");

    // WI#179 concern 6: the timeout must actually remove the container, not
    // just report an error kind. `--rm` + kill-by-name means no labeled
    // container may remain shortly after the kill.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if list_sandbox_containers().await.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "container left behind after timeout: {:?}",
            list_sandbox_containers().await
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    mgr.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn podman_outer_tool_timeout_cleans_up_container() {
    if !gate_open() {
        return;
    }
    let dir = scratch_dir("outer-timeout");
    let mgr = Arc::new(manager(
        SandboxPolicy::WorkspaceWrite,
        SandboxNetworkMode::None,
        Duration::from_secs(120), // manager timeout far longer than the tool's
    ));
    mgr.initialize().await.expect("initialize");
    let tool = ShellTool::new()
        .with_sandbox(mgr.clone())
        .with_working_dir(dir.clone())
        .with_timeout(Duration::from_secs(3));

    // WI#179 concern 1 repro: the shell tool's outer timeout expires before
    // the manager's configured timeout. The dropped future must still kill
    // the container and clear the in-flight registry.
    let result = tool
        .execute(
            serde_json::json!({"command": "sleep 60"}),
            &JobContext::default(),
        )
        .await;
    let err = result.expect_err("sleep 60 must hit the outer tool timeout");
    match err {
        lunarwing::tools::ToolError::Sandbox(msg) => assert!(
            msg.contains("[sandbox:timeout]"),
            "outer timeout must surface the sandbox timeout envelope: {msg}"
        ),
        other => panic!("expected ToolError::Sandbox, got {other:?}"),
    }

    // In-flight registry must be empty again (no leaked registration).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !inflight_sandbox_containers().is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        inflight_sandbox_containers().is_empty(),
        "in-flight registry leaked: {:?}",
        inflight_sandbox_containers()
    );

    // And no sandbox container may remain (kill + rm -f on cancellation).
    // Cleanup runs in a detached task, so poll briefly instead of asserting
    // synchronously.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let leftovers = list_sandbox_containers().await;
        if leftovers.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "cancelled execution leaked containers: {leftovers:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    mgr.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn podman_reaper_sweep_skips_inflight_and_reaps_orphans() {
    if !gate_open() {
        return;
    }
    let dir = scratch_dir("reaper");
    let mgr = manager(
        SandboxPolicy::WorkspaceWrite,
        SandboxNetworkMode::None,
        Duration::from_secs(120),
    );
    mgr.initialize().await.expect("initialize");

    // An old orphan: labeled container created 2h ago, not in-flight.
    let orphan = format!("lunarwing-sandbox-orphan-{}", uuid::Uuid::new_v4());
    let old = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
    assert!(
        create_labeled_container(&orphan, &old).await,
        "create orphan fixture"
    );

    // A young container (created now) must survive the sweep.
    let young = format!("lunarwing-sandbox-young-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();
    assert!(
        create_labeled_container(&young, &now).await,
        "create young fixture"
    );

    let config = ReaperConfig {
        scan_interval: Duration::from_secs(300),
        orphan_threshold: Duration::from_secs(600),
        ..ReaperConfig::default()
    };
    sweep_cli_sandboxes_once(&config).await;

    assert!(
        !container_exists(&orphan).await,
        "old orphan must be reaped"
    );
    assert!(
        container_exists(&young).await,
        "young container must survive the sweep"
    );

    // Clean up the young fixture.
    let _ = tokio::process::Command::new(runtime_bin())
        .args(["rm", "-f", &young])
        .status()
        .await;

    mgr.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// List `lunarwing.sandbox`-labeled containers via the runtime CLI.
async fn list_sandbox_containers() -> Vec<String> {
    let output = tokio::process::Command::new(runtime_bin())
        .args([
            "ps",
            "-a",
            "--filter",
            "label=lunarwing.sandbox",
            "--format",
            "{{.Names}}",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .await;
    match output {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

#[tokio::test]
async fn podman_network_none_blocks_egress() {
    if !gate_open() {
        return;
    }
    let dir = scratch_dir("nonet");
    let mgr = manager(
        SandboxPolicy::WorkspaceWrite,
        SandboxNetworkMode::None,
        Duration::from_secs(60),
    );
    mgr.initialize().await.expect("initialize");

    // With --network none there is no DNS and no route: any fetch must fail.
    let out = mgr
        .execute(
            "wget -T 2 -t 1 -q -O /dev/null http://example.com; echo exit=$?",
            &dir,
            HashMap::new(),
        )
        .await
        .expect("execute wget");

    assert!(
        out.stdout.contains("exit=1") || out.stdout.contains("exit=2") || out.exit_code != 0,
        "egress should fail under --network none; stdout: {} stderr: {}",
        out.stdout,
        out.stderr
    );

    mgr.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn podman_readonly_blocks_workspace_writes() {
    if !gate_open() {
        return;
    }
    let dir = scratch_dir("ro");
    let mgr = manager(
        SandboxPolicy::ReadOnly,
        SandboxNetworkMode::None,
        Duration::from_secs(60),
    );
    mgr.initialize().await.expect("initialize");

    let out = mgr
        .execute("touch /workspace/should-fail", &dir, HashMap::new())
        .await
        .expect("execute touch");

    assert_ne!(out.exit_code, 0, "write must fail on a ro mount");
    assert!(
        !dir.join("should-fail").exists(),
        "host scratch dir must stay untouched"
    );

    mgr.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn podman_workspace_write_allows_writes() {
    if !gate_open() {
        return;
    }
    let dir = scratch_dir("rw");
    let mgr = manager(
        SandboxPolicy::WorkspaceWrite,
        SandboxNetworkMode::None,
        Duration::from_secs(60),
    );
    mgr.initialize().await.expect("initialize");

    let out = mgr
        .execute(
            "echo sandbox-was-here > /workspace/marker.txt",
            &dir,
            HashMap::new(),
        )
        .await
        .expect("execute write");

    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
    let marker = dir.join("marker.txt");
    assert!(marker.exists(), "marker must be visible on the host mount");
    let content = std::fs::read_to_string(&marker).unwrap_or_default();
    assert_eq!(content.trim(), "sandbox-was-here");

    mgr.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn podman_proxy_mode_sets_proxy_env() {
    if !gate_open() {
        return;
    }
    let dir = scratch_dir("proxy");
    let mgr = manager(
        SandboxPolicy::ReadOnly,
        SandboxNetworkMode::Proxy,
        Duration::from_secs(60),
    );
    mgr.initialize().await.expect("initialize");
    assert!(mgr.proxy_port().await.is_some(), "proxy should be running");

    // The proxy env var must be visible inside the container, pointing at the
    // runtime's host alias (no real egress needed for this assertion).
    let out = mgr
        .execute("echo $http_proxy", &dir, HashMap::new())
        .await
        .expect("execute env echo");

    let stdout = out.stdout.trim();
    assert!(
        stdout.contains("host.containers.internal") || stdout.contains("host.docker.internal"),
        "http_proxy not wired into the container: {stdout:?}"
    );

    mgr.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn podman_proxy_mode_enforces_allowlist() {
    if !gate_open() {
        return;
    }
    let dir = scratch_dir("proxy-deny");
    let mgr = manager(
        SandboxPolicy::ReadOnly,
        SandboxNetworkMode::Proxy,
        Duration::from_secs(60),
    );
    mgr.initialize().await.expect("initialize");
    assert!(mgr.proxy_port().await.is_some(), "proxy should be running");

    // WI#179 concern 6: the proxy test must prove connectivity + enforcement,
    // not just the env var. A non-allowlisted domain fetched THROUGH the
    // proxy must come back with the stable deny body — which also proves the
    // container reached the proxy (host alias + port wiring work).
    //
    // Connectivity requires the runtime to map its host alias to the host's
    // loopback (pasta/slirp4netns). Some hosts (e.g. netavark-only dev VMs)
    // cannot do this, so the live fetch is gated behind an explicit opt-in;
    // the env wiring assertion above still runs everywhere.
    if std::env::var("LUNARWING_E2E_PROXY_CONNECTIVITY")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    {
        let out = mgr
            .execute(
                "wget -T 5 -t 1 -q -O - http://evil-not-allowlisted.invalid/ 2>&1; echo exit=$?",
                &dir,
                HashMap::new(),
            )
            .await
            .expect("execute wget via proxy");

        assert!(
            out.stdout.contains("sandbox-proxy denied:"),
            "proxy must deny non-allowlisted domains with the stable prefix; stdout: {} stderr: {}",
            out.stdout,
            out.stderr
        );
    } else {
        eprintln!(
            "skipping live proxy fetch: set LUNARWING_E2E_PROXY_CONNECTIVITY=1 on a host whose \
             runtime maps the host alias to host loopback (pasta/slirp4netns)"
        );
    }

    mgr.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}
