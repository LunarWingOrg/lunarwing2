//! Real-Podman tunnel sidecar coverage (WI#248 and WI#592).
//!
//! WireGuard is double-gated by LUNARWING_E2E_PODMAN_TESTS=1 and
//! LUNARWING_E2E_TUNNEL_TESTS=1. NetBird additionally requires
//! LUNARWING_E2E_NETBIRD_TESTS=1, a real management service, two setup keys,
//! and an image containing NetBird, nftables, and nc.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use lunarwing::sandbox::cli_runner::{CliRunner, ExecParams};
use lunarwing::sandbox::config::ResourceLimits;
use lunarwing::sandbox::{
    ContainerRuntime, SandboxNetworkMode, SandboxPolicy, SandboxRuntime, TunnelProvider,
    TunnelRunConfig, TunnelSidecar, TunnelSidecarConfig,
};
use secrecy::SecretString;

const E2E_IMAGE: &str = "localhost/lw-tunnel-e2e:latest";
const JOINED_PROBE: &str = "cat /etc/resolv.conf; \
     ip -o link show wg0 >/dev/null && echo LUNARWING-TUNNEL-JOIN-OK; \
     wget -qO- -T 4 http://1.1.1.1/ >/dev/null 2>&1 && echo LUNARWING-TUNNEL-UPLINK-OPEN; \
     wget -qO- -T 3 http://169.254.169.254/ >/dev/null 2>&1 && echo LUNARWING-TUNNEL-METADATA-OPEN; \
     H=$(getent hosts host.containers.internal | cut -d' ' -f1); \
     if [ -n \"$H\" ] && nc -z -w 2 \"$H\" 22 >/dev/null 2>&1; then echo LUNARWING-TUNNEL-HOST-OPEN; fi; \
     nslookup example.com >/dev/null 2>&1 && echo LUNARWING-TUNNEL-DNS-OK; \
     true";

fn gate_open(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

async fn podman_output(args: &[&str]) -> Result<Output, String> {
    tokio::process::Command::new("podman")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|error| error.to_string())
}

async fn podman_success(args: &[&str]) -> Result<(), String> {
    let output = podman_output(args).await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "podman {} failed: {}",
            args.first().copied().unwrap_or("command"),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

async fn tunnel_image() -> Option<String> {
    if let Ok(image) = std::env::var("LUNARWING_E2E_TUNNEL_IMAGE") {
        return Some(image);
    }
    let exists = podman_success(&["image", "exists", E2E_IMAGE])
        .await
        .is_ok();
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
    let result = podman_success(&["build", "-t", E2E_IMAGE, dir.to_string_lossy().as_ref()]).await;
    let _ = std::fs::remove_dir_all(&dir);
    result.is_ok()
}

async fn rotate_tunnel_image(image: &str) -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("lw-tunnel-rotate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let containerfile = dir.join("Containerfile");
    std::fs::write(
        &containerfile,
        format!("FROM {image}\nRUN echo wi592 > /etc/lunarwing-image-rotation\n"),
    )
    .map_err(|error| error.to_string())?;
    let result = podman_success(&["build", "-t", image, dir.to_string_lossy().as_ref()]).await;
    let _ = std::fs::remove_dir_all(&dir);
    result
}

async fn generate_wg_private_key(image: &str) -> Option<String> {
    let output = podman_output(&["run", "--rm", image, "wg", "genkey"])
        .await
        .ok()?;
    let key = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !key.is_empty()).then_some(key)
}

fn write_wireguard_config(workdir: &Path, private_key: &str) -> PathBuf {
    std::fs::create_dir_all(workdir).expect("workdir");
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
    config
}

async fn tunnel_container_id(name: &str) -> Option<String> {
    let output = podman_output(&["inspect", "--format", "{{.Id}}", name])
        .await
        .ok()?;
    let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !id.is_empty()).then_some(id)
}

async fn assert_firewall_listing(name: &str, allow_uplink_dns: bool) {
    let output = podman_output(&[
        "exec",
        name,
        "nft",
        "list",
        "table",
        "inet",
        "lunarwing_sandbox_egress",
    ])
    .await
    .expect("nft listing");
    assert!(output.status.success());
    let listing = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "chain input {",
        "type filter hook input priority filter; policy drop;",
        "ct state established,related accept",
        "chain output {",
        "type filter hook output priority filter; policy drop;",
        "oifname \"wg0\" accept",
        "ip daddr 169.254.0.0/16 drop",
        "0x00004c57 accept",
    ] {
        assert!(
            listing.contains(expected),
            "missing {expected:?} in:\n{listing}"
        );
    }
    assert_eq!(
        listing.contains("dport 53"),
        allow_uplink_dns,
        "unexpected DNS policy in:\n{listing}"
    );
}

