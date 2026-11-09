//! Real-Podman tunnel sidecar coverage (WI#248).
//!
//! Double-gated by LUNARWING_E2E_PODMAN_TESTS=1 and
//! LUNARWING_E2E_TUNNEL_TESTS=1. The peerless WireGuard lane validates the
//! netns lifecycle and egress-only firewall without requiring a live overlay.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lunarwing::sandbox::cli_runner::{CliRunner, ExecParams};
use lunarwing::sandbox::config::ResourceLimits;
use lunarwing::sandbox::{
    ContainerRuntime, SandboxNetworkMode, SandboxPolicy, SandboxRuntime, TunnelProvider,
    TunnelSidecar, TunnelSidecarConfig,
};

const E2E_IMAGE: &str = "localhost/lw-tunnel-e2e:latest";

fn gate_open(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

async fn tunnel_image() -> Option<String> {
    if let Ok(image) = std::env::var("LUNARWING_E2E_TUNNEL_IMAGE") {
        return Some(image);
    }
    let exists = tokio::process::Command::new("podman")
        .args(["image", "exists", E2E_IMAGE])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success());
    if exists || build_tunnel_image().await {
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
    let written = std::fs::write(
        &containerfile,
        format!("FROM {alpine}\nRUN apk add --no-cache bash iproute2 nftables wireguard-tools\n"),
    )
    .is_ok();
    if !written {
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
    status.is_ok_and(|status| status.success())
}

async fn generate_wg_private_key(image: &str) -> Option<String> {
    let output = tokio::process::Command::new("podman")
        .args(["run", "--rm", image, "wg", "genkey"])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    let key = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !key.is_empty()).then_some(key)
}

async fn tunnel_container_id() -> Option<String> {
    let output = tokio::process::Command::new("podman")
        .args([
            "inspect",
            "--format",
            "{{.Id}}",
            lunarwing::sandbox::SANDBOX_TUNNEL_CONTAINER,
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !id.is_empty()).then_some(id)
}

async fn assert_firewall_listing() {
    let output = tokio::process::Command::new("podman")
        .args([
            "exec",
            lunarwing::sandbox::SANDBOX_TUNNEL_CONTAINER,
            "nft",
            "list",
            "chain",
            "inet",
            "lunarwing_sandbox_egress",
            "input",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .expect("nft listing");
    assert!(output.status.success());
    let listing = String::from_utf8_lossy(&output.stdout);
    assert!(listing.contains("policy drop"), "{listing}");
    assert!(
        listing.contains("ct state established,related accept"),
        "{listing}"
    );
}

async fn exercise_sidecar(
    sidecar: &TunnelSidecar,
    image: &str,
    workdir: &Path,
) -> Result<(), String> {
    sidecar
        .ensure_started()
        .await
        .map_err(|error| error.to_string())?;
    let first_id = tunnel_container_id()
        .await
        .ok_or("missing first container id")?;
    sidecar
        .ensure_started()
        .await
        .map_err(|error| error.to_string())?;
    let reused_id = tunnel_container_id()
        .await
        .ok_or("missing reused container id")?;
    assert_eq!(first_id, reused_id, "healthy sidecar must be reused");
    sidecar
        .ensure_healthy()
        .await
        .map_err(|error| error.to_string())?;
    assert_firewall_listing().await;

    let runner = CliRunner::detect(SandboxRuntime::Podman, image.to_string())
        .await
        .map_err(|error| error.to_string())?;
    let limits = ResourceLimits {
        timeout: Duration::from_secs(120),
        ..ResourceLimits::default()
    };
    let env = HashMap::new();
    let protected = Vec::<PathBuf>::new();
    let output = runner
        .execute(ExecParams {
            command: "ip -o link show wg0 >/dev/null && echo LUNARWING-TUNNEL-JOIN-OK",
            working_dir: workdir,
            policy: SandboxPolicy::ReadOnly,
            limits: &limits,
            env: &env,
            network: SandboxNetworkMode::Tunnel,
            proxy_port: 0,
            protected_paths: &protected,
            tenant_logs: false,
        })
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(output.exit_code, 0, "{}{}", output.stdout, output.stderr);
    assert!(output.stdout.contains("LUNARWING-TUNNEL-JOIN-OK"));

    let tampered = tokio::process::Command::new("podman")
        .args([
            "exec",
            lunarwing::sandbox::SANDBOX_TUNNEL_CONTAINER,
            "nft",
            "add",
            "rule",
            "inet",
            "lunarwing_sandbox_egress",
            "input",
            "iifname",
            "\"wg0\"",
            "accept",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map_err(|error| error.to_string())?;
    assert!(tampered.success());
    let error = sidecar
        .ensure_healthy()
        .await
        .expect_err("an altered input firewall must fail closed");
    assert_eq!(error.kind(), "sandbox_unavailable");
    Ok(())
}

#[tokio::test]
async fn podman_tunnel_sidecar_wireguard_lifecycle_and_join() {
    if !gate_open("LUNARWING_E2E_PODMAN_TESTS") || !gate_open("LUNARWING_E2E_TUNNEL_TESTS") {
        eprintln!("skipping: set both podman and tunnel e2e gates to 1");
        return;
    }
    let image = tunnel_image()
        .await
        .expect("tunnel image with wg-quick, iproute2, and nftables");
    let private_key = generate_wg_private_key(&image)
        .await
        .expect("generate a WireGuard key in the e2e image");
    let workdir = std::env::temp_dir().join(format!("lw-tunnel-join-{}", std::process::id()));
    std::fs::create_dir_all(&workdir).expect("workdir");
    let config = workdir.join("wg0.conf");
    std::fs::write(
        &config,
        format!("[Interface]\nPrivateKey = {private_key}\nAddress = 10.231.99.1/32\n"),
    )
    .expect("write config");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600))
            .expect("chmod 600");
    }
    let sidecar = TunnelSidecar::new(
        ContainerRuntime::Podman,
        TunnelSidecarConfig {
            provider: TunnelProvider::Wireguard,
            image: Some(image.clone()),
            wireguard_config: Some(config),
            startup_timeout: Duration::from_secs(60),
            ..TunnelSidecarConfig::default()
        },
    );
    let result = exercise_sidecar(&sidecar, &image, &workdir).await;
    sidecar.stop().await;
    let _ = std::fs::remove_dir_all(&workdir);
    result.expect("real WireGuard sidecar lifecycle and egress-only namespace");
}
