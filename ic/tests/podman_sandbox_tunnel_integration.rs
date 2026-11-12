//! Real-Podman tunnel sidecar coverage (WI#248, WI#592, WI#600, WI#602, WI#603, WI#604, and WI#614).
//!
//! WireGuard is double-gated by LUNARWING_E2E_PODMAN_TESTS=1 and
//! LUNARWING_E2E_TUNNEL_TESTS=1. NetBird additionally requires
//! LUNARWING_E2E_NETBIRD_TESTS=1, a real management service, a reusable key
//! file, and an image containing NetBird, nftables, nc, wget, ping, and nslookup.
//! Optional settings cover management-server restart. NetBird custom DNS is
//! rejected by the unit-level configuration gate in WI#603.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use lunarwing::sandbox::cli_runner::{CliRunner, ExecParams};
use lunarwing::sandbox::config::ResourceLimits;
use lunarwing::sandbox::{
    ContainerRuntime, SandboxConfig, SandboxError, SandboxManager, SandboxNetworkMode,
    SandboxPolicy, SandboxRuntime, TenantLogsMode, TunnelProvider, TunnelRunConfig, TunnelSidecar,
    TunnelSidecarConfig,
};
use secrecy::SecretString;

const E2E_IMAGE: &str = "localhost/lw-tunnel-e2e:latest";
const LABEL_SALT_FILE: &str = "lunarwing-sandbox-tunnel.salt";
const JOINED_PROBE: &str = "cat /etc/resolv.conf; \
     ip -o link show wg0 >/dev/null && echo LUNARWING-TUNNEL-JOIN-OK; \
     wget -qO- -T 4 http://1.1.1.1/ >/dev/null 2>&1 && echo LUNARWING-TUNNEL-UPLINK-OPEN; \
     wget -qO- -T 3 http://169.254.169.254/ >/dev/null 2>&1 && echo LUNARWING-TUNNEL-METADATA-OPEN; \
     H=$(getent hosts host.containers.internal | cut -d' ' -f1); \
     if [ -n \"$H\" ] && nc -z -w 2 \"$H\" 22 >/dev/null 2>&1; then echo LUNARWING-TUNNEL-HOST-OPEN; fi; \
     nslookup example.com 2>&1 || true; \
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

async fn generate_wg_keypair(image: &str) -> Option<(String, String)> {
    let output = podman_output(&[
        "run",
        "--rm",
        image,
        "sh",
        "-c",
        "key=$(wg genkey); pub=$(printf %s \"$key\" | wg pubkey); printf '%s\\n%s\\n' \"$pub\" \"$key\"",
    ])
    .await
    .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();
    let public = lines.next()?.trim().to_string();
    let private = lines.next()?.trim().to_string();
    (output.status.success() && !public.is_empty() && !private.is_empty())
        .then_some((private, public))
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

fn private_runtime_dir(workdir: &Path) -> PathBuf {
    let runtime_dir = workdir.join("runtime");
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700))
            .expect("chmod 700");
    }
    runtime_dir
}

async fn tunnel_container_id(name: &str) -> Option<String> {
    let output = podman_output(&["inspect", "--format", "{{.Id}}", name])
        .await
        .ok()?;
    let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !id.is_empty()).then_some(id)
}

async fn assert_no_unfiltered_sidecar(name: &str) -> Result<(), String> {
    if tunnel_container_id(name).await.is_none() {
        return Ok(());
    }
    let listed = podman_success(&[
        "exec",
        name,
        "nft",
        "list",
        "table",
        "inet",
        "lunarwing_sandbox_egress",
    ])
    .await;
    listed.map_err(|_| format!("sidecar {name} is running without its firewall"))
}

async fn wait_container_absent(name: &str) -> Result<(), String> {
    for _ in 0..225 {
        if tunnel_container_id(name).await.is_none() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err(format!(
        "sidecar {name} remained after cancelled-start cleanup"
    ))
}

async fn manager_command_error(
    manager: &SandboxManager,
    workdir: &Path,
) -> Result<(SandboxError, Duration), String> {
    let started = std::time::Instant::now();
    match manager.execute("true", workdir, HashMap::new()).await {
        Ok(output) => Err(format!(
            "dead peer unexpectedly ran the command: {}{}",
            output.stdout, output.stderr
        )),
        Err(error) => Ok((error, started.elapsed())),
    }
}

fn manager_with_sidecar(
    image: &str,
    config: &TunnelSidecarConfig,
    sidecar: TunnelSidecar,
) -> SandboxManager {
    SandboxManager::new_with_tunnel_sidecar(
        SandboxConfig {
            image: image.to_string(),
            runtime: SandboxRuntime::Podman,
            network: SandboxNetworkMode::Tunnel,
            tunnel: Some(config.clone()),
            protected_paths: Vec::new(),
            tenant_logs: TenantLogsMode::Off,
            ..SandboxConfig::default()
        },
        sidecar,
    )
}

async fn assert_detached_helper_stopped(container_name: &str) {
    let instance = container_name
        .strip_prefix("lunarwing-sandbox-tunnel-")
        .expect("instance-specific sidecar name");
    let unit = format!("lunarwing-tunnel-{instance}");
    let manager = tokio::process::Command::new("systemctl")
        .args(["--user", "show", "--property=Version"])
        .output()
        .await;
    if !matches!(manager, Ok(output) if output.status.success()) {
        eprintln!("skipping detached-unit check: no systemd user manager");
        return;
    }
    let output = tokio::process::Command::new("systemctl")
        .args([
            "--user",
            "list-units",
            "--all",
            "--plain",
            "--no-legend",
            "--type=service",
            "--full",
        ])
        .output()
        .await
        .expect("list user units");
    assert!(output.status.success());
    let listing = String::from_utf8_lossy(&output.stdout);
    assert!(
        !listing.contains(&format!("{unit}.service")),
        "detached unit remains:\n{listing}"
    );
    for process in std::fs::read_dir("/proc").expect("procfs").flatten() {
        let path = process.path();
        if !path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().chars().all(|c| c.is_ascii_digit()))
        {
            continue;
        }
        let Ok(cmdline) = std::fs::read(path.join("cmdline")) else {
            continue;
        };
        let is_pasta = cmdline
            .split(|byte| *byte == 0)
            .next()
            .is_some_and(|program| program.ends_with(b"pasta"));
        if is_pasta
            && std::fs::read_to_string(path.join("cgroup"))
                .is_ok_and(|cgroup| cgroup.contains(&unit))
        {
            panic!("pasta remains in {unit}: {}", path.display());
        }
    }
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
        "type filter hook input priority",
        "ct state established,related accept",
        "chain output {",
        "type filter hook output priority",
        "oifname \"wg0\" accept",
        "ip daddr 169.254.0.0/16 drop",
    ] {
        assert!(
            listing.contains(expected),
            "missing {expected:?} in:\n{listing}"
        );
    }
    assert!(
        listing.lines().map(str::trim).any(|line| {
            [
                "meta mark 0x00004c57 accept",
                "mark 0x00004c57 accept",
                "meta mark 0x4c57 accept",
                "mark 0x4c57 accept",
            ]
            .contains(&line)
        }),
        "missing exact WireGuard mark accept in:\n{listing}"
    );
    assert_eq!(listing.matches("policy drop;").count(), 2, "{listing}");
    let has_uplink_dns = listing.contains("dport 53") || listing.contains("dport domain");
    assert_eq!(
        has_uplink_dns, allow_uplink_dns,
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

async fn sidecar_resolv_conf(sidecar: &TunnelSidecar) -> Result<String, String> {
    let output =
        podman_output(&["exec", sidecar.container_name(), "cat", "/etc/resolv.conf"]).await?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn old_resolver_probe(resolv_conf: &str) -> String {
    resolv_conf
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            if parts.next() != Some("nameserver") {
                return None;
            }
            let resolver = parts.next()?.parse::<IpAddr>().ok()?;
            Some(format!(
                "nslookup example.com {resolver} >/dev/null 2>&1 && echo LUNARWING-OLD-RESOLVER-OPEN;"
            ))
        })
        .collect()
}