async fn tamper_chain(name: &str, chain: &str, rule: &[&str]) -> Result<(), String> {
    let mut args = vec![
        "exec",
        name,
        "nft",
        "add",
        "rule",
        "inet",
        "lunarwing_sandbox_egress",
        chain,
    ];
    args.extend_from_slice(rule);
    podman_success(&args).await
}

async fn assert_joined_run(
    sidecar: &TunnelSidecar,
    image: &str,
    workdir: &Path,
    dns: &[IpAddr],
) -> Result<(), String> {
    let runner = CliRunner::detect(SandboxRuntime::Podman, image.to_string())
        .await
        .map_err(|error| error.to_string())?;
    let limits = ResourceLimits {
        timeout: Duration::from_secs(120),
        ..ResourceLimits::default()
    };
    let output = runner
        .execute(ExecParams {
            command: JOINED_PROBE,
            working_dir: workdir,
            policy: SandboxPolicy::ReadOnly,
            limits: &limits,
            env: &HashMap::new(),
            network: SandboxNetworkMode::Tunnel,
            proxy_port: 0,
            tunnel: Some(TunnelRunConfig {
                container_name: sidecar.container_name(),
                dns,
            }),
            protected_paths: &[],
            tenant_logs: false,
        })
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(output.exit_code, 0, "{}{}", output.stdout, output.stderr);
    assert!(output.stdout.contains("LUNARWING-TUNNEL-JOIN-OK"));
    for open in [
        "LUNARWING-TUNNEL-UPLINK-OPEN",
        "LUNARWING-TUNNEL-METADATA-OPEN",
        "LUNARWING-TUNNEL-HOST-OPEN",
    ] {
        assert!(!output.stdout.contains(open), "uplink open: {open}");
    }
    if dns.is_empty() {
        assert!(output.stdout.contains("LUNARWING-TUNNEL-DNS-OK"));
    } else {
        for resolver in dns {
            assert!(output.stdout.contains(&format!("nameserver {resolver}")));
        }
    }
    Ok(())
}

