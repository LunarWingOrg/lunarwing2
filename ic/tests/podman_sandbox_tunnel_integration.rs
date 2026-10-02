//! End-to-end tests for the sandbox tunnel sidecar (WI#248).
//!
//! Double-gated: requires both `LUNARWING_E2E_PODMAN_TESTS=1` (real
//! containers) and `LUNARWING_E2E_TUNNEL_TESTS=1` (kernel WireGuard support
//! on the host — the module must be loaded; rootless creation needs no host
//! privileges because the container's user namespace owns its network
//! namespace).
//!
//! The wireguard lane exercises the real sidecar lifecycle with a real
//! (peerless) kernel WireGuard interface: `wg-quick up` inside the sidecar
//! brings `wg0` up, the health gate waits for it, and a sandbox run joined
//! via `--network container:` can see it. No endpoint or peer is needed —
//! the test validates the namespace plumbing, not tunnel crypto.
//!
//! The sidecar image must contain `wg-quick`, `ip` (iproute2), `/bin/sh`,
//! and `sleep`. When `LUNARWING_E2E_TUNNEL_IMAGE` is unset, the test builds
//! `localhost/lw-tunnel-e2e:latest` from the alpine image used by the other
//! podman e2e suites (requires registry access, like every gated lane).
//!
//! Run with:
//! ```text
//! LUNARWING_E2E_PODMAN_TESTS=1 LUNARWING_E2E_TUNNEL_TESTS=1 \
//!   cargo test --test podman_sandbox_tunnel_integration -- --nocapture
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use lunarwing::sandbox::SandboxNetworkMode;
use lunarwing::sandbox::SandboxPolicy;
use lunarwing::sandbox::cli_runner::{CliRunner, ExecParams};
use lunarwing::sandbox::config::ResourceLimits;
use lunarwing::sandbox::{TunnelProvider, TunnelSidecar, TunnelSidecarConfig};

fn podman_gate_open() -> bool {
    if std::env::var("LUNARWING_E2E_PODMAN_TESTS")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    {
        true
    } else {
        eprintln!("skipping: set LUNARWING_E2E_PODMAN_TESTS=1 to run podman sandbox e2e tests");
        false
    }
}

fn tunnel_gate_open() -> bool {
    if std::env::var("LUNARWING_E2E_TUNNEL_TESTS")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    {
        true
    } else {
        eprintln!("skipping: set LUNARWING_E2E_TUNNEL_TESTS=1 to run sandbox tunnel e2e tests");
        false
    }
}

const E2E_IMAGE: &str = "localhost/lw-tunnel-e2e:latest";

/// The sidecar image (`wg-quick` + iproute2 + sh + sleep). Built on demand
/// from alpine unless `LUNARWING_E2E_TUNNEL_IMAGE` overrides it.
async fn tunnel_image() -> Option<String> {
    if let Ok(image) = std::env::var("LUNARWING_E2E_TUNNEL_IMAGE") {
        return Some(image);
    }
    let built = tokio::process::Command::new("podman")
        .args(["image", "exists", E2E_IMAGE])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success());
    if built || build_tunnel_image().await {
        Some(E2E_IMAGE.to_string())
    } else {
        None
    }
}

