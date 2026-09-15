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
use std::time::Duration;

use lunarwing::sandbox::{SandboxConfig, SandboxManager, SandboxNetworkMode, SandboxPolicy};

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

    mgr.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
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