async fn exercise_sidecar(
    sidecar: &TunnelSidecar,
    image: &str,
    workdir: &Path,
    dns: &[IpAddr],
) -> Result<(), String> {
    sidecar
        .ensure_started()
        .await
        .map_err(|error| error.to_string())?;
    let name = sidecar.container_name();
    let first_id = tunnel_container_id(name)
        .await
        .ok_or("missing first container id")?;
    sidecar
        .ensure_started()
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(
        first_id,
        tunnel_container_id(name).await.unwrap_or_default()
    );
    sidecar
        .ensure_healthy()
        .await
        .map_err(|error| error.to_string())?;
    assert_firewall_listing(name, dns.is_empty()).await;
    assert_joined_run(sidecar, image, workdir, dns).await?;

    tamper_chain(name, "output", &["ip", "daddr", "1.1.1.1", "accept"]).await?;
    let error = sidecar
        .ensure_healthy()
        .await
        .expect_err("an altered output firewall must fail closed");
    assert_eq!(error.kind(), "sandbox_unavailable");
    sidecar
        .ensure_started()
        .await
        .map_err(|error| error.to_string())?;
    let recreated_id = tunnel_container_id(name)
        .await
        .ok_or("missing recreated container id")?;
    assert_ne!(first_id, recreated_id);
    tamper_chain(name, "input", &["iifname", "\"wg0\"", "accept"]).await?;
    let error = sidecar
        .ensure_healthy()
        .await
        .expect_err("an altered input firewall must fail closed");
    assert_eq!(error.kind(), "sandbox_unavailable");

    if image == E2E_IMAGE {
        rotate_tunnel_image(image).await?;
        sidecar
            .ensure_healthy()
            .await
            .expect_err("an image-ID change must invalidate the old sidecar");
        sidecar
            .ensure_started()
            .await
            .map_err(|error| error.to_string())?;
        assert_ne!(
            recreated_id,
            tunnel_container_id(name).await.unwrap_or_default()
        );
        sidecar
            .ensure_healthy()
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[tokio::test]
async fn podman_tunnel_sidecar_wireguard_lifecycle_and_join() {
    if !gate_open("LUNARWING_E2E_PODMAN_TESTS") || !gate_open("LUNARWING_E2E_TUNNEL_TESTS") {
        eprintln!("skipping: set both podman and tunnel e2e gates to 1");
        return;
    }
    let image = tunnel_image().await.expect("WireGuard-capable image");
    let private_key = generate_wg_private_key(&image)
        .await
        .expect("generate a WireGuard key");
    let workdir = std::env::temp_dir().join(format!("lw-tunnel-join-{}", std::process::id()));
    let config = write_wireguard_config(&workdir, &private_key);
    let tunnel_config = TunnelSidecarConfig {
        provider: TunnelProvider::Wireguard,
        image: Some(image.clone()),
        wireguard_config: Some(config),
        startup_timeout: Duration::from_secs(60),
        ..TunnelSidecarConfig::default()
    };
    let sidecar =
        TunnelSidecar::for_instance(ContainerRuntime::Podman, tunnel_config.clone(), &workdir);
    let other_dir = workdir.join("other-instance");
    let other = TunnelSidecar::for_instance(ContainerRuntime::Podman, tunnel_config, &other_dir);
    assert_ne!(sidecar.container_name(), other.container_name());
    let result = async {
        other
            .ensure_started()
            .await
            .map_err(|error| error.to_string())?;
        sidecar
            .ensure_healthy()
            .await
            .map_err(|error| error.to_string())?;
        other.stop().await;
        sidecar
            .ensure_healthy()
            .await
            .map_err(|error| error.to_string())?;
        exercise_sidecar(&sidecar, &image, &workdir, &[]).await
    }
    .await;
    sidecar.stop().await;
    other.stop().await;
    let _ = std::fs::remove_dir_all(&workdir);
    result.expect("real WireGuard sidecar lifecycle and egress-only namespace");
}

#[tokio::test]
async fn podman_tunnel_sidecar_custom_dns_closes_uplink() {
    if !gate_open("LUNARWING_E2E_PODMAN_TESTS") || !gate_open("LUNARWING_E2E_TUNNEL_TESTS") {
        eprintln!("skipping: set both podman and tunnel e2e gates to 1");
        return;
    }
    let image = tunnel_image().await.expect("WireGuard-capable image");
    let private_key = generate_wg_private_key(&image)
        .await
        .expect("generate a WireGuard key");
    let workdir = std::env::temp_dir().join(format!("lw-tunnel-dns-{}", std::process::id()));
    let config = write_wireguard_config(&workdir, &private_key);
    let dns = vec!["10.231.99.53".parse().unwrap()];
    let sidecar = TunnelSidecar::for_instance(
        ContainerRuntime::Podman,
        TunnelSidecarConfig {
            provider: TunnelProvider::Wireguard,
            image: Some(image.clone()),
            wireguard_config: Some(config),
            dns: dns.clone(),
            startup_timeout: Duration::from_secs(60),
            ..TunnelSidecarConfig::default()
        },
        &workdir,
    );
    let result = exercise_sidecar(&sidecar, &image, &workdir, &dns).await;
    sidecar.stop().await;
    let _ = std::fs::remove_dir_all(&workdir);
    result.expect("custom tunnel DNS must remove uplink DNS");
}

fn required_env(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{name} is required for the NetBird live lane"))
}

async fn netbird_ipv4(sidecar: &TunnelSidecar) -> Result<IpAddr, String> {
    let output = podman_output(&[
        "exec",
        sidecar.container_name(),
        "netbird",
        "status",
        "--ipv4",
    ])
    .await?;
    if !output.status.success() {
        return Err("netbird status --ipv4 failed".into());
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|_| "NetBird returned an invalid overlay IPv4 address".to_string())
}

async fn netbird_volume(sidecar: &TunnelSidecar) -> Result<String, String> {
    let format =
        "{{range .Mounts}}{{if eq .Destination \"/var/lib/netbird\"}}{{.Name}}{{end}}{{end}}";
    let output = podman_output(&["inspect", "--format", format, sidecar.container_name()]).await?;
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if output.status.success() && !name.is_empty() {
        Ok(name)
    } else {
        Err("NetBird state volume is missing".into())
    }
}

async fn assert_netbird_restart_identity(sidecar: &TunnelSidecar) -> Result<String, String> {
    let first_id = tunnel_container_id(sidecar.container_name())
        .await
        .ok_or("missing NetBird container ID")?;
    let first_ip = netbird_ipv4(sidecar).await?;
    let first_volume = netbird_volume(sidecar).await?;
    sidecar.stop().await;
    sidecar
        .ensure_started()
        .await
        .map_err(|error| error.to_string())?;
    assert_ne!(
        first_id,
        tunnel_container_id(sidecar.container_name())
            .await
            .unwrap_or_default()
    );
    assert_eq!(first_ip, netbird_ipv4(sidecar).await?);
    assert_eq!(first_volume, netbird_volume(sidecar).await?);
    Ok(first_volume)
}

async fn assert_netbird_reconnect(sidecar: &TunnelSidecar) -> Result<(), String> {
    podman_success(&["exec", sidecar.container_name(), "netbird", "down"]).await?;
    podman_success(&["exec", sidecar.container_name(), "netbird", "up"]).await?;
    sidecar
        .ensure_started()
        .await
        .map_err(|error| error.to_string())
}

async fn assert_netbird_ingress_denied(
    primary: &TunnelSidecar,
    peer: &TunnelSidecar,
) -> Result<(), String> {
    let primary_ip = netbird_ipv4(primary).await?;
    podman_success(&[
        "exec",
        "-d",
        primary.container_name(),
        "nc",
        "-l",
        "-p",
        "18080",
    ])
    .await?;
    let output = podman_output(&[
        "exec",
        peer.container_name(),
        "nc",
        "-z",
        "-w",
        "3",
        &primary_ip.to_string(),
        "18080",
    ])
    .await?;
    if output.status.success() {
        Err("peer-initiated connection reached the sandbox listener".into())
    } else {
        Ok(())
    }
}

#[tokio::test]
async fn podman_tunnel_sidecar_netbird_live() {
    if !gate_open("LUNARWING_E2E_PODMAN_TESTS") || !gate_open("LUNARWING_E2E_NETBIRD_TESTS") {
        eprintln!("skipping: set both podman and NetBird e2e gates to 1");
        return;
    }
    let settings: Result<(String, String, String, String), String> = (|| {
        Ok((
            required_env("LUNARWING_E2E_NETBIRD_IMAGE")?,
            required_env("NB_MANAGEMENT_URL")?,
            required_env("NB_SETUP_KEY")?,
            required_env("NB_PEER_SETUP_KEY")?,
        ))
    })();
    let (image, management_url, setup_key, peer_setup_key) =
        settings.expect("NetBird live settings");
    let workdir = std::env::temp_dir().join(format!("lw-netbird-live-{}", std::process::id()));
    let primary_dir = workdir.join("primary");
    let peer_dir = workdir.join("peer");
    std::fs::create_dir_all(&primary_dir).expect("primary instance dir");
    std::fs::create_dir_all(&peer_dir).expect("peer instance dir");
    let make_config = |key: String| TunnelSidecarConfig {
        provider: TunnelProvider::Netbird,
        image: Some(image.clone()),
        netbird_management_url: Some(management_url.clone()),
        netbird_setup_key: Some(SecretString::from(key)),
        startup_timeout: Duration::from_secs(120),
        ..TunnelSidecarConfig::default()
    };
    let primary = TunnelSidecar::for_instance(
        ContainerRuntime::Podman,
        make_config(setup_key),
        &primary_dir,
    );
    let peer = TunnelSidecar::for_instance(
        ContainerRuntime::Podman,
        make_config(peer_setup_key),
        &peer_dir,
    );
    assert_ne!(primary.container_name(), peer.container_name());
    let mut volumes = Vec::new();
    let result = async {
        primary
            .ensure_started()
            .await
            .map_err(|error| error.to_string())?;
        peer.ensure_started()
            .await
            .map_err(|error| error.to_string())?;
        primary
            .ensure_healthy()
            .await
            .map_err(|error| error.to_string())?;
        peer.ensure_healthy()
            .await
            .map_err(|error| error.to_string())?;
        let primary_volume = netbird_volume(&primary).await?;
        let peer_volume = netbird_volume(&peer).await?;
        assert_ne!(primary_volume, peer_volume);
        volumes.push(primary_volume.clone());
        volumes.push(peer_volume);
        assert_eq!(
            primary_volume,
            assert_netbird_restart_identity(&primary).await?
        );
        assert_netbird_reconnect(&primary).await?;
        assert_netbird_ingress_denied(&primary, &peer).await
    }
    .await;
    primary.stop().await;
    peer.stop().await;
    for volume in &volumes {
        let _ = podman_success(&["volume", "rm", volume]).await;
    }
    let _ = std::fs::remove_dir_all(&workdir);
    result.expect("live NetBird reconnect, identity, and ingress checks");
}