async fn build_tunnel_image() -> bool {
    let dir = std::env::temp_dir().join(format!("lw-tunnel-e2e-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let containerfile = dir.join("Containerfile");
    let alpine = std::env::var("LUNARWING_E2E_PODMAN_IMAGE")
        .unwrap_or_else(|_| "quay.io/libpod/alpine:latest".to_string());
    let ok = std::fs::write(
        &containerfile,
        format!("FROM {alpine}\nRUN apk add --no-cache iproute2 wireguard-tools\n"),
    )
    .is_ok();
    if !ok {
        return false;
    }
    let status = tokio::process::Command::new("podman")
        .args(["build", "-t", E2E_IMAGE, dir.to_string_lossy().as_ref()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;
    let _ = std::fs::remove_dir_all(&dir);
    status.is_ok_and(|s| s.success())
}

/// Generate a WireGuard private key inside the e2e image (host may not ship
/// wireguard-tools).
async fn generate_wg_private_key(image: &str) -> Option<String> {
    let output = tokio::process::Command::new("podman")
        .args(["run", "--rm", image, "wg", "genkey"])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let key = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!key.is_empty()).then_some(key)
}

/// WireGuard lane: sidecar lifecycle (start, health gate, idempotent reuse)
/// plus a joined sandbox run observing `wg0` in the shared namespace.
#[tokio::test]
async fn podman_tunnel_sidecar_wireguard_lifecycle_and_join() {
    if !podman_gate_open() || !tunnel_gate_open() {
        return;
    }
    let Some(image) = tunnel_image().await else {
        panic!(
            "no tunnel e2e image available: set LUNARWING_E2E_TUNNEL_IMAGE or allow building \
             {E2E_IMAGE} (needs registry access)"
        );
    };
    let Some(private_key) = generate_wg_private_key(&image).await else {
        panic!("could not generate a wireguard private key with {image} (needs `wg`)");
    };

    let workdir = std::env::temp_dir().join(format!("lw-tunnel-join-{}", std::process::id()));
    std::fs::create_dir_all(&workdir).expect("workdir");
    let conf = workdir.join("wg0.conf");
    std::fs::write(
        &conf,
        format!("[Interface]\nPrivateKey = {private_key}\nAddress = 10.231.99.1/32\n"),
    )
    .expect("write conf");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&conf, std::fs::Permissions::from_mode(0o600)).expect("chmod 600");
    }

    let sidecar = TunnelSidecar::new(
        lunarwing::sandbox::ContainerRuntime::Podman,
        TunnelSidecarConfig {
            provider: TunnelProvider::Wireguard,
            image: Some(image.clone()),
            wireguard_config: Some(conf),
            startup_timeout: Duration::from_secs(60),
            ..TunnelSidecarConfig::default()
        },
    );

    // Health-gated start; fail-closed contract is asserted by construction
    // (an error here fails the test rather than degrading).
    sidecar
        .ensure_started()
        .await
        .expect("sidecar starts healthy");

    // Idempotent: a second start reuses the running, hash-matching sidecar.
    sidecar.ensure_started().await.expect("reuse path works");

    // A sandbox run joins the sidecar namespace and observes wg0.
    let runner = CliRunner::detect(lunarwing::sandbox::SandboxRuntime::Podman, image.clone())
        .await
        .expect("detect runtime");
    static LIMITS: std::sync::LazyLock<ResourceLimits> =
        std::sync::LazyLock::new(|| ResourceLimits {
            timeout: Duration::from_secs(120),
            ..ResourceLimits::default()
        });
    static EMPTY_ENV: std::sync::LazyLock<HashMap<String, String>> =
        std::sync::LazyLock::new(HashMap::new);
    static NO_PROTECTED: std::sync::LazyLock<Vec<PathBuf>> = std::sync::LazyLock::new(Vec::new);

    let output = runner
        .execute(ExecParams {
            command: "ip -o link show wg0 >/dev/null && echo LUNARWING-TUNNEL-JOIN-OK",
            working_dir: &workdir,
            policy: SandboxPolicy::ReadOnly,
            limits: &LIMITS,
            env: &EMPTY_ENV,
            network: SandboxNetworkMode::Tunnel,
            proxy_port: 0,
            protected_paths: &NO_PROTECTED,
        })
        .await
        .expect("joined sandbox run");

    assert_eq!(
        output.exit_code, 0,
        "joined run failed: {}{}",
        output.stdout, output.stderr
    );
    assert!(
        output.stdout.contains("LUNARWING-TUNNEL-JOIN-OK"),
        "wg0 not visible in joined namespace: {}{}",
        output.stdout,
        output.stderr
    );

    sidecar.stop().await;
    let _ = std::fs::remove_dir_all(&workdir);
}