fn assert_read_only_resolver_mount(stdout: &str) {
    let options = stdout
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            if fields.nth(4) != Some("/etc/resolv.conf") {
                return None;
            }
            fields.next()
        })
        .expect("joined-run resolv.conf mountinfo");
    assert!(
        options.split(',').any(|option| option == "ro"),
        "resolver mount is not read-only: {options}"
    );
}

async fn assert_joined_run(
    sidecar: &TunnelSidecar,
    image: &str,
    workdir: &Path,
    dns: &[IpAddr],
) -> Result<(), String> {
    let sidecar_dns =
        podman_output(&["exec", sidecar.container_name(), "nslookup", "example.com"]).await?;
    assert_eq!(
        sidecar_dns.status.success(),
        dns.is_empty(),
        "sidecar DNS does not match the uplink policy:\n{}{}",
        String::from_utf8_lossy(&sidecar_dns.stdout),
        String::from_utf8_lossy(&sidecar_dns.stderr)
    );
    let sidecar_conf = sidecar_resolv_conf(sidecar).await?;
    let old_resolver_probe = if dns.is_empty() {
        String::new()
    } else {
        for resolver in dns {
            assert!(!sidecar_conf.contains(&format!("nameserver {resolver}")));
        }
        let probe = old_resolver_probe(&sidecar_conf);
        assert!(!probe.is_empty(), "sidecar has no former resolver to probe");
        probe
    };
    let container_id = sidecar
        .ensure_healthy()
        .await
        .map_err(|error| error.to_string())?;
    let runner = CliRunner::detect(SandboxRuntime::Podman, image.to_string())
        .await
        .map_err(|error| error.to_string())?;
    let limits = ResourceLimits {
        timeout: Duration::from_secs(120),
        ..ResourceLimits::default()
    };
    let resolv_conf = sidecar
        .tunnel_resolv_conf()
        .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    if let Some(path) = resolv_conf.as_deref() {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .expect("resolver metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o644);
    }
    let command = format!("{JOINED_PROBE}\ncat /proc/self/mountinfo\n{old_resolver_probe}\ntrue");
    let output = runner
        .execute(ExecParams {
            command: &command,
            working_dir: workdir,
            policy: SandboxPolicy::ReadOnly,
            limits: &limits,
            env: &HashMap::new(),
            network: SandboxNetworkMode::Tunnel,
            proxy_port: 0,
            tunnel: Some(TunnelRunConfig {
                container_id: &container_id,
                dns,
                resolv_conf: resolv_conf.as_deref(),
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
        "LUNARWING-OLD-RESOLVER-OPEN",
    ] {
        assert!(!output.stdout.contains(open), "uplink open: {open}");
    }
    if dns.is_empty() {
        assert!(
            output.stdout.contains("LUNARWING-TUNNEL-DNS-OK"),
            "default tunnel DNS failed:\n{}{}",
            output.stdout,
            output.stderr
        );
    } else {
        assert!(output.stdout.contains("# lunarwing-tunnel-dns"));
        assert_read_only_resolver_mount(&output.stdout);
        for resolver in dns {
            assert!(output.stdout.contains(&format!("nameserver {resolver}")));
        }
    }
    Ok(())
}

async fn recreate_and_verify(
    sidecar: &TunnelSidecar,
    previous_id: &str,
    allow_uplink_dns: bool,
) -> Result<String, String> {
    sidecar
        .ensure_started()
        .await
        .map_err(|error| error.to_string())?;
    let id = tunnel_container_id(sidecar.container_name())
        .await
        .ok_or("missing recreated container id")?;
    assert_ne!(previous_id, id);
    sidecar
        .ensure_healthy()
        .await
        .map_err(|error| error.to_string())?;
    assert_firewall_listing(sidecar.container_name(), allow_uplink_dns).await;
    Ok(id)
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
    let recreated_id = recreate_and_verify(sidecar, &first_id, dns.is_empty()).await?;

    tamper_chain(name, "input", &["iifname", "\"wg0\"", "accept"]).await?;
    let error = sidecar
        .ensure_healthy()
        .await
        .expect_err("an altered input firewall must fail closed");
    assert_eq!(error.kind(), "sandbox_unavailable");
    let clean_id = recreate_and_verify(sidecar, &recreated_id, dns.is_empty()).await?;
    assert_joined_run(sidecar, image, workdir, dns).await?;

    if image == E2E_IMAGE {
        rotate_tunnel_image(image).await?;
        let error = sidecar
            .ensure_healthy()
            .await
            .expect_err("an image-ID change must invalidate the old sidecar");
        assert!(
            error.to_string().contains("current configuration"),
            "image rotation failed for the wrong reason: {error}"
        );
        recreate_and_verify(sidecar, &clean_id, dns.is_empty()).await?;
    }
    Ok(())
}

fn complete_firewall_listing(listing: &str) -> bool {
    let required = [
        "chain input {",
        "type filter hook input priority",
        "ct state established,related accept",
        "chain output {",
        "type filter hook output priority",
        "oifname \"wg0\" accept",
        "ip daddr 169.254.0.0/16 drop",
    ];
    let has_dns = listing.contains("dport 53") || listing.contains("dport domain");
    let has_mark = [
        "meta mark 0x00004c57 accept",
        "mark 0x00004c57 accept",
        "meta mark 0x4c57 accept",
        "mark 0x4c57 accept",
    ]
    .into_iter()
    .any(|mark| listing.contains(mark));
    required.iter().all(|line| listing.contains(line))
        && listing.matches("policy drop;").count() == 2
        && has_mark
        && has_dns
}

async fn monitor_stable_firewall(
    name: &str,
    done: tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {
    let mut verified: Option<String> = None;
    loop {
        if *done.borrow() {
            return verified
                .map(|_| ())
                .ok_or_else(|| "no complete firewall listing was observed".into());
        }
        let output = podman_output(&[
            "exec",
            name,
            "nft",
            "list",
            "table",
            "inet",
            "lunarwing_sandbox_egress",
        ])
        .await?;
        if output.status.success() {
            let listing = String::from_utf8_lossy(&output.stdout).into_owned();
            if !complete_firewall_listing(&listing) {
                return Err(format!("partial firewall observed:\n{listing}"));
            }
            if verified
                .as_ref()
                .is_some_and(|previous| previous != &listing)
            {
                return Err("an already verified firewall was rewritten".into());
            }
            verified = Some(listing);
        } else if verified.is_some() {
            return Err("a verified firewall disappeared during concurrent starts".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn podman_tunnel_concurrent_first_commands_keep_firewall_stable() {
    if !gate_open("LUNARWING_E2E_PODMAN_TESTS") || !gate_open("LUNARWING_E2E_TUNNEL_TESTS") {
        eprintln!("skipping: set both podman and tunnel e2e gates to 1");
        return;
    }
    let image = tunnel_image().await.expect("WireGuard-capable image");
    let private_key = generate_wg_private_key(&image)
        .await
        .expect("generate a WireGuard key");
    let workdir = std::env::temp_dir().join(format!("lw-tunnel-race-{}", std::process::id()));
    let config = write_wireguard_config(&workdir, &private_key);
    let tunnel_config = TunnelSidecarConfig {
        provider: TunnelProvider::Wireguard,
        image: Some(image.clone()),
        wireguard_config: Some(config),
        startup_timeout: Duration::from_secs(60),
        ..TunnelSidecarConfig::default()
    };
    let runtime_dir = private_runtime_dir(&workdir);
    let sidecar = TunnelSidecar::for_instance_with_runtime_dir(
        ContainerRuntime::Podman,
        tunnel_config.clone(),
        &workdir,
        &runtime_dir,
    );
    let name = sidecar.container_name().to_string();
    let manager = std::sync::Arc::new(manager_with_sidecar(&image, &tunnel_config, sidecar));
    let (done, monitor_done) = tokio::sync::watch::channel(false);
    let monitor_name = name.clone();
    let monitor =
        tokio::spawn(async move { monitor_stable_firewall(&monitor_name, monitor_done).await });

    let first = manager.execute("sleep 3; echo FIRST", &workdir, HashMap::new());
    let second = manager.execute("echo SECOND", &workdir, HashMap::new());
    let (first, second) = tokio::join!(first, second);
    let _ = done.send(true);
    let monitored = monitor.await.expect("firewall monitor task");
    manager.shutdown().await;
    assert_detached_helper_stopped(&name).await;
    let _ = std::fs::remove_dir_all(&workdir);

    let first = first.expect("first concurrent sandbox command");
    let second = second.expect("second concurrent sandbox command");
    assert_eq!(first.exit_code, 0, "{}{}", first.stdout, first.stderr);
    assert_eq!(second.exit_code, 0, "{}{}", second.stdout, second.stderr);
    assert!(first.stdout.contains("FIRST"));
    assert!(second.stdout.contains("SECOND"));
    monitored.expect("firewall stayed complete and unchanged during concurrent first commands");
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
    let runtime_dir = private_runtime_dir(&workdir);
    let sidecar = TunnelSidecar::for_instance_with_runtime_dir(
        ContainerRuntime::Podman,
        tunnel_config.clone(),
        &workdir,
        &runtime_dir,
    );
    let other_dir = workdir.join("other-instance");
    let other = TunnelSidecar::for_instance_with_runtime_dir(
        ContainerRuntime::Podman,
        tunnel_config,
        &other_dir,
        &runtime_dir,
    );
    assert_ne!(sidecar.container_name(), other.container_name());
    let result = async {
        other
            .ensure_started()
            .await
            .map_err(|error| error.to_string())?;
        sidecar
            .ensure_started()
            .await
            .map_err(|error| error.to_string())?;
        sidecar
            .ensure_healthy()
            .await
            .map_err(|error| error.to_string())?;
        other
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
    assert_detached_helper_stopped(sidecar.container_name()).await;
    assert_detached_helper_stopped(other.container_name()).await;
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
    let runtime_dir = private_runtime_dir(&workdir);
    let sidecar = TunnelSidecar::for_instance_with_runtime_dir(
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
        &runtime_dir,
    );
    let result = exercise_sidecar(&sidecar, &image, &workdir, &dns).await;
    sidecar.stop().await;
    let _ = std::fs::remove_dir_all(&workdir);
    result.expect("custom tunnel DNS must remove uplink DNS");
}

#[tokio::test]
async fn podman_tunnel_sidecar_hostname_endpoint_reaches_handshake_gate() {
    if !gate_open("LUNARWING_E2E_PODMAN_TESTS") || !gate_open("LUNARWING_E2E_TUNNEL_TESTS") {
        eprintln!("skipping: set both podman and tunnel e2e gates to 1");
        return;
    }
    let image = tunnel_image().await.expect("WireGuard-capable image");
    let private_key = generate_wg_private_key(&image)
        .await
        .expect("generate a WireGuard key");
    let peer_key = generate_wg_private_key(&image)
        .await
        .expect("generate a peer key");
    let workdir = std::env::temp_dir().join(format!("lw-tunnel-peer-{}", std::process::id()));
    std::fs::create_dir_all(&workdir).expect("workdir");
    let config = workdir.join("wg0.conf");
    std::fs::write(
        &config,
        format!(
            "[Interface]\nPrivateKey = {private_key}\nAddress = 10.231.99.1/32\n\
             [Peer]\nPublicKey = {peer_key}\nEndpoint = localhost:51820\n\
             AllowedIPs = 10.231.99.2/32\nPersistentKeepalive = 25\n"
        ),
    )
    .expect("write peered config");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600))
            .expect("chmod 600");
    }
    let runtime_dir = private_runtime_dir(&workdir);
    let sidecar = TunnelSidecar::for_instance_with_runtime_dir(
        ContainerRuntime::Podman,
        TunnelSidecarConfig {
            provider: TunnelProvider::Wireguard,
            image: Some(image),
            wireguard_config: Some(config),
            dns: vec!["10.231.99.53".parse().unwrap()],
            startup_timeout: Duration::from_secs(6),
            ..TunnelSidecarConfig::default()
        },
        &workdir,
        &runtime_dir,
    );
    let name = sidecar.container_name().to_string();
    let start = tokio::spawn(async move { sidecar.ensure_started().await });
    let mut endpoint_visible = false;
    for _ in 0..30 {
        if start.is_finished() {
            break;
        }
        if let Ok(output) = podman_output(&["exec", &name, "wg", "show", "wg0", "endpoints"]).await
            && output.status.success()
            && String::from_utf8_lossy(&output.stdout).contains("51820")
        {
            endpoint_visible = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let resolver_conf = podman_output(&["exec", &name, "cat", "/etc/resolv.conf"]).await;
    let result = start.await.expect("sidecar start task");
    let _ = podman_success(&["rm", "-f", &name]).await;
    let _ = std::fs::remove_dir_all(&workdir);
    let error = result.expect_err("an unreachable peer must fail the handshake gate");
    assert_eq!(error.kind(), "sandbox_unavailable");
    assert!(
        endpoint_visible,
        "wg-quick did not resolve the hostname endpoint and configure the peer"
    );
    let resolver_conf = resolver_conf.expect("read bootstrap resolvers");
    assert!(resolver_conf.status.success());
    assert!(!String::from_utf8_lossy(&resolver_conf.stdout).contains("10.231.99.53"));
}

async fn assert_cancelled_start_cleans_up(
    config: TunnelSidecarConfig,
    instance_dir: &Path,
) -> Result<(), String> {
    let runtime_dir = instance_dir.join("runtime");
    std::fs::create_dir_all(&runtime_dir).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    let sidecar = TunnelSidecar::for_instance_with_runtime_dir(
        ContainerRuntime::Podman,
        config,
        instance_dir,
        &runtime_dir,
    );
    let name = sidecar.container_name().to_string();
    let task_sidecar = sidecar.clone();
    let start = tokio::spawn(async move { task_sidecar.recover_for_run().await });
    let mut appeared = false;
    for _ in 0..75 {
        if start.is_finished() {
            break;
        }
        if tunnel_container_id(&name).await.is_some() {
            appeared = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if !appeared {
        let outcome = start.await;
        sidecar.stop().await;
        return Err(format!(
            "sidecar did not appear before its start ended: {outcome:?}"
        ));
    }
    start.abort();
    let _ = start.await;
    let cleaned = wait_container_absent(&name).await;
    let cached_started = std::time::Instant::now();
    let cached = sidecar.recover_for_run().await;
    let cached_elapsed = cached_started.elapsed();
    sidecar.stop().await;
    cleaned?;
    let cached = cached.expect_err("the cancelled recovery must retain its failure");
    if cached_elapsed >= Duration::from_secs(1)
        || !cached.to_string().contains("previous recovery failed")
    {
        return Err(format!(
            "cancelled recovery did not use its recorded back-off ({cached_elapsed:?}): {cached}"
        ));
    }
    assert_no_unfiltered_sidecar(&name).await
}

#[tokio::test]
async fn podman_tunnel_dead_peer_fails_fast_through_manager() {
    if !gate_open("LUNARWING_E2E_PODMAN_TESTS") || !gate_open("LUNARWING_E2E_TUNNEL_TESTS") {
        eprintln!("skipping: set both podman and tunnel e2e gates to 1");
        return;
    }
    let image = tunnel_image().await.expect("WireGuard-capable image");
    let private_key = generate_wg_private_key(&image)
        .await
        .expect("generate a WireGuard key");
    let peer_key = generate_wg_private_key(&image)
        .await
        .expect("generate a peer key");
    let workdir =
        std::env::temp_dir().join(format!("lw-tunnel-dead-manager-{}", std::process::id()));
    std::fs::create_dir_all(&workdir).expect("workdir");
    let config = workdir.join("wg0.conf");
    std::fs::write(
        &config,
        format!(
            "[Interface]\nPrivateKey = {private_key}\nAddress = 10.231.99.1/32\n\
             [ Peer ] # office\nPublicKey = {peer_key}\nEndpoint = localhost:51820\n\
             AllowedIPs = 10.231.99.2/32\nPersistentKeepalive = 25\n"
        ),
    )
    .expect("write peered config");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600))
            .expect("chmod 600");
    }
    let instance_dir = workdir.join("instance");
    let workspace = workdir.join("workspace");
    let runtime_dir = workdir.join("runtime");
    std::fs::create_dir_all(&instance_dir).expect("instance dir");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700))
            .expect("chmod 700");
    }
    let tunnel_config = TunnelSidecarConfig {
        provider: TunnelProvider::Wireguard,
        image: Some(image.clone()),
        wireguard_config: Some(config),
        startup_timeout: Duration::from_secs(20),
        ..TunnelSidecarConfig::default()
    };
    let sidecar = TunnelSidecar::for_instance_with_runtime_dir(
        ContainerRuntime::Podman,
        tunnel_config.clone(),
        &instance_dir,
        &runtime_dir,
    );
    let name = sidecar.container_name().to_string();
    let manager = SandboxManager::new_with_tunnel_sidecar(
        SandboxConfig {
            image,
            runtime: SandboxRuntime::Podman,
            network: SandboxNetworkMode::Tunnel,
            tunnel: Some(tunnel_config.clone()),
            protected_paths: Vec::new(),
            tenant_logs: TenantLogsMode::Off,
            ..SandboxConfig::default()
        },
        sidecar,
    );
    let result = async {
        let (first, first_elapsed) = manager_command_error(&manager, &workspace).await?;
        if first.kind() != "sandbox_unavailable" || !first.to_string().contains("handshake") {
            return Err(format!("first failure missed the handshake gate: {first}"));
        }
        if first_elapsed >= Duration::from_secs(60) {
            return Err(format!(
                "first failure exceeded the bounded recovery: {first_elapsed:?}"
            ));
        }
        assert_no_unfiltered_sidecar(&name).await?;

        let (second, second_elapsed) = manager_command_error(&manager, &workspace).await?;
        if second_elapsed >= Duration::from_secs(1) {
            return Err(format!("cached failure took too long: {second_elapsed:?}"));
        }
        if !second.to_string().contains("previous recovery failed") {
            return Err(format!("second failure did not use the back-off: {second}"));
        }

        tokio::time::sleep(Duration::from_secs(31)).await;
        let (third, third_elapsed) = manager_command_error(&manager, &workspace).await?;
        if third_elapsed < Duration::from_secs(1)
            || third.to_string().contains("previous recovery failed")
        {
            return Err(format!("post-back-off command did not retry: {third}"));
        }
        assert_no_unfiltered_sidecar(&name).await?;
        assert_cancelled_start_cleans_up(tunnel_config, &workdir.join("cancelled")).await
    }
    .await;
    manager.shutdown().await;
    let _ = std::fs::remove_dir_all(&workdir);
    result.expect("dead-peer manager recovery, back-off, retry, and cancelled-start cleanup");
}

async fn build_delayed_tunnel_image(base: &str) -> Result<String, String> {
    let dir = std::env::temp_dir().join(format!("lw-tunnel-slow-image-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let image = format!("localhost/lw-tunnel-slow-e2e:{}", std::process::id());
    let containerfile = format!(
        "FROM {base}\n\
         RUN WG_QUICK=\"$(command -v wg-quick)\" && \\\n\
             mv \"$WG_QUICK\" /usr/local/bin/wg-quick-real && \\\n\
             printf '#!/bin/sh\\nsleep 25\\nexec /usr/local/bin/wg-quick-real \"$@\"\\n' > \"$WG_QUICK\" && \\\n\
             chmod 0755 \"$WG_QUICK\"\n"
    );
    std::fs::write(dir.join("Containerfile"), containerfile).map_err(|error| error.to_string())?;
    let result = podman_success(&["build", "-t", &image, dir.to_string_lossy().as_ref()]).await;
    let _ = std::fs::remove_dir_all(&dir);
    result.map(|()| image)
}

#[tokio::test]
async fn podman_tunnel_cold_start_uses_the_configured_provider_budget() {
    if !gate_open("LUNARWING_E2E_PODMAN_TESTS") || !gate_open("LUNARWING_E2E_TUNNEL_TESTS") {
        eprintln!("skipping: set both podman and tunnel e2e gates to 1");
        return;
    }
    let sandbox_image = tunnel_image().await.expect("WireGuard-capable image");
    let delayed_image = build_delayed_tunnel_image(&sandbox_image)
        .await
        .expect("build delayed provider image");
    let private_key = generate_wg_private_key(&sandbox_image)
        .await
        .expect("generate a WireGuard key");
    let workdir = std::env::temp_dir().join(format!("lw-tunnel-slow-start-{}", std::process::id()));
    let instance_dir = workdir.join("instance");
    let workspace = workdir.join("workspace");
    let runtime_dir = workdir.join("runtime");
    std::fs::create_dir_all(&instance_dir).expect("instance dir");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700))
            .expect("chmod 700");
    }
    let config = write_wireguard_config(&workdir, &private_key);
    let tunnel_config = TunnelSidecarConfig {
        provider: TunnelProvider::Wireguard,
        image: Some(delayed_image.clone()),
        wireguard_config: Some(config),
        startup_timeout: Duration::from_secs(35),
        ..TunnelSidecarConfig::default()
    };
    let sidecar = TunnelSidecar::for_instance_with_runtime_dir(
        ContainerRuntime::Podman,
        tunnel_config.clone(),
        &instance_dir,
        &runtime_dir,
    );
    let manager = SandboxManager::new_with_tunnel_sidecar(
        SandboxConfig {
            image: sandbox_image,
            runtime: SandboxRuntime::Podman,
            network: SandboxNetworkMode::Tunnel,
            tunnel: Some(tunnel_config),
            protected_paths: Vec::new(),
            tenant_logs: TenantLogsMode::Off,
            ..SandboxConfig::default()
        },
        sidecar,
    );
    let started = std::time::Instant::now();
    let result = manager.execute("true", &workspace, HashMap::new()).await;
    let elapsed = started.elapsed();
    manager.shutdown().await;
    let _ = podman_success(&["image", "rm", "-f", &delayed_image]).await;
    let _ = std::fs::remove_dir_all(&workdir);
    let output = result.expect("a provider taking about 25 seconds must start");
    assert_eq!(output.exit_code, 0, "{}{}", output.stdout, output.stderr);
    assert!(
        elapsed >= Duration::from_secs(20),
        "the delayed provider did not exercise the old 20s cap: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(45),
        "cold start did not retain its configured 35s budget: {elapsed:?}"
    );
}

struct TestPeerConfig<'a> {
    private_key: &'a str,
    address: &'a str,
    peer_public_key: &'a str,
    peer_address: &'a str,
    endpoint: Option<&'a str>,
    listen_port: Option<u16>,
}

fn write_test_peer_config(path: &Path, config: &TestPeerConfig<'_>) {
    let endpoint = config
        .endpoint
        .map(|value| format!("Endpoint = {value}\n"))
        .unwrap_or_default();
    let listen = config
        .listen_port
        .map(|port| format!("ListenPort = {port}\n"))
        .unwrap_or_default();
    std::fs::write(
        path,
        format!(
            "[Interface]\nPrivateKey = {}\nAddress = {}\n{listen}\n\
             [Peer]\nPublicKey = {}\n{endpoint}AllowedIPs = {}\nPersistentKeepalive = 1\n",
            config.private_key, config.address, config.peer_public_key, config.peer_address
        ),
    )
    .expect("write peer config");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("chmod 600");
    }
}

async fn build_dual_peer_image(base: &str, peer_config: &Path) -> Result<String, String> {
    let dir = std::env::temp_dir().join(format!("lw-tunnel-dual-image-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let image = format!("localhost/lw-tunnel-dual-e2e:{}", std::process::id());
    std::fs::copy(peer_config, dir.join("wg1.conf")).map_err(|error| error.to_string())?;
    let wrapper = "#!/bin/sh\nset -eu\n\
         if [ \"${1:-}\" = up ] && [ \"${2:-}\" = /etc/wireguard/wg0.conf ]; then\n\
             /usr/local/bin/wg-quick-real up /etc/wireguard/wg0.conf\n\
             sleep 10\n\
             if [ ! -e /tmp/lunarwing-peer-disabled ]; then\n\
                 /usr/local/bin/wg-quick-real up /etc/wireguard/wg1.conf\n\
             fi\n\
             exit 0\n\
         fi\n\
         exec /usr/local/bin/wg-quick-real \"$@\"\n";
    std::fs::write(dir.join("wg-quick"), wrapper).map_err(|error| error.to_string())?;
    let containerfile = format!(
        "FROM {base}\n\
         COPY wg1.conf /etc/wireguard/wg1.conf\n\
         COPY wg-quick /usr/local/bin/lunarwing-wg-quick\n\
         RUN WG_QUICK=\"$(command -v wg-quick)\" && \\\n\
             mv \"$WG_QUICK\" /usr/local/bin/wg-quick-real && \\\n\
             cp /usr/local/bin/lunarwing-wg-quick \"$WG_QUICK\" && \\\n\
             chmod 0755 \"$WG_QUICK\" && chmod 0600 /etc/wireguard/wg1.conf\n"
    );
    std::fs::write(dir.join("Containerfile"), containerfile).map_err(|error| error.to_string())?;
    let result = podman_success(&["build", "-t", &image, dir.to_string_lossy().as_ref()]).await;
    let _ = std::fs::remove_dir_all(&dir);
    result.map(|()| image)
}

async fn disable_recreated_peer(
    name: &str,
    previous_id: &str,
    peer_public: &str,
) -> Result<(), String> {
    for _ in 0..300 {
        if tunnel_container_id(name)
            .await
            .is_some_and(|id| id != previous_id)
            && podman_success(&["exec", name, "touch", "/tmp/lunarwing-peer-disabled"])
                .await
                .is_ok()
            && podman_success(&[
                "exec",
                name,
                "wg",
                "set",
                "wg0",
                "peer",
                peer_public,
                "endpoint",
                "127.0.0.1:9",
            ])
            .await
            .is_ok()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err("could not disable the recreated sidecar's peer".into())
}

async fn wait_for_stale_handshake(sidecar: &TunnelSidecar) -> Result<(), String> {
    for _ in 0..100 {
        match sidecar.ensure_healthy().await {
            Ok(_) => tokio::time::sleep(Duration::from_secs(2)).await,
            Err(error) if error.to_string().contains("liveness budget") => return Ok(()),
            Err(error) => return Err(format!("unexpected health failure: {error}")),
        }
    }
    Err("the dead peer's handshake did not age out".into())
}

fn write_live_peer_configs(
    workdir: &Path,
    primary_private: &str,
    primary_public: &str,
    peer_private: &str,
    peer_public: &str,
) -> (PathBuf, PathBuf) {
    let primary_config = workdir.join("wg0.conf");
    let peer_config = workdir.join("peer.conf");
    write_test_peer_config(
        &primary_config,
        &TestPeerConfig {
            private_key: primary_private,
            address: "10.231.100.1/32",
            peer_public_key: peer_public,
            peer_address: "10.231.100.2/32",
            endpoint: Some("127.0.0.1:51821"),
            listen_port: Some(51820),
        },
    );
    write_test_peer_config(
        &peer_config,
        &TestPeerConfig {
            private_key: peer_private,
            address: "10.231.100.2/32",
            peer_public_key: primary_public,
            peer_address: "10.231.100.1/32",
            endpoint: Some("127.0.0.1:51820"),
            listen_port: Some(51821),
        },
    );
    (primary_config, peer_config)
}

#[tokio::test]
async fn podman_tunnel_healthy_peer_death_uses_bounded_reprobe() {
    if !gate_open("LUNARWING_E2E_PODMAN_TESTS") || !gate_open("LUNARWING_E2E_TUNNEL_TESTS") {
        eprintln!("skipping: set both podman and tunnel e2e gates to 1");
        return;
    }
    let image = tunnel_image().await.expect("WireGuard-capable image");
    let (primary_private, primary_public) =
        generate_wg_keypair(&image).await.expect("primary keypair");
    let (peer_private, peer_public) = generate_wg_keypair(&image).await.expect("peer keypair");
    let workdir = std::env::temp_dir().join(format!("lw-tunnel-live-peer-{}", std::process::id()));
    std::fs::create_dir_all(&workdir).expect("workdir");
    let (primary_config, peer_config) = write_live_peer_configs(
        &workdir,
        &primary_private,
        &primary_public,
        &peer_private,
        &peer_public,
    );
    let dual_image = build_dual_peer_image(&image, &peer_config)
        .await
        .expect("build dual-peer image");
    let runtime_dir = private_runtime_dir(&workdir);
    let tunnel_config = TunnelSidecarConfig {
        provider: TunnelProvider::Wireguard,
        image: Some(dual_image.clone()),
        wireguard_config: Some(primary_config),
        startup_timeout: Duration::from_secs(45),
        ..TunnelSidecarConfig::default()
    };
    let sidecar = TunnelSidecar::for_instance_with_runtime_dir(
        ContainerRuntime::Podman,
        tunnel_config.clone(),
        &workdir,
        &runtime_dir,
    );
    let name = sidecar.container_name().to_string();
    let result = async {
        sidecar
            .ensure_started()
            .await
            .map_err(|error| error.to_string())?;
        let previous_id = sidecar
            .ensure_healthy()
            .await
            .map_err(|error| error.to_string())?;
        let manager = manager_with_sidecar(&image, &tunnel_config, sidecar.clone());
        let output = manager
            .execute("true", &workdir, HashMap::new())
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(output.exit_code, 0, "{}{}", output.stdout, output.stderr);

        podman_success(&["exec", &name, "wg-quick", "down", "/etc/wireguard/wg1.conf"]).await?;
        podman_success(&[
            "exec",
            &name,
            "wg",
            "set",
            "wg0",
            "peer",
            &peer_public,
            "endpoint",
            "127.0.0.1:9",
        ])
        .await?;
        wait_for_stale_handshake(&sidecar).await?;
        let command = manager_command_error(&manager, &workdir);
        let disable = disable_recreated_peer(&name, &previous_id, &peer_public);
        let (command, disabled) = tokio::join!(command, disable);
        disabled?;
        let (first, elapsed) = command.map_err(|error| format!("post-death recovery: {error}"))?;
        if elapsed < Duration::from_secs(6) || elapsed >= Duration::from_secs(60) {
            return Err(format!(
                "recovery did not exercise the bounded re-probe: {elapsed:?}"
            ));
        }
        if !first.to_string().contains("20s per-run cap") {
            return Err(format!("recovery missed the fresh-provider cap: {first}"));
        }
        assert_no_unfiltered_sidecar(&name).await?;
        let (_, cached_elapsed) = manager_command_error(&manager, &workdir)
            .await
            .map_err(|error| format!("cached peer-death command: {error}"))?;
        if cached_elapsed >= Duration::from_secs(1) {
            return Err(format!(
                "cached peer-death failure took too long: {cached_elapsed:?}"
            ));
        }
        Ok(())
    }
    .await;
    sidecar.stop().await;
    let _ = podman_success(&["image", "rm", "-f", &dual_image]).await;
    let _ = std::fs::remove_dir_all(&workdir);
    result.expect("a real handshake followed by peer death must fail through bounded recovery");
}

fn required_env(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{name} is required for the NetBird live lane"))
}

fn required_netbird_setup_key() -> Result<String, String> {
    let path = required_env("LUNARWING_E2E_NETBIRD_SETUP_KEY_FILE")?;
    let key = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read the NetBird setup-key file: {error}"))?;
    let key = key.trim();
    if key.is_empty() || key.contains(char::is_whitespace) {
        return Err("the NetBird setup-key file must contain one token".into());
    }
    Ok(key.to_string())
}

async fn netbird_ipv4(container: &str) -> Result<IpAddr, String> {
    let output = podman_output(&["exec", container, "netbird", "status", "--ipv4"]).await?;
    if !output.status.success() {
        return Err("netbird status --ipv4 failed".into());
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|_| "NetBird returned an invalid overlay IPv4 address".to_string())
}

async fn wait_netbird_ready(container: &str) -> Result<(), String> {
    for _ in 0..60 {
        if podman_success(&["exec", container, "netbird", "status", "--check", "startup"])
            .await
            .is_ok()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err("NetBird did not become ready".into())
}

fn write_peer_env_file(workdir: &Path, url: &str, key: &str) -> Result<PathBuf, String> {
    let path = workdir.join("peer.env");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .map_err(|error| format!("could not create the peer env file: {error}"))?;
    std::io::Write::write_all(
        &mut file,
        format!(
            "NB_MANAGEMENT_URL={url}\nNB_SETUP_KEY={key}\nNB_LOG_FILE=console\nNB_DISABLE_FIREWALL=true\n"
        )
        .as_bytes(),
    )
    .map_err(|error| format!("could not write the peer env file: {error}"))?;
    Ok(path)
}

async fn start_plain_netbird_peer(image: &str, name: &str, env_file: &Path) -> Result<(), String> {
    let env_file = env_file.to_string_lossy().into_owned();
    let state_volume = format!("{name}:/var/lib/netbird");
    let started = podman_success(&[
        "run",
        "-d",
        "--rm",
        "--name",
        name,
        "--cap-drop",
        "ALL",
        "--cap-add",
        "NET_ADMIN",
        "--cap-add",
        "NET_RAW",
        "--cap-add",
        "NET_BIND_SERVICE",
        "--security-opt=no-new-privileges",
        "--read-only",
        "--tmpfs",
        "/tmp:size=16m",
        "--tmpfs",
        "/run:size=16m",
        "--device",
        "/dev/net/tun",
        "-v",
        &state_volume,
        "--env-file",
        &env_file,
        image,
    ])
    .await;
    let _ = std::fs::remove_file(&env_file);
    started?;
    wait_netbird_ready(name).await
}

async fn netbird_status_detail(container: &str) -> Result<String, String> {
    let output = podman_output(&["exec", container, "netbird", "status", "-d"]).await?;
    if !output.status.success() {
        return Err("netbird status -d failed".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn status_has_connected_peer(status: &str, ip: IpAddr) -> bool {
    let ip = ip.to_string();
    status.split("\n\n").any(|block| {
        block.contains(&ip)
            && block.lines().any(|line| {
                let line = line.trim_start();
                line.starts_with("Status:") && line.contains("Connected")
            })
    })
}

#[test]
fn netbird_peer_status_requires_the_same_peer_block() {
    let ip: IpAddr = "100.64.0.2".parse().unwrap();
    assert!(status_has_connected_peer(
        "Peers:\n  IP: 100.64.0.2\n  Status: Connected\n",
        ip
    ));
    assert!(!status_has_connected_peer(
        "Peers:\n  IP: 100.64.0.2\n\nManagement:\n  Status: Connected\n",
        ip
    ));
}

async fn assert_netbird_peers_connected(primary: &str, peer: &str) -> Result<(), String> {
    for _ in 0..30 {
        let primary_ip = netbird_ipv4(primary).await?;
        let peer_ip = netbird_ipv4(peer).await?;
        let _ = podman_success(&[
            "exec",
            primary,
            "ping",
            "-c",
            "1",
            "-W",
            "1",
            &peer_ip.to_string(),
        ])
        .await;
        let _ = podman_success(&[
            "exec",
            peer,
            "ping",
            "-c",
            "1",
            "-W",
            "1",
            &primary_ip.to_string(),
        ])
        .await;
        let primary_status = netbird_status_detail(primary).await?;
        let peer_status = netbird_status_detail(peer).await?;
        if status_has_connected_peer(&primary_status, peer_ip)
            && status_has_connected_peer(&peer_status, primary_ip)
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err("NetBird peers did not report each other as Connected".into())
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

async fn assert_netbird_restart_identity(
    sidecar: &TunnelSidecar,
    runtime_dir: &Path,
) -> Result<String, String> {
    let name = sidecar.container_name();
    let first_id = tunnel_container_id(name)
        .await
        .ok_or("missing NetBird container ID")?;
    let first_ip = netbird_ipv4(name).await?;
    let first_volume = netbird_volume(sidecar).await?;
    sidecar.stop().await;
    std::fs::remove_file(runtime_dir.join(LABEL_SALT_FILE))
        .map_err(|error| format!("could not remove the runtime label salt: {error}"))?;
    sidecar
        .ensure_started()
        .await
        .map_err(|error| error.to_string())?;
    assert_ne!(
        first_id,
        tunnel_container_id(name).await.unwrap_or_default()
    );
    assert_eq!(first_ip, netbird_ipv4(name).await?);
    assert_eq!(first_volume, netbird_volume(sidecar).await?);
    Ok(first_volume)
}

async fn assert_netbird_reconnect(sidecar: &TunnelSidecar) -> Result<(), String> {
    let name = sidecar.container_name();
    let servers = std::env::var("LUNARWING_E2E_NETBIRD_SERVER_CONTAINER").unwrap_or_default();
    let mut restart = vec!["restart"];
    restart.extend(servers.split_whitespace());
    if restart.len() > 1 {
        podman_success(&restart).await?;
    } else {
        podman_success(&["exec", name, "netbird", "down"]).await?;
        podman_success(&["exec", name, "netbird", "up"]).await?;
    }
    wait_netbird_ready(name).await?;
    sidecar
        .ensure_started()
        .await
        .map_err(|error| error.to_string())?;
    let mut stable_checks = 0;
    for _ in 0..60 {
        if sidecar.ensure_healthy().await.is_ok() {
            stable_checks += 1;
            if stable_checks == 3 {
                return Ok(());
            }
        } else {
            stable_checks = 0;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err("NetBird did not remain ready after reconnect".into())
}

async fn assert_netbird_mark_rule(sidecar: &TunnelSidecar) -> Result<(), String> {
    let output = podman_output(&[
        "exec",
        sidecar.container_name(),
        "nft",
        "list",
        "table",
        "inet",
        "lunarwing_sandbox_egress",
    ])
    .await?;
    if !output.status.success() {
        return Err("could not list the NetBird sidecar firewall".into());
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    let accepts = listing
        .lines()
        .map(str::trim)
        .filter(|line| {
            [
                "meta mark 0x0001bd00 accept",
                "mark 0x0001bd00 accept",
                "meta mark 0x1bd00 accept",
                "mark 0x1bd00 accept",
            ]
            .contains(line)
        })
        .count();
    assert_eq!(accepts, 1, "wrong NetBird mark rules:\n{listing}");
    assert!(listing.contains("oifname \"wt0\" accept"), "{listing}");
    Ok(())
}

async fn start_netbird_http_server(peer: &str) -> Result<(), String> {
    if podman_success(&["exec", peer, "nc", "-z", "-w", "1", "127.0.0.1", "8080"])
        .await
        .is_ok()
    {
        return Ok(());
    }
    podman_success(&[
        "exec",
        "-d",
        peer,
        "sh",
        "-c",
        "while true; do printf 'HTTP/1.0 200 OK\\r\\nContent-Type: text/plain\\r\\n\\r\\nLUNARWING-NETBIRD-OK\\n' | nc -l -p 8080; done",
    ])
    .await?;
    for _ in 0..30 {
        if podman_success(&["exec", peer, "nc", "-z", "-w", "1", "127.0.0.1", "8080"])
            .await
            .is_ok()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err("the plain-peer HTTP listener never became ready".into())
}

async fn assert_netbird_joined_run(
    sidecar: &TunnelSidecar,
    image: &str,
    workdir: &Path,
    peer: &str,
) -> Result<(), String> {
    let peer_ip = netbird_ipv4(peer).await?;
    start_netbird_http_server(peer).await?;
    let container_id = sidecar
        .ensure_healthy()
        .await
        .map_err(|error| error.to_string())?;
    let runner = CliRunner::detect(SandboxRuntime::Podman, image.to_string())
        .await
        .map_err(|error| error.to_string())?;
    let limits = ResourceLimits {
        timeout: Duration::from_secs(120),
        ..ResourceLimits::default()
    };
    let command = format!(
        "cat /proc/net/dev; \
         wget -qO- -T 5 http://{peer_ip}:8080/ | grep -q LUNARWING-NETBIRD-OK \
         && echo LUNARWING-NETBIRD-OVERLAY-OK; \
         wget -qO- -T 4 http://1.1.1.1/ >/dev/null 2>&1 \
         && echo LUNARWING-NETBIRD-UPLINK-OPEN; \
         wget -qO- -T 3 http://169.254.169.254/ >/dev/null 2>&1 \
         && echo LUNARWING-NETBIRD-METADATA-OPEN; true"
    );
    let output = runner
        .execute(ExecParams {
            command: &command,
            working_dir: workdir,
            policy: SandboxPolicy::ReadOnly,
            limits: &limits,
            env: &HashMap::new(),
            network: SandboxNetworkMode::Tunnel,
            proxy_port: 0,
            tunnel: Some(TunnelRunConfig {
                container_id: &container_id,
                dns: &[],
                resolv_conf: None,
            }),
            protected_paths: &[],
            tenant_logs: false,
        })
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(output.exit_code, 0, "{}{}", output.stdout, output.stderr);
    assert!(output.stdout.contains("wt0"), "joined run has no wt0");
    assert!(
        output.stdout.contains("LUNARWING-NETBIRD-OVERLAY-OK"),
        "peer HTTP fetch failed: {}{}",
        output.stdout,
        output.stderr
    );
    assert!(!output.stdout.contains("LUNARWING-NETBIRD-UPLINK-OPEN"));
    assert!(!output.stdout.contains("LUNARWING-NETBIRD-METADATA-OPEN"));
    Ok(())
}

async fn wait_local_listener(sidecar: &TunnelSidecar) -> Result<(), String> {
    for _ in 0..30 {
        if podman_success(&[
            "exec",
            sidecar.container_name(),
            "nc",
            "-z",
            "-w",
            "1",
            "127.0.0.1",
            "18080",
        ])
        .await
        .is_ok()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err("the sandbox listener never became ready".into())
}

async fn assert_netbird_ingress_denied(primary: &TunnelSidecar, peer: &str) -> Result<(), String> {
    assert_netbird_peers_connected(primary.container_name(), peer).await?;
    let primary_ip = netbird_ipv4(primary.container_name()).await?;
    podman_success(&[
        "exec",
        "-d",
        primary.container_name(),
        "sh",
        "-c",
        "while true; do nc -l -p 18080; done",
    ])
    .await?;
    wait_local_listener(primary).await?;
    let output = podman_output(&[
        "exec",
        peer,
        "nc",
        "-z",
        "-w",
        "3",
        &primary_ip.to_string(),
        "18080",
    ])
    .await?;
    if output.status.success() {
        return Err("peer-initiated connection reached the sandbox listener".into());
    }
    wait_local_listener(primary).await?;
    Ok(())
}

async fn remove_netbird_volumes(prefix: &str) {
    if let Ok(output) = podman_output(&["volume", "ls", "--format", "{{.Name}}"]).await
        && output.status.success()
    {
        for name in String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|name| name.starts_with(prefix))
        {
            let _ = podman_success(&["volume", "rm", name]).await;
        }
    }
}

#[tokio::test]
async fn podman_tunnel_sidecar_netbird_live() {
    if !gate_open("LUNARWING_E2E_PODMAN_TESTS")
        || !gate_open("LUNARWING_E2E_TUNNEL_TESTS")
        || !gate_open("LUNARWING_E2E_NETBIRD_TESTS")
    {
        eprintln!("skipping: set podman, tunnel, and NetBird e2e gates to 1");
        return;
    }
    let settings: Result<(String, String, String), String> = (|| {
        Ok((
            required_env("LUNARWING_E2E_NETBIRD_IMAGE")?,
            required_env("LUNARWING_E2E_NETBIRD_MANAGEMENT_URL")?,
            required_netbird_setup_key()?,
        ))
    })();
    let (image, management_url, setup_key) = settings.expect("NetBird live settings");
    let workdir = std::env::temp_dir().join(format!("lw-netbird-live-{}", std::process::id()));
    let primary_dir = workdir.join("primary");
    let runtime_dir = workdir.join("runtime");
    std::fs::create_dir_all(&primary_dir).expect("primary instance dir");
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700))
            .expect("private runtime dir");
    }
    let primary = TunnelSidecar::for_instance_with_runtime_dir(
        ContainerRuntime::Podman,
        TunnelSidecarConfig {
            provider: TunnelProvider::Netbird,
            image: Some(image.clone()),
            netbird_management_url: Some(management_url.clone()),
            netbird_setup_key: Some(SecretString::from(setup_key.clone())),
            startup_timeout: Duration::from_secs(120),
            ..TunnelSidecarConfig::default()
        },
        &primary_dir,
        &runtime_dir,
    );
    let peer_name = format!("{}-plain-peer", primary.container_name());
    let peer_env = write_peer_env_file(&workdir, &management_url, &setup_key)
        .expect("stage plain-peer credentials");
    let volume_prefix = format!("{}-netbird-", primary.container_name());
    let result = async {
        primary
            .ensure_started()
            .await
            .map_err(|error| error.to_string())?;
        start_plain_netbird_peer(&image, &peer_name, &peer_env).await?;
        assert_netbird_peers_connected(primary.container_name(), &peer_name).await?;
        assert_netbird_mark_rule(&primary).await?;
        assert_netbird_joined_run(&primary, &image, &workdir, &peer_name).await?;
        let primary_volume = netbird_volume(&primary).await?;
        assert_eq!(
            primary_volume,
            assert_netbird_restart_identity(&primary, &runtime_dir).await?
        );
        assert_netbird_reconnect(&primary).await?;
        assert_netbird_joined_run(&primary, &image, &workdir, &peer_name).await?;
        assert_netbird_ingress_denied(&primary, &peer_name).await
    }
    .await;
    primary.stop().await;
    let _ = podman_success(&["rm", "-f", &peer_name]).await;
    remove_netbird_volumes(&volume_prefix).await;
    let _ = podman_success(&["volume", "rm", &peer_name]).await;
    let _ = std::fs::remove_file(peer_env);
    let _ = std::fs::remove_dir_all(&workdir);
    result.expect("live NetBird connectivity, joined-run, reconnect, identity, and ingress checks");
}
