//! Egress-only NetBird and WireGuard sidecars for the generic sandbox (WI#248).
//!
//! Sandboxed commands join this sidecar's network namespace while retaining
//! their own filesystem, user namespace, and hardening flags. The sidecar owns
//! all tunnel credentials and carries a kernel-enforced netfilter policy in
//! its namespace:
//!
//! - an `input` chain drops every new inbound flow, so joining the namespace
//!   cannot expose a listener to the host, LAN, or overlay (WI#357);
//! - an `output` chain drops every outbound flow that does not leave through
//!   the tunnel interface. The uplink (pasta/slirp, or the Docker bridge)
//!   carries only the tunnel's own control traffic, recognised by the socket
//!   mark the provider sets on it and, by default, DNS to the resolvers the
//!   runtime gave the sidecar. `SANDBOX_TUNNEL_DNS` moves DNS to resolvers
//!   reachable through the tunnel and removes the uplink DNS allowance.
//!   Link-local IPv4 (the cloud metadata service and podman's
//!   `host.containers.internal` alias) is dropped explicitly.
//!
//! Joined commands have no `NET_ADMIN`, cannot set socket marks, and therefore
//! cannot reach the uplink at all: with a split tunnel they reach the overlay
//! CIDRs only, with a full-route tunnel everything goes through the overlay.
//! Both chains are installed before the first joiner and re-verified, rule by
//! rule, before every run; a missing or altered chain fails closed.

use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};

use crate::sandbox::cli_runner::ContainerRuntime;
use crate::sandbox::error::{Result, SandboxError};

/// Prefix of the daemon-instance-specific sidecar name.
pub const SANDBOX_TUNNEL_CONTAINER: &str = "lunarwing-sandbox-tunnel";
pub const SANDBOX_TUNNEL_CONTAINER_LABEL: &str = "lunarwing.sandbox-tunnel";
pub const SANDBOX_TUNNEL_HASH_LABEL: &str = "lunarwing.sandbox-tunnel-hash";
pub const SANDBOX_TUNNEL_INSTANCE_LABEL: &str = "lunarwing.sandbox-tunnel-instance";

/// fwmark the sidecar sets on a WireGuard device whose config left it `off`
/// (ASCII "LW"). wg-quick's own full-route fwmark (the routing table number)
/// is kept when the config already set one.
pub const LUNARWING_TUNNEL_FWMARK: u32 = 0x4c57;
/// NetBird's control-plane socket mark (`client/net/fwmark.go`: `fwmarkBase`
/// 0x1BD00 | `offsetControlPlane` 0x00). Every management, signal, relay,
/// STUN and WireGuard socket the client opens carries it while advanced
/// routing is active, which is the default on Linux with `NET_ADMIN`.
pub const NETBIRD_CONTROL_PLANE_MARK: u32 = 0x1BD00;

const SIDECAR_MEMORY_MB: u64 = 256;
const SIDECAR_PIDS_LIMIT: u32 = 64;
const HEALTH_POLL_INTERVAL: Duration = Duration::from_secs(2);
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(15);
const RUN_RECOVERY_REPROBE_TIMEOUT: Duration = Duration::from_secs(6);
const RUN_PROVIDER_READY_TIMEOUT: Duration = Duration::from_secs(20);
const RUN_RECOVERY_BUDGET: Duration = Duration::from_secs(60);
const RECOVERY_BACKOFF: Duration = Duration::from_secs(30);
const WIREGUARD_HANDSHAKE_MAX_AGE_SECS: u64 = 180;
const WIREGUARD_KEEPALIVE_MAX_SECS: u64 = 60;
const MAX_WIREGUARD_CONFIG_BYTES: u64 = 1024 * 1024;
const NFT_TABLE: &str = "lunarwing_sandbox_egress";
const NFT_INPUT_CHAIN: &str = "input";
const NFT_OUTPUT_CHAIN: &str = "output";
/// Interfaces the providers create inside the sidecar namespace.
const TUNNEL_INTERFACES: [&str; 2] = ["wg0", "wt0"];
/// IPv4 link-local: the cloud metadata service (169.254.169.254) and podman's
/// `host.containers.internal` alias (169.254.1.2 under pasta) live here.
const LINK_LOCAL_V4: &str = "169.254.0.0/16";
const LABEL_SALT_FILE: &str = "lunarwing-sandbox-tunnel.salt";
const IDENTITY_SALT_FILE: &str = "sandbox-tunnel-identity.salt";
const IDENTITY_SALT_BYTES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TunnelProvider {
    #[default]
    Netbird,
    Wireguard,
}

impl TunnelProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Netbird => "netbird",
            Self::Wireguard => "wireguard",
        }
    }

    pub fn default_image(self) -> &'static str {
        match self {
            // NetBird 0.82.0, OCI multi-architecture index. Keep the version in
            // this comment and the digest in sync when upgrading the client.
            Self::Netbird => {
                "docker.io/netbirdio/netbird@sha256:eca5b274c635bfbb04ec6724a0712d1146190dd450eea199eeb83916fdd94965"
            }
            Self::Wireguard => "localhost/lunarwing-tunnel-wireguard:latest",
        }
    }

    /// The interface the provider brings up inside the sidecar namespace.
    pub fn interface(self) -> &'static str {
        match self {
            Self::Netbird => "wt0",
            Self::Wireguard => "wg0",
        }
    }
}

impl std::str::FromStr for TunnelProvider {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "netbird" => Ok(Self::Netbird),
            "wireguard" | "wg" => Ok(Self::Wireguard),
            _ => Err(format!(
                "invalid tunnel provider '{value}', expected 'netbird' or 'wireguard'"
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TunnelSidecarConfig {
    pub provider: TunnelProvider,
    pub image: Option<String>,
    pub netbird_management_url: Option<String>,
    pub netbird_setup_key: Option<SecretString>,
    pub wireguard_config: Option<PathBuf>,
    /// Optional resolver addresses reachable through the tunnel. When nonempty,
    /// joined runs use them and no DNS traffic is allowed onto the uplink.
    pub dns: Vec<IpAddr>,
    /// Optional ICMP canary reachable only through WireGuard.
    pub wireguard_canary: Option<IpAddr>,
    pub startup_timeout: Duration,
    /// Mirrors `SANDBOX_AUTO_PULL`: when false the sidecar image must already
    /// be present in the local store, otherwise the start fails closed instead
    /// of letting the runtime pull a mutable tag from a registry.
    pub auto_pull_image: bool,
}

impl Default for TunnelSidecarConfig {
    fn default() -> Self {
        Self {
            provider: TunnelProvider::Netbird,
            image: None,
            netbird_management_url: None,
            netbird_setup_key: None,
            wireguard_config: None,
            dns: Vec::new(),
            wireguard_canary: None,
            startup_timeout: Duration::from_secs(90),
            auto_pull_image: true,
        }
    }
}

impl TunnelSidecarConfig {
    pub fn image(&self) -> &str {
        self.image
            .as_deref()
            .unwrap_or(self.provider.default_image())
    }

    /// A stable drift marker, never an authentication value. The salt is a
    /// random per-user secret kept in the private runtime directory, so the
    /// label published on the container is not a plain digest of the setup key
    /// or of the WireGuard private key. WireGuard file contents and the resolved
    /// image ID are included so credential or image rotation recreates the sidecar.
    pub fn config_hash(&self, salt: &[u8], image_id: &str) -> String {
        let mut hasher = Sha256::new();
        hash_field(&mut hasher, salt);
        hash_field(&mut hasher, self.provider.as_str().as_bytes());
        hash_field(&mut hasher, self.image().as_bytes());
        hash_field(&mut hasher, image_id.as_bytes());
        for resolver in &self.dns {
            hash_field(&mut hasher, resolver.to_string().as_bytes());
        }
        let canary = self.wireguard_canary.map(|address| address.to_string());
        hash_optional(&mut hasher, canary.as_deref());
        hash_optional(&mut hasher, self.netbird_management_url.as_deref());
        hash_optional(
            &mut hasher,
            self.netbird_setup_key
                .as_ref()
                .map(|key| key.expose_secret()),
        );
        let wireguard_path = self
            .wireguard_config
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        hash_optional(&mut hasher, wireguard_path.as_deref());
        if let Some(path) = self.wireguard_config.as_ref()
            && let Ok(contents) = std::fs::read(path)
        {
            hash_field(&mut hasher, &contents);
        }
        format!("{:x}", hasher.finalize())
    }

    /// Named volume holding NetBird's peer identity and config across sidecar
    /// restarts. Only instance, management URL, or setup-key rotation enrols a
    /// fresh peer; image, DNS, canary, and WireGuard changes retain the volume.
    pub fn netbird_state_volume(&self, instance_id: &str, salt: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hash_field(&mut hasher, salt);
        hash_field(&mut hasher, instance_id.as_bytes());
        hash_optional(&mut hasher, self.netbird_management_url.as_deref());
        hash_optional(
            &mut hasher,
            self.netbird_setup_key
                .as_ref()
                .map(|key| key.expose_secret()),
        );
        let identity = format!("{:x}", hasher.finalize());
        format!(
            "{SANDBOX_TUNNEL_CONTAINER}-{instance_id}-netbird-{}",
            identity.get(..16).unwrap_or(&identity)
        )
    }

    fn uplink_resolvers(&self, resolv_conf: &str) -> Vec<IpAddr> {
        if self.dns.is_empty() {
            parse_resolvers(resolv_conf)
        } else {
            Vec::new()
        }
    }

    pub fn validate(&self) -> std::result::Result<(), String> {
        validate_image_reference(self.image())?;
        if self
            .dns
            .iter()
            .any(|address| !valid_tunnel_address(address))
        {
            return Err(
                "SANDBOX_TUNNEL_DNS rejects loopback, link-local, broadcast, unspecified, and multicast addresses"
                    .into(),
            );
        }
        if self.provider == TunnelProvider::Netbird && !self.dns.is_empty() {
            return Err(
                "SANDBOX_TUNNEL_DNS is unsupported with the netbird provider; leave it unset to use runtime DNS"
                    .into(),
            );
        }
        if self
            .wireguard_canary
            .as_ref()
            .is_some_and(|address| !valid_tunnel_address(address))
        {
            return Err(
                "SANDBOX_TUNNEL_WIREGUARD_CANARY rejects loopback, link-local, broadcast, unspecified, and multicast addresses"
                    .into(),
            );
        }
        if self.provider != TunnelProvider::Wireguard && self.wireguard_canary.is_some() {
            return Err("SANDBOX_TUNNEL_WIREGUARD_CANARY requires the wireguard provider".into());
        }
        match self.provider {
            TunnelProvider::Netbird => self.validate_netbird(),
            TunnelProvider::Wireguard => {
                let path = self.wireguard_config.as_deref().ok_or_else(|| {
                    "SANDBOX_TUNNEL_WIREGUARD_CONFIG is required for the wireguard provider"
                        .to_string()
                })?;
                validate_wireguard_config(path, self.wireguard_canary.is_some())
            }
        }?;
        if self.startup_timeout.is_zero() {
            return Err("tunnel startup timeout must be greater than zero".into());
        }
        Ok(())
    }

    fn validate_netbird(&self) -> std::result::Result<(), String> {
        let raw_url = self.netbird_management_url.as_deref().ok_or_else(|| {
            "SANDBOX_TUNNEL_NETBIRD_MANAGEMENT_URL is required for the netbird provider".to_string()
        })?;
        let url = url::Url::parse(raw_url).map_err(|_| {
            "SANDBOX_TUNNEL_NETBIRD_MANAGEMENT_URL must be a valid https:// URL".to_string()
        })?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(
                "SANDBOX_TUNNEL_NETBIRD_MANAGEMENT_URL must be an https:// URL without \
                 userinfo, query, or fragment"
                    .into(),
            );
        }
        let key = self.netbird_setup_key.as_ref().ok_or_else(|| {
            "SANDBOX_TUNNEL_NETBIRD_SETUP_KEY is required for the netbird provider".to_string()
        })?;
        if key.expose_secret().trim().is_empty()
            || key.expose_secret().contains(char::is_whitespace)
        {
            return Err("SANDBOX_TUNNEL_NETBIRD_SETUP_KEY must be a single token".into());
        }
        Ok(())
    }
}

fn valid_tunnel_address(address: &IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            !address.is_unspecified()
                && !address.is_loopback()
                && !address.is_link_local()
                && !address.is_broadcast()
                && !address.is_multicast()
        }
        IpAddr::V6(address) => {
            !address.is_unspecified()
                && !address.is_loopback()
                && !address.is_unicast_link_local()
                && !address.is_multicast()
        }
    }
}

fn valid_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_image_reference(image: &str) -> std::result::Result<(), String> {
    if image.is_empty() || image.trim() != image {
        return Err(
            "SANDBOX_TUNNEL_IMAGE must not be empty or surround the reference with whitespace"
                .into(),
        );
    }
    if let Some((name, digest)) = image.split_once('@') {
        let valid_digest = digest.strip_prefix("sha256:").is_some_and(valid_sha256_hex);
        if name.is_empty() || !valid_digest {
            return Err(
                "SANDBOX_TUNNEL_IMAGE digest references must use image@sha256:<64 hex characters>"
                    .into(),
            );
        }
    }
    Ok(())
}

fn parse_image_id(output: &str) -> Option<&str> {
    let id = output.trim();
    let hex = id.strip_prefix("sha256:").unwrap_or(id);
    valid_sha256_hex(hex).then_some(id)
}

fn instance_id_for(base_dir: &Path) -> String {
    let digest = format!(
        "{:x}",
        Sha256::digest(base_dir.to_string_lossy().as_bytes())
    );
    digest[..16].to_string()
}

pub fn container_name_for_instance(base_dir: &Path) -> String {
    format!("{SANDBOX_TUNNEL_CONTAINER}-{}", instance_id_for(base_dir))
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn hash_optional(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_field(hasher, value.as_bytes());
        }
        None => hasher.update([0]),
    }
}

fn validate_wireguard_config(
    path: &Path,
    canary_configured: bool,
) -> std::result::Result<(), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        format!(
            "SANDBOX_TUNNEL_WIREGUARD_CONFIG {} is unreadable: {error}",
            path.display()
        )
    })?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "SANDBOX_TUNNEL_WIREGUARD_CONFIG {} must be a regular, non-symlink file",
            path.display()
        ));
    }
    if metadata.len() > MAX_WIREGUARD_CONFIG_BYTES {
        return Err("WireGuard config exceeds the 1 MiB limit".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(format!(
                "SANDBOX_TUNNEL_WIREGUARD_CONFIG {} must be mode 0600 because it holds a \
                 private key",
                path.display()
            ));
        }
    }
    let content = std::fs::read_to_string(path).map_err(|error| {
        format!(
            "SANDBOX_TUNNEL_WIREGUARD_CONFIG {} is unreadable: {error}",
            path.display()
        )
    })?;
    let parsed = parse_wireguard_config(&content)
        .map_err(|reason| format!("invalid WireGuard config {}: {reason}", path.display()))?;
    if !parsed.has_interface_private_key {
        return Err(format!(
            "SANDBOX_TUNNEL_WIREGUARD_CONFIG {} needs a non-empty PrivateKey in [Interface]",
            path.display()
        ));
    }
    if parsed.has_peers && !canary_configured && !parsed.has_suitable_keepalive {
        return Err(
            "peered WireGuard configs require PersistentKeepalive = 1..=60 on at least one peer, or SANDBOX_TUNNEL_WIREGUARD_CANARY"
                .into(),
        );
    }
    Ok(())
}

#[derive(Debug, Default)]
struct WireGuardConfigSummary {
    has_interface_private_key: bool,
    has_peers: bool,
    has_suitable_keepalive: bool,
}

struct WireGuardPeerSummary {
    header_line: usize,
    has_public_key: bool,
    keepalive: Option<u64>,
}

fn finish_wireguard_peer(
    peer: Option<WireGuardPeerSummary>,
    summary: &mut WireGuardConfigSummary,
) -> std::result::Result<(), String> {
    let Some(peer) = peer else {
        return Ok(());
    };
    if !peer.has_public_key {
        return Err(format!(
            "[Peer] at line {} needs a non-empty PublicKey",
            peer.header_line
        ));
    }
    summary.has_peers = true;
    summary.has_suitable_keepalive |= peer
        .keepalive
        .is_some_and(|seconds| (1..=WIREGUARD_KEEPALIVE_MAX_SECS).contains(&seconds));
    Ok(())
}

fn parse_wireguard_keepalive(value: &str, line: usize) -> std::result::Result<u64, String> {
    if value == "off" {
        return Ok(0);
    }
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!(
            "PersistentKeepalive at line {line} must be 'off' or digits only"
        ));
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|seconds| *seconds <= u16::MAX as u64)
        .ok_or_else(|| format!("PersistentKeepalive at line {line} must be at most 65535"))
}

/// Normalize and validate the WireGuard fields that drive the health gate.
/// Unknown wg-quick settings are left to `wg-quick`, but malformed sections and
/// health-relevant keys are rejected with their source line rather than being
/// mistaken for a peerless config.
fn parse_wireguard_config(content: &str) -> std::result::Result<WireGuardConfigSummary, String> {
    let mut summary = WireGuardConfigSummary::default();
    let mut in_interface = false;
    let mut peer: Option<WireGuardPeerSummary> = None;
    for (index, raw) in content.lines().enumerate() {
        let line_number = index + 1;
        let line: String = raw
            .split_once('#')
            .map_or(raw, |(before_comment, _)| before_comment)
            .chars()
            .filter(|character| !character.is_whitespace())
            .flat_map(char::to_lowercase)
            .collect();
        if line.is_empty() {
            continue;
        }
        if line.starts_with(';') {
            return Err(format!(
                "semicolon comments are not accepted at line {line_number}"
            ));
        }
        if line.starts_with('[') {
            finish_wireguard_peer(peer.take(), &mut summary)?;
            in_interface = line == "[interface]";
            peer = (line == "[peer]").then_some(WireGuardPeerSummary {
                header_line: line_number,
                has_public_key: false,
                keepalive: None,
            });
            if !in_interface && peer.is_none() {
                return Err(format!("invalid section header at line {line_number}"));
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("expected key = value at line {line_number}"));
        };
        if let Some(current_peer) = peer.as_mut() {
            match key {
                "publickey" => current_peer.has_public_key = !value.is_empty(),
                "persistentkeepalive" => {
                    current_peer.keepalive = Some(parse_wireguard_keepalive(value, line_number)?);
                }
                _ => {}
            }
        } else if in_interface {
            if key == "privatekey" {
                summary.has_interface_private_key = !value.is_empty();
            } else if key == "persistentkeepalive" {
                return Err(format!(
                    "PersistentKeepalive at line {line_number} belongs in [Peer]"
                ));
            }
        } else {
            return Err(format!("key outside a section at line {line_number}"));
        }
    }
    finish_wireguard_peer(peer, &mut summary)?;
    Ok(summary)
}

/// The runtime arguments that start the sidecar. `hash` is the salted config
/// hash published as a label; `netbird_state_volume` is the named volume that
/// keeps NetBird's peer identity across restarts.
pub(crate) fn build_sidecar_args(
    cfg: &TunnelSidecarConfig,
    secret_env_file: Option<&Path>,
    container_name: &str,
    instance_id: &str,
    hash: &str,
    netbird_state_volume: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "run".into(),
        "-d".into(),
        "--rm".into(),
        "--name".into(),
        container_name.into(),
        "--label".into(),
        format!(
            "{}={}",
            SANDBOX_TUNNEL_CONTAINER_LABEL,
            cfg.provider.as_str()
        ),
        "--label".into(),
        format!("{SANDBOX_TUNNEL_INSTANCE_LABEL}={instance_id}"),
        "--label".into(),
        format!("{SANDBOX_TUNNEL_HASH_LABEL}={hash}"),
        "--cap-drop=ALL".into(),
        "--cap-add=NET_ADMIN".into(),
        "--security-opt=no-new-privileges".into(),
        "--read-only".into(),
        "--memory".into(),
        format!("{SIDECAR_MEMORY_MB}m"),
        "--pids-limit".into(),
        SIDECAR_PIDS_LIMIT.to_string(),
        "--stop-timeout".into(),
        "10".into(),
        "--sysctl".into(),
        "net.ipv4.ip_forward=0".into(),
        "--sysctl".into(),
        "net.ipv6.conf.all.forwarding=0".into(),
        // wg-quick's full-route path (`AllowedIPs = 0.0.0.0/0`) sets this
        // sysctl itself and aborts when /proc/sys is read-only; a namespaced
        // net sysctl may only be set at creation.
        "--sysctl".into(),
        "net.ipv4.conf.all.src_valid_mark=1".into(),
        "--tmpfs".into(),
        "/tmp:size=16m".into(),
        "--tmpfs".into(),
        "/run:size=16m".into(),
    ];
    match cfg.provider {
        TunnelProvider::Netbird => {
            args.extend([
                "--cap-add=NET_RAW".into(),
                "--cap-add=NET_BIND_SERVICE".into(),
                "--device".into(),
                "/dev/net/tun".into(),
            ]);
            if let Some(volume) = netbird_state_volume {
                args.extend(["-v".into(), format!("{volume}:/var/lib/netbird")]);
            }
            if let Some(file) = secret_env_file {
                args.extend(["--env-file".into(), file.display().to_string()]);
            }
            // The upstream image's entrypoint runs `netbird service run` and
            // `netbird up` itself and ignores its arguments.
            args.push(cfg.image().into());
        }
        TunnelProvider::Wireguard => {
            if let Some(config) = cfg.wireguard_config.as_deref() {
                // `Z`: a private SELinux label; the file is this sidecar's.
                args.extend([
                    "-v".into(),
                    format!("{}:/etc/wireguard/wg0.conf:ro,Z", config.display()),
                ]);
            }
            args.extend([
                cfg.image().into(),
                "/bin/sh".into(),
                "-c".into(),
                "wg-quick up /etc/wireguard/wg0.conf && exec sleep 2147483647".into(),
            ]);
        }
    }
    args
}

fn build_health_args_for(
    provider: TunnelProvider,
    container_name: &str,
    wireguard_has_peers: bool,
    wireguard_canary: Option<IpAddr>,
) -> Vec<String> {
    let probe: Vec<String> = match provider {
        TunnelProvider::Netbird => ["netbird", "status", "--check", "startup"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        TunnelProvider::Wireguard => match (wireguard_has_peers, wireguard_canary) {
            (_, Some(address)) => vec![
                "ping".into(),
                "-c".into(),
                "1".into(),
                "-W".into(),
                "2".into(),
                address.to_string(),
            ],
            (true, None) => ["wg", "show", "wg0", "latest-handshakes"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            (false, None) => ["ip", "link", "show", "wg0"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        },
    };
    let refs: Vec<&str> = probe.iter().map(String::as_str).collect();
    exec_args(&refs, container_name)
}

fn exec_args(command: &[&str], container_name: &str) -> Vec<String> {
    let mut args = vec!["exec".into(), container_name.into()];
    args.extend(command.iter().map(|arg| (*arg).into()));
    args
}

fn nft_exec_args(nft_args: &[&str], container_name: &str) -> Vec<String> {
    let mut args = vec!["exec".into(), container_name.into(), "nft".into()];
    args.extend(nft_args.iter().map(|arg| (*arg).into()));
    args
}

/// What the sidecar's `output` chain must let onto the uplink: the provider's
/// own control traffic (by socket mark) and, unless custom tunnel DNS is set,
/// DNS to the runtime-assigned resolvers. Everything else uses the tunnel or drops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EgressPolicy {
    pub tunnel_mark: u32,
    pub resolvers: Vec<IpAddr>,
}

impl EgressPolicy {
    fn input_rules() -> Vec<String> {
        vec![
            "iifname \"lo\" accept".into(),
            "ct state established,related accept".into(),
        ]
    }

    fn output_rules(&self) -> Vec<String> {
        let mut rules = vec!["oifname \"lo\" accept".to_string()];
        rules.extend(
            TUNNEL_INTERFACES
                .iter()
                .map(|ifname| format!("oifname \"{ifname}\" accept")),
        );
        // Neighbour discovery keeps an IPv6 uplink usable for the tunnel's
        // own marked traffic; it carries no payload.
        rules.extend(
            [
                "nd-router-solicit",
                "nd-neighbor-solicit",
                "nd-neighbor-advert",
            ]
            .iter()
            .map(|kind| format!("icmpv6 type {kind} accept")),
        );
        for resolver in &self.resolvers {
            let family = if resolver.is_ipv4() { "ip" } else { "ip6" };
            rules.push(format!("{family} daddr {resolver} udp dport 53 accept"));
            rules.push(format!("{family} daddr {resolver} tcp dport 53 accept"));
        }
        rules.push(format!("ip daddr {LINK_LOCAL_V4} drop"));
        rules.push(format!("meta mark 0x{:x} accept", self.tunnel_mark));
        rules
    }
}

fn firewall_chain(name: &str, rules: &[String]) -> String {
    let mut chain =
        format!("    chain {name} {{\n        type filter hook {name} priority 0; policy drop;\n");
    for rule in rules {
        chain.push_str(&format!("        {rule}\n"));
    }
    chain.push_str("    }\n");
    chain
}

fn firewall_ruleset(policy: &EgressPolicy) -> String {
    let mut ruleset = format!(
        "add table inet {NFT_TABLE}\ndelete table inet {NFT_TABLE}\ntable inet {NFT_TABLE} {{\n"
    );
    ruleset.push_str(&firewall_chain(
        NFT_INPUT_CHAIN,
        &EgressPolicy::input_rules(),
    ));
    ruleset.push_str(&firewall_chain(NFT_OUTPUT_CHAIN, &policy.output_rules()));
    ruleset.push_str("}\n");
    ruleset
}

fn firewall_install_command(policy: &EgressPolicy, container_name: &str) -> (Vec<String>, String) {
    let args = vec![
        "exec".into(),
        "-i".into(),
        container_name.into(),
        "nft".into(),
        "-f".into(),
        "-".into(),
    ];
    (args, firewall_ruleset(policy))
}

fn firewall_list_args(container_name: &str) -> Vec<String> {
    nft_exec_args(&["list", "table", "inet", NFT_TABLE], container_name)
}

/// Canonical form of one listed nft line: whitespace collapsed, the trailing
/// `# handle N` and any `comment "…"` removed, `mark` normalized to `meta mark`,
/// marks printed as `0x<hex>`, and named priority `filter` mapped back to `0`.
fn canonical_nft_line(line: &str) -> String {
    let no_handle = line.split(" # handle ").next().unwrap_or(line);
    let no_comment = no_handle.split(" comment \"").next().unwrap_or(no_handle);
    let mut out: Vec<String> = Vec::new();
    for token in no_comment.split_whitespace() {
        let trimmed = token.trim_end_matches(';');
        let suffix = if trimmed.len() == token.len() {
            ""
        } else {
            ";"
        };
        if token == "mark" && out.is_empty() {
            out.extend(["meta".to_string(), "mark".to_string()]);
        } else if let Some(hex) = trimmed.strip_prefix("0x")
            && let Ok(value) = u32::from_str_radix(hex, 16)
        {
            out.push(format!("0x{value:x}{suffix}"));
        } else if token == "domain" && out.last().map(String::as_str) == Some("dport") {
            out.push("53".to_string());
        } else if token == "filter;" && out.last().map(String::as_str) == Some("priority") {
            out.push("0;".to_string());
        } else {
            out.push(token.to_string());
        }
    }
    out.join(" ")
}

/// Strict check of `nft list table inet lunarwing_sandbox_egress`: exactly the
/// two expected base chains, each with the expected header and exactly the
/// expected rules in order. Any extra chain, extra rule, missing rule or
/// changed policy is a mismatch.
pub(crate) fn firewall_matches(
    listing: &str,
    policy: &EgressPolicy,
) -> std::result::Result<(), String> {
    let mut chains: Vec<(String, Vec<String>)> = Vec::new();
    let mut in_table = false;
    for raw in listing.lines() {
        let line = canonical_nft_line(raw);
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("table inet ") {
            if rest.trim_end_matches(" {") != NFT_TABLE {
                return Err(format!("unexpected table in listing: {rest}"));
            }
            in_table = true;
            continue;
        }
        if !in_table {
            return Err(format!("unexpected text before the table: {line}"));
        }
        if let Some(name) = line.strip_prefix("chain ") {
            chains.push((name.trim_end_matches(" {").to_string(), Vec::new()));
            continue;
        }
        if line == "}" {
            continue;
        }
        match chains.last_mut() {
            Some((_, rules)) => rules.push(line),
            None => return Err(format!("rule outside a chain: {line}")),
        }
    }
    if !in_table {
        return Err("firewall table is absent".into());
    }
    let expected: Vec<(&str, Vec<String>)> = vec![
        (NFT_INPUT_CHAIN, EgressPolicy::input_rules()),
        (NFT_OUTPUT_CHAIN, policy.output_rules()),
    ];
    if chains.len() != expected.len() {
        return Err(format!(
            "expected {} chains, found {}",
            expected.len(),
            chains.len()
        ));
    }
    for (name, rules) in &expected {
        let Some((_, listed)) = chains.iter().find(|(listed_name, _)| listed_name == name) else {
            return Err(format!("chain {name} is absent"));
        };
        let header = format!("type filter hook {name} priority 0; policy drop;");
        let Some((listed_header, listed_rules)) = listed.split_first() else {
            return Err(format!("chain {name} has no header"));
        };
        if listed_header != &header {
            return Err(format!("chain {name} header changed: {listed_header}"));
        }
        if listed_rules != rules.as_slice() {
            return Err(format!(
                "chain {name} rules changed: expected {} rules, found {}",
                rules.len(),
                listed_rules.len()
            ));
        }
    }
    Ok(())
}

/// `nameserver` entries of the sidecar's `/etc/resolv.conf`.
pub(crate) fn parse_resolvers(resolv_conf: &str) -> Vec<IpAddr> {
    let mut resolvers = Vec::new();
    for line in resolv_conf.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("nameserver") {
            continue;
        }
        if let Some(address) = parts.next()
            && let Ok(address) = address
                .split_once('%')
                .map_or(address, |(addr, _zone)| addr)
                .parse::<IpAddr>()
            && !resolvers.contains(&address)
        {
            resolvers.push(address);
        }
    }
    resolvers
}

/// `wg show wg0 fwmark` prints `off` or a hex mark.
pub(crate) fn parse_wireguard_fwmark(output: &str) -> Option<u32> {
    let hex = output.trim().strip_prefix("0x")?;
    u32::from_str_radix(hex, 16).ok()
}

fn wireguard_handshake_status(output: &str, now_unix: u64) -> std::result::Result<(), String> {
    let latest = output
        .lines()
        .filter_map(|line| {
            line.split_whitespace()
                .nth(1)
                .and_then(|timestamp| timestamp.parse::<u64>().ok())
        })
        .max();
    match latest {
        None | Some(0) => Err("WireGuard handshake probe found no completed handshake".into()),
        Some(timestamp) if timestamp > now_unix => {
            Err("WireGuard handshake probe returned a future timestamp".into())
        }
        Some(timestamp) if now_unix - timestamp > WIREGUARD_HANDSHAKE_MAX_AGE_SECS => Err(format!(
            "WireGuard handshake is {}s old, exceeding the {}s liveness budget",
            now_unix - timestamp,
            WIREGUARD_HANDSHAKE_MAX_AGE_SECS
        )),
        Some(_) => Ok(()),
    }
}

type RecoveryResult = std::result::Result<(), RecoveryError>;
type RecoveryCompletion = Option<RecoveryResult>;

#[derive(Clone, Debug)]
enum RecoveryError {
    Config { reason: String },
    Unavailable { reason: String },
}

impl RecoveryError {
    fn from_sandbox(error: &SandboxError) -> Self {
        match error {
            SandboxError::Config { reason } => Self::Config {
                reason: reason.clone(),
            },
            SandboxError::TunnelUnavailable { reason } => Self::Unavailable {
                reason: reason.clone(),
            },
            other => Self::Unavailable {
                reason: other.to_string(),
            },
        }
    }

    fn into_sandbox(self) -> SandboxError {
        match self {
            Self::Config { reason } => SandboxError::Config { reason },
            Self::Unavailable { reason } => SandboxError::TunnelUnavailable { reason },
        }
    }
}

#[derive(Default)]
struct RecoveryState {
    last_failure: Option<RecoveryFailure>,
    active: Option<tokio::sync::watch::Receiver<RecoveryCompletion>>,
    stopping: bool,
}

struct RecoveryFailure {
    failed_at: tokio::time::Instant,
    reason: String,
}

impl RecoveryState {
    fn cached_reason(&self, now: tokio::time::Instant) -> Option<String> {
        let failure = self.last_failure.as_ref()?;
        let retry_at = failure.failed_at + RECOVERY_BACKOFF;
        if now >= retry_at {
            return None;
        }
        let remaining = retry_at.duration_since(now).as_secs().max(1);
        Some(format!(
            "previous recovery failed: {}; next attempt in {}s",
            failure.reason, remaining
        ))
    }

    fn record(&mut self, error: &RecoveryError) {
        if let RecoveryError::Unavailable { reason } = error {
            self.last_failure = Some(RecoveryFailure {
                failed_at: tokio::time::Instant::now(),
                reason: reason.clone(),
            });
        }
    }

    fn complete(&mut self, result: &RecoveryResult) {
        self.active = None;
        match result {
            Ok(()) => self.last_failure = None,
            Err(error) => self.record(error),
        }
    }
}

#[derive(Default)]
struct TunnelLifecycle {
    state: tokio::sync::Mutex<RecoveryState>,
    operation: tokio::sync::Mutex<()>,
}

static TUNNEL_LIFECYCLES: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Weak<TunnelLifecycle>>>,
> = std::sync::OnceLock::new();

fn lifecycle_for_container(container_name: &str) -> std::sync::Arc<TunnelLifecycle> {
    let registry =
        TUNNEL_LIFECYCLES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut registry = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(lifecycle) = registry
        .get(container_name)
        .and_then(std::sync::Weak::upgrade)
    {
        return lifecycle;
    }
    let lifecycle = std::sync::Arc::new(TunnelLifecycle::default());
    registry.insert(
        container_name.to_string(),
        std::sync::Arc::downgrade(&lifecycle),
    );
    lifecycle
}

#[derive(Clone)]
pub struct TunnelSidecar {
    runtime: ContainerRuntime,
    bin: &'static str,
    cfg: TunnelSidecarConfig,
    base_dir: PathBuf,
    runtime_dir: Option<PathBuf>,
    container_name: String,
    instance_id: String,
    lifecycle: std::sync::Arc<TunnelLifecycle>,
    #[cfg(test)]
    test_runtime: Option<crate::sandbox::cli_runner::TestRuntime>,
}

impl TunnelSidecar {
    pub fn new(runtime: ContainerRuntime, cfg: TunnelSidecarConfig) -> Self {
        Self::for_instance(runtime, cfg, &crate::bootstrap::lunarwing_base_dir())
    }

    pub fn for_instance(
        runtime: ContainerRuntime,
        cfg: TunnelSidecarConfig,
        base_dir: &Path,
    ) -> Self {
        Self::for_instance_with_optional_runtime_dir(runtime, cfg, base_dir, None)
    }

    pub fn for_instance_with_runtime_dir(
        runtime: ContainerRuntime,
        cfg: TunnelSidecarConfig,
        base_dir: &Path,
        runtime_dir: &Path,
    ) -> Self {
        Self::for_instance_with_optional_runtime_dir(
            runtime,
            cfg,
            base_dir,
            Some(runtime_dir.to_path_buf()),
        )
    }

    fn for_instance_with_optional_runtime_dir(
        runtime: ContainerRuntime,
        cfg: TunnelSidecarConfig,
        base_dir: &Path,
        runtime_dir: Option<PathBuf>,
    ) -> Self {
        let container_name = container_name_for_instance(base_dir);
        let lifecycle = lifecycle_for_container(&container_name);
        Self {
            runtime,
            bin: runtime.binary(),
            cfg,
            base_dir: base_dir.to_path_buf(),
            runtime_dir,
            container_name,
            instance_id: instance_id_for(base_dir),
            lifecycle,
            #[cfg(test)]
            test_runtime: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_runtime(
        mut self,
        runtime: crate::sandbox::cli_runner::TestRuntime,
    ) -> Self {
        self.test_runtime = Some(runtime);
        self
    }

    pub fn container_name(&self) -> &str {
        &self.container_name
    }

    fn runtime_dir(&self) -> Result<PathBuf> {
        match &self.runtime_dir {
            Some(dir) => Ok(dir.clone()),
            None => secret_dir().map_err(|reason| self.unavailable(&reason)),
        }
    }

    fn resolv_conf_path(&self) -> Result<Option<PathBuf>> {
        if self.cfg.dns.is_empty() {
            return Ok(None);
        }
        let dir = self.runtime_dir()?;
        Ok(Some(
            dir.join(format!("{}.resolv.conf", self.container_name)),
        ))
    }

    /// Materialize the per-instance resolver file joined containers mount
    /// read-only because runtimes reject `--dns` with `--network container`.
    pub fn tunnel_resolv_conf(&self) -> Result<Option<PathBuf>> {
        let Some(path) = self.resolv_conf_path()? else {
            return Ok(None);
        };
        let contents = std::iter::once("# lunarwing-tunnel-dns\n".to_string())
            .chain(
                self.cfg
                    .dns
                    .iter()
                    .map(|resolver| format!("nameserver {resolver}\n")),
            )
            .collect::<String>();
        let temp_path = path.with_file_name(format!(
            ".{}.{}",
            path.file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default(),
            uuid::Uuid::new_v4()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o644);
        }
        let mut file = options
            .open(&temp_path)
            .map_err(|_| self.unavailable("could not create the tunnel resolver file"))?;
        if file
            .write_all(contents.as_bytes())
            .and_then(|()| file.sync_all())
            .is_err()
        {
            let _ = std::fs::remove_file(&temp_path);
            return Err(self.unavailable("could not write the tunnel resolver file"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o644)).is_err()
            {
                let _ = std::fs::remove_file(&temp_path);
                return Err(self.unavailable("could not set tunnel resolver permissions"));
            }
        }
        std::fs::rename(&temp_path, &path)
            .map_err(|_| self.unavailable("could not install the tunnel resolver file"))?;
        Ok(Some(path))
    }

    fn netbird_state_volume(&self) -> Result<String> {
        let salt = read_or_create_identity_salt(&self.base_dir).map_err(|_| {
            self.unavailable("could not read or create the persistent identity salt")
        })?;
        Ok(self.cfg.netbird_state_volume(&self.instance_id, &salt))
    }

    fn netbird_state_volume_prefix(&self) -> String {
        format!("{SANDBOX_TUNNEL_CONTAINER}-{}-netbird-", self.instance_id)
    }

    /// Start the sidecar, or briefly re-probe a running one that carries our
    /// config hash. A fresh provider retains the full configured start budget.
    pub async fn ensure_started(&self) -> Result<()> {
        let timeout = self.cfg.startup_timeout;
        self.start_recovery(RUN_RECOVERY_REPROBE_TIMEOUT, timeout, None, true)
            .await
    }

    /// Bounded recovery for a command path. The sidecar owns the attempt, so a
    /// cancelled caller cannot cancel its cleanup or lose the failure back-off.
    pub async fn recover_for_run(&self) -> Result<()> {
        let provider_timeout = self.cfg.startup_timeout.min(RUN_PROVIDER_READY_TIMEOUT);
        self.start_recovery(
            RUN_RECOVERY_REPROBE_TIMEOUT,
            provider_timeout,
            Some(RUN_RECOVERY_BUDGET),
            false,
        )
        .await
    }

    /// Bound the initial health check, any recovery, and the final verification
    /// together so image and runtime probes cannot add a second budget.
    pub async fn container_for_run(&self) -> Result<String> {
        let attempt = async {
            match self.ensure_healthy().await {
                Ok(container_id) => Ok(container_id),
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "Sandbox tunnel sidecar unhealthy; using bounded recovery before this run"
                    );
                    self.recover_for_run().await?;
                    self.ensure_healthy().await
                }
            }
        };
        match tokio::time::timeout(RUN_RECOVERY_BUDGET, attempt).await {
            Ok(result) => result,
            Err(_) => {
                let error = RecoveryError::Unavailable {
                    reason: format!(
                        "{} sidecar: health check and recovery exceeded the 60s per-run budget during image, runtime, provider, or firewall probes",
                        self.cfg.provider.as_str()
                    ),
                };
                self.lifecycle.state.lock().await.record(&error);
                Err(error.into_sandbox())
            }
        }
    }

    async fn start_recovery(
        &self,
        reprobe_timeout: Duration,
        provider_timeout: Duration,
        budget: Option<Duration>,
        restart_stopped: bool,
    ) -> Result<()> {
        self.cfg
            .validate()
            .map_err(|reason| SandboxError::Config { reason })?;
        let receiver = {
            let mut lifecycle = self.lifecycle.state.lock().await;
            if lifecycle.stopping && !restart_stopped {
                return Err(self.unavailable("sidecar is stopped"));
            }
            if let Some(reason) = lifecycle.cached_reason(tokio::time::Instant::now()) {
                return Err(self.unavailable(&reason));
            }
            if let Some(active) = &lifecycle.active {
                active.clone()
            } else {
                lifecycle.stopping = false;
                lifecycle.last_failure = None;
                let (sender, receiver) = tokio::sync::watch::channel(None);
                lifecycle.active = Some(receiver.clone());
                self.spawn_recovery(sender, reprobe_timeout, provider_timeout, budget);
                receiver
            }
        };
        self.await_recovery(receiver).await
    }

    fn spawn_recovery(
        &self,
        sender: tokio::sync::watch::Sender<RecoveryCompletion>,
        reprobe_timeout: Duration,
        provider_timeout: Duration,
        budget: Option<Duration>,
    ) {
        if self.cfg.auto_pull_image && !self.cfg.image().contains("@sha256:") {
            tracing::warn!(
                provider = self.cfg.provider.as_str(),
                "Sandbox tunnel image is not digest-pinned and auto-pull is enabled; pin image@sha256:<digest>"
            );
        }
        let sidecar = self.clone();
        drop(tokio::spawn(async move {
            // Keep the operation lock through completion. A cancellation cleanup
            // queued by the dropped attempt is therefore ahead of the next start
            // in this fair mutex's queue, never behind it.
            let _operation = sidecar.lifecycle.operation.lock().await;
            let result = sidecar
                .perform_recovery(reprobe_timeout, provider_timeout, budget)
                .await
                .map_err(|error| RecoveryError::from_sandbox(&error));
            sidecar.lifecycle.state.lock().await.complete(&result);
            sender.send_replace(Some(result));
        }));
    }

    async fn perform_recovery(
        &self,
        reprobe_timeout: Duration,
        provider_timeout: Duration,
        budget: Option<Duration>,
    ) -> Result<()> {
        if self.recovery_stop_requested().await {
            return Err(self.unavailable("sidecar recovery interrupted by shutdown"));
        }
        let attempt = self.start_or_adopt(reprobe_timeout, provider_timeout);
        let result = match budget {
            Some(budget) => match tokio::time::timeout(budget, attempt).await {
                Ok(result) => result,
                Err(_) => Err(self.unavailable(&format!(
                    "sidecar recovery exceeded the {}s per-run budget during image inspection/pull, runtime start, provider health, or firewall verification",
                    budget.as_secs()
                ))),
            },
            None => attempt.await,
        };
        if self.recovery_stop_requested().await {
            self.remove_unlocked().await;
            return Err(self.unavailable("sidecar recovery interrupted by shutdown"));
        }
        result
    }

    pub(crate) async fn recovery_stop_requested(&self) -> bool {
        self.lifecycle.state.lock().await.stopping
    }

    async fn await_recovery(
        &self,
        mut receiver: tokio::sync::watch::Receiver<RecoveryCompletion>,
    ) -> Result<()> {
        let completion = receiver
            .wait_for(Option::is_some)
            .await
            .ok()
            .and_then(|result| result.as_ref().cloned());
        match completion {
            Some(Ok(())) => Ok(()),
            Some(Err(error)) => Err(error.into_sandbox()),
            None => {
                let error = RecoveryError::from_sandbox(&self.unavailable(
                    "sidecar recovery task ended without a result; a later command may retry",
                ));
                let mut lifecycle = self.lifecycle.state.lock().await;
                if lifecycle
                    .active
                    .as_ref()
                    .is_some_and(|active| active.same_channel(&receiver))
                {
                    lifecycle.complete(&error);
                }
                Err(error.into_sandbox())
            }
        }
    }

    async fn start_or_adopt(
        &self,
        reprobe_timeout: Duration,
        provider_timeout: Duration,
    ) -> Result<()> {
        let _ = self.tunnel_resolv_conf()?;
        let hash = self.current_hash().await?;
        if self.matching_container_id(&hash).await.is_some() {
            match self.wait_healthy(reprobe_timeout).await {
                Ok(()) => return Ok(()),
                Err(error) => tracing::warn!(
                    container = %self.container_name,
                    error = %error,
                    "Sandbox tunnel sidecar failed its health gate; recreating"
                ),
            }
        }
        self.remove_unlocked().await;
        self.start_fresh(&hash, provider_timeout).await
    }

    /// Recheck the transport and both firewall chains before every run, then
    /// return the exact container ID whose namespace was verified.
    pub async fn ensure_healthy(&self) -> Result<String> {
        let (cached, recovering, stopping) = {
            let lifecycle = self.lifecycle.state.lock().await;
            (
                lifecycle.cached_reason(tokio::time::Instant::now()),
                lifecycle.active.is_some(),
                lifecycle.stopping,
            )
        };
        if stopping {
            return Err(self.unavailable("sidecar is stopped"));
        }
        if let Some(reason) = cached {
            return Err(self.unavailable(&reason));
        }
        if recovering {
            return Err(self.unavailable("sidecar recovery is in progress"));
        }
        let _ = self.tunnel_resolv_conf()?;
        let hash = self.current_hash().await?;
        let Some(container_id) = self.matching_container_id(&hash).await else {
            return Err(self.unavailable("sidecar is not running with the current configuration"));
        };
        self.provider_health().await?;
        self.check_egress_firewall().await?;
        Ok(container_id)
    }

    async fn start_fresh(&self, hash: &str, provider_timeout: Duration) -> Result<()> {
        let state_volume = match self.cfg.provider {
            TunnelProvider::Netbird => {
                let volume = self.netbird_state_volume()?;
                self.prune_stale_netbird_volumes(&volume).await;
                Some(volume)
            }
            TunnelProvider::Wireguard => None,
        };
        let env_file = self.write_secret_env_file()?;
        let args = build_sidecar_args(
            &self.cfg,
            env_file.as_ref().map(SecretEnvFile::as_path),
            &self.container_name,
            &self.instance_id,
            hash,
            state_volume.as_deref(),
        );
        let mut cleanup = SidecarStartCleanup::new(self.clone());
        let started = match self
            .start_detached(&args, env_file, hash, provider_timeout)
            .await
        {
            Ok(started) => started,
            Err(error) => return Err(self.abort_start(&mut cleanup, error).await),
        };
        if !started {
            cleanup.disarm();
            return Ok(());
        }
        if self.recovery_stop_requested().await {
            let error = self.unavailable("sidecar start interrupted by shutdown");
            return Err(self.abort_start(&mut cleanup, error).await);
        }
        tracing::info!(
            container = %self.container_name,
            provider = self.cfg.provider.as_str(),
            "Sandbox tunnel sidecar started"
        );
        // Provider startup finishes before the firewall goes in so bootstrap
        // DNS and endpoint connections can use the still-open uplink.
        if let Err(error) = self.wait_provider_ready(provider_timeout).await {
            return Err(self.abort_start(&mut cleanup, error).await);
        }
        if let Err(error) = self.install_egress_firewall().await {
            return Err(self.abort_start(&mut cleanup, error).await);
        }
        if self.cfg.wireguard_canary.is_some()
            && let Err(error) = self.provider_health().await
        {
            let error = self.error_with_context(error, "after the egress firewall was installed");
            return Err(self.abort_start(&mut cleanup, error).await);
        }
        cleanup.disarm();
        tracing::info!(
            container = %self.container_name,
            provider = self.cfg.provider.as_str(),
            "Sandbox tunnel sidecar healthy; inbound denied, uplink limited to tunnel traffic"
        );
        Ok(())
    }

    async fn start_detached(
        &self,
        args: &[String],
        env_file: Option<SecretEnvFile>,
        hash: &str,
        provider_timeout: Duration,
    ) -> Result<bool> {
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let detached_unit = self.helper_unit();
        let start_timeout = self.cfg.startup_timeout.min(provider_timeout);
        let started = tokio::time::timeout(
            start_timeout,
            self.runtime_detached(&arg_refs, &detached_unit),
        )
        .await;
        drop(env_file);
        let output = match started {
            Ok(Ok(output)) => output,
            Ok(Err(_)) => return Err(self.unavailable("failed to execute the container runtime")),
            Err(_) => {
                return Err(self.unavailable(&format!(
                    "sidecar start exceeded the {}s runtime start budget",
                    start_timeout.as_secs()
                )));
            }
        };
        if output.status.success() {
            return Ok(true);
        }
        if self.matching_container_id(hash).await.is_some() {
            self.wait_healthy(provider_timeout).await?;
            return Ok(false);
        }
        if crate::tools::process::unit_name_in_use(&String::from_utf8_lossy(&output.stderr)) {
            return Err(self.unavailable(&format!(
                "helper unit {detached_unit}.service of a previous sidecar was still loaded, so the sidecar could not start; retry, and if it persists run `systemctl --user kill {detached_unit}.service`"
            )));
        }
        Err(self.unavailable(
            "container runtime rejected sidecar start; verify the image and host prerequisites",
        ))
    }

    async fn abort_start(
        &self,
        cleanup: &mut SidecarStartCleanup,
        error: SandboxError,
    ) -> SandboxError {
        self.remove_unlocked().await;
        cleanup.disarm();
        error
    }

    async fn wait_provider_ready(&self, timeout: Duration) -> Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let error = match self.provider_health().await {
                Ok(()) => return Ok(()),
                Err(error) => error,
            };
            if tokio::time::Instant::now() >= deadline {
                let context = if timeout < self.cfg.startup_timeout {
                    format!(
                        "provider remained unhealthy for the {}s per-run cap; configured startup timeout is {}s; a provider that needs longer after recreation requires a daemon restart",
                        timeout.as_secs(),
                        self.cfg.startup_timeout.as_secs()
                    )
                } else {
                    format!("provider remained unhealthy for {}s", timeout.as_secs())
                };
                return Err(self.error_with_context(error, &context));
            }
            tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
        }
    }

    async fn wait_healthy(&self, timeout: Duration) -> Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let error = match self.provider_health().await {
                Ok(()) => {
                    return match self.check_egress_firewall().await {
                        Ok(()) => Ok(()),
                        Err(error) => {
                            self.remove_unlocked().await;
                            Err(error)
                        }
                    };
                }
                Err(error) => error,
            };
            if tokio::time::Instant::now() >= deadline {
                self.remove_unlocked().await;
                return Err(self.error_with_context(
                    error,
                    &format!("sidecar remained unhealthy for {}s", timeout.as_secs()),
                ));
            }
            tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
        }
    }

    fn error_with_context(&self, error: SandboxError, context: &str) -> SandboxError {
        match error {
            SandboxError::TunnelUnavailable { reason } => {
                self.unavailable(&format!("{reason}; {context}"))
            }
            other => other,
        }
    }

    async fn matching_container_id(&self, hash: &str) -> Option<String> {
        let (id, running, label, instance) = self.inspect().await?;
        (running
            && label.as_deref() == Some(hash)
            && instance.as_deref() == Some(&self.instance_id))
        .then_some(id)
    }

    async fn inspect(&self) -> Option<(String, bool, Option<String>, Option<String>)> {
        let format = format!(
            "{{{{.Id}}}}|{{{{.State.Running}}}}|{{{{index .Config.Labels \"{SANDBOX_TUNNEL_HASH_LABEL}\"}}}}|{{{{index .Config.Labels \"{SANDBOX_TUNNEL_INSTANCE_LABEL}\"}}}}"
        );
        let output = self
            .output_probe(&["inspect", "--format", &format, &self.container_name])
            .await
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut parts = text.trim().splitn(4, '|');
        let id = parts.next()?.trim();
        parse_image_id(id)?;
        let running = parts.next()? == "true";
        let label = |value: Option<&str>| {
            value
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        let hash = label(parts.next());
        let instance = label(parts.next());
        Some((id.to_string(), running, hash, instance))
    }

    fn wireguard_has_peers(&self) -> Option<bool> {
        let path = self.cfg.wireguard_config.as_deref()?;
        let content = std::fs::read_to_string(path).ok()?;
        Some(parse_wireguard_config(&content).ok()?.has_peers)
    }

    async fn provider_health(&self) -> Result<()> {
        let has_peers = if self.cfg.provider == TunnelProvider::Wireguard {
            self.wireguard_has_peers().ok_or_else(|| {
                self.unavailable("WireGuard config could not be parsed for the health probe")
            })?
        } else {
            false
        };
        let args = build_health_args_for(
            self.cfg.provider,
            &self.container_name,
            has_peers,
            self.cfg.wireguard_canary,
        );
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        if self.cfg.provider == TunnelProvider::Wireguard
            && has_peers
            && self.cfg.wireguard_canary.is_none()
        {
            let output = self
                .output_probe(&refs)
                .await
                .map_err(|_| self.unavailable("WireGuard handshake probe could not run"))?;
            if !output.status.success() {
                return Err(self.unavailable("WireGuard handshake probe command failed"));
            }
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            return wireguard_handshake_status(&String::from_utf8_lossy(&output.stdout), now)
                .map_err(|reason| self.unavailable(&reason));
        }
        if self.status_probe(&refs).await.is_some_and(|s| s.success()) {
            return Ok(());
        }
        let reason = match self.cfg.provider {
            TunnelProvider::Netbird => "netbird status --check startup probe failed".to_string(),
            TunnelProvider::Wireguard if self.cfg.wireguard_canary.is_some() => format!(
                "WireGuard canary probe to {} failed",
                self.cfg
                    .wireguard_canary
                    .map(|address| address.to_string())
                    .unwrap_or_else(|| "the configured address".to_string())
            ),
            TunnelProvider::Wireguard => "WireGuard wg0 interface probe failed".to_string(),
        };
        Err(self.unavailable(&reason))
    }

    /// The uplink allowance for the running sidecar. `set_wireguard_mark`
    /// assigns [`LUNARWING_TUNNEL_FWMARK`] to a WireGuard device that has none
    /// (install time); verification only reads.
    async fn egress_policy(&self, set_wireguard_mark: bool) -> Result<EgressPolicy> {
        let tunnel_mark = match self.cfg.provider {
            TunnelProvider::Netbird => NETBIRD_CONTROL_PLANE_MARK,
            TunnelProvider::Wireguard => self.wireguard_fwmark(set_wireguard_mark).await?,
        };
        let cat = exec_args(&["cat", "/etc/resolv.conf"], &self.container_name);
        let cat_refs: Vec<&str> = cat.iter().map(String::as_str).collect();
        let resolv = self
            .output_probe(&cat_refs)
            .await
            .map_err(|_| self.unavailable("could not read the sidecar's resolver list"))?;
        if !resolv.status.success() {
            return Err(self.unavailable("could not read the sidecar's resolver list"));
        }
        Ok(EgressPolicy {
            tunnel_mark,
            resolvers: self
                .cfg
                .uplink_resolvers(&String::from_utf8_lossy(&resolv.stdout)),
        })
    }

    async fn wireguard_fwmark(&self, set_if_off: bool) -> Result<u32> {
        let show = exec_args(&["wg", "show", "wg0", "fwmark"], &self.container_name);
        let show_refs: Vec<&str> = show.iter().map(String::as_str).collect();
        let output = self
            .output_probe(&show_refs)
            .await
            .map_err(|_| self.unavailable("could not read the WireGuard fwmark"))?;
        if !output.status.success() {
            return Err(self.unavailable("could not read the WireGuard fwmark"));
        }
        if let Some(mark) = parse_wireguard_fwmark(&String::from_utf8_lossy(&output.stdout)) {
            return Ok(mark);
        }
        if !set_if_off {
            return Err(self.unavailable("WireGuard device carries no fwmark"));
        }
        let mark = format!("0x{LUNARWING_TUNNEL_FWMARK:x}");
        let set = exec_args(&["wg", "set", "wg0", "fwmark", &mark], &self.container_name);
        let set_refs: Vec<&str> = set.iter().map(String::as_str).collect();
        match self.status_probe(&set_refs).await {
            Some(status) if status.success() => Ok(LUNARWING_TUNNEL_FWMARK),
            _ => Err(self.unavailable("could not set the WireGuard fwmark")),
        }
    }

    async fn install_egress_firewall(&self) -> Result<()> {
        let policy = self.egress_policy(true).await?;
        if self.verify_egress_firewall(&policy).await.is_ok() {
            return Ok(());
        }
        let (args, ruleset) = firewall_install_command(&policy, &self.container_name);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = self
            .output_probe_with_stdin(&refs, Some(ruleset.as_bytes()))
            .await
            .map_err(|_| self.unavailable("failed to execute nftables in the tunnel sidecar"))?;
        if !output.status.success() {
            return Err(self.unavailable(
                "egress firewall setup failed; the sidecar image must provide nftables",
            ));
        }
        self.verify_egress_firewall(&policy).await
    }

    async fn check_egress_firewall(&self) -> Result<()> {
        let policy = self.egress_policy(false).await?;
        self.verify_egress_firewall(&policy).await
    }

    async fn verify_egress_firewall(&self, policy: &EgressPolicy) -> Result<()> {
        let args = firewall_list_args(&self.container_name);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = self
            .output_probe(&refs)
            .await
            .map_err(|_| self.unavailable("egress firewall verification could not run"))?;
        if !output.status.success() {
            return Err(self
                .unavailable("egress firewall is absent or altered; refusing sandbox execution"));
        }
        let listing = String::from_utf8_lossy(&output.stdout);
        firewall_matches(&listing, policy).map_err(|reason| {
            tracing::warn!(
                container = %self.container_name,
                reason = %reason,
                "Sandbox tunnel firewall does not match the expected policy"
            );
            self.unavailable("egress firewall is absent or altered; refusing sandbox execution")
        })
    }

    async fn ensure_image_available(&self) -> Result<()> {
        if self
            .status_probe(&["image", "inspect", self.cfg.image()])
            .await
            .is_some_and(|status| status.success())
        {
            return Ok(());
        }
        if !self.cfg.auto_pull_image {
            return Err(self.unavailable(
                "sidecar image is not present locally and SANDBOX_AUTO_PULL is false",
            ));
        }
        let pulled = tokio::time::timeout(
            self.cfg.startup_timeout,
            self.runtime_output(&["pull", self.cfg.image()], None),
        )
        .await;
        match pulled {
            Ok(Ok(output)) if output.status.success() => Ok(()),
            Ok(Ok(_)) => Err(self.unavailable("could not pull the sidecar image")),
            Ok(Err(_)) => Err(self.unavailable("failed to execute the image pull")),
            Err(_) => Err(self.unavailable("sidecar image pull exceeded the startup timeout")),
        }
    }

    async fn resolve_image_id(&self) -> Result<String> {
        let output = self
            .output_probe(&["image", "inspect", "--format", "{{.Id}}", self.cfg.image()])
            .await
            .map_err(|_| self.unavailable("could not inspect the sidecar image"))?;
        if !output.status.success() {
            return Err(self.unavailable("could not inspect the sidecar image"));
        }
        parse_image_id(&String::from_utf8_lossy(&output.stdout))
            .map(str::to_string)
            .ok_or_else(|| self.unavailable("the runtime returned an invalid sidecar image ID"))
    }

    /// Remove NetBird state volumes left by earlier configurations (a rotated
    /// key enrols a new peer; the old identity must not linger on disk).
    async fn prune_stale_netbird_volumes(&self, keep: &str) {
        let Ok(output) = self
            .output_probe(&["volume", "ls", "--format", "{{.Name}}"])
            .await
        else {
            return;
        };
        if !output.status.success() {
            return;
        }
        let listing = String::from_utf8_lossy(&output.stdout);
        let prefix = self.netbird_state_volume_prefix();
        for name in listing.lines().map(str::trim) {
            if name.starts_with(&prefix) && name != keep {
                let _ = self.status_probe(&["volume", "rm", name]).await;
            }
        }
    }

    async fn runtime_status(&self, args: &[&str]) -> Option<std::process::ExitStatus> {
        #[cfg(test)]
        if let Some(runtime) = &self.test_runtime {
            return runtime
                .output(args, None)
                .await
                .ok()
                .map(|output| output.status);
        }
        crate::sandbox::cli_runner::run_status_isolated(self.bin, args).await
    }

    async fn runtime_output(
        &self,
        args: &[&str],
        stdin: Option<&[u8]>,
    ) -> std::io::Result<std::process::Output> {
        #[cfg(test)]
        if let Some(runtime) = &self.test_runtime {
            return runtime.output(args, stdin).await;
        }
        match stdin {
            Some(input) => {
                crate::sandbox::cli_runner::output_isolated_with_stdin(self.bin, args, input).await
            }
            None => crate::sandbox::cli_runner::output_isolated(self.bin, args).await,
        }
    }

    async fn runtime_detached(
        &self,
        args: &[&str],
        unit_name: &str,
    ) -> std::io::Result<std::process::Output> {
        #[cfg(test)]
        if let Some(runtime) = &self.test_runtime {
            return runtime.output(args, None).await;
        }
        crate::sandbox::cli_runner::output_detached_isolated(self.bin, args, unit_name).await
    }

    async fn status_probe(&self, args: &[&str]) -> Option<std::process::ExitStatus> {
        tokio::time::timeout(PROBE_TIMEOUT, self.runtime_status(args))
            .await
            .ok()
            .flatten()
    }

    async fn output_probe(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        self.output_probe_with_stdin(args, None).await
    }

    async fn output_probe_with_stdin(
        &self,
        args: &[&str],
        stdin: Option<&[u8]>,
    ) -> std::io::Result<std::process::Output> {
        tokio::time::timeout(PROBE_TIMEOUT, self.runtime_output(args, stdin))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "probe timed out"))?
    }

    fn unavailable(&self, detail: &str) -> SandboxError {
        SandboxError::TunnelUnavailable {
            reason: format!("{} sidecar: {detail}", self.cfg.provider.as_str()),
        }
    }

    async fn current_hash(&self) -> Result<String> {
        self.ensure_image_available().await?;
        let image_id = self.resolve_image_id().await?;
        let dir = self.runtime_dir()?;
        let salt = read_or_create_label_salt(&dir)
            .map_err(|_| self.unavailable("could not read or create the label salt"))?;
        Ok(self.cfg.config_hash(&salt, &image_id))
    }

    /// Kill-and-remove only a sidecar carrying this instance's ownership label.
    pub async fn remove(&self) {
        let _operation = self.lifecycle.operation.lock().await;
        self.remove_unlocked().await;
    }

    async fn remove_unlocked(&self) {
        let _ = tokio::time::timeout(CLEANUP_TIMEOUT, self.remove_with_grace(0)).await;
        self.release_network_helper().await;
    }

    /// Stop the sidecar gracefully, then remove it if this instance owns it.
    pub async fn stop(&self) {
        let active = {
            let mut lifecycle = self.lifecycle.state.lock().await;
            lifecycle.stopping = true;
            lifecycle.active.clone()
        };
        if let Some(receiver) = active {
            let _ = tokio::time::timeout(CLEANUP_TIMEOUT, self.await_recovery(receiver)).await;
        }
        match tokio::time::timeout(CLEANUP_TIMEOUT, self.lifecycle.operation.lock()).await {
            Ok(_operation) => self.stop_unlocked().await,
            Err(_) => tracing::warn!(
                container = %self.container_name,
                "Sidecar recovery exceeded the shutdown wait; its stop check will remove any late start"
            ),
        }
    }

    async fn stop_unlocked(&self) {
        if tokio::time::timeout(CLEANUP_TIMEOUT, self.remove_with_grace(10))
            .await
            .ok()
            == Some(true)
        {
            tracing::info!(
                container = %self.container_name,
                "Sandbox tunnel sidecar stopped"
            );
        }
        self.release_network_helper().await;
        if let Ok(Some(path)) = self.resolv_conf_path() {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Transient unit of the detached `run -d` client. In systemd service
    /// mode the runtime's network helpers (pasta, conmon) stay in its cgroup.
    fn helper_unit(&self) -> String {
        format!("lunarwing-tunnel-{}", self.instance_id)
    }

    /// Free the helper unit's name once the sidecar container is gone.
    ///
    /// The unit stays loaded while a helper lives, and the next start reuses
    /// its name. The helpers must keep running for as long as the container
    /// does, so nothing is signalled unless the runtime positively reports
    /// that no container has the sidecar's name.
    async fn release_network_helper(&self) {
        let spawner = crate::tools::process::workload_spawner().await;
        if !spawner.uses_systemd_services() {
            return;
        }
        if self.container_absent().await != Some(true) {
            tracing::debug!(
                container = %self.container_name,
                "Sidecar container is present or its state is unknown; leaving its helper unit alone"
            );
            return;
        }
        let unit = format!("{}.service", self.helper_unit());
        let released = tokio::time::timeout(
            CLEANUP_TIMEOUT,
            crate::tools::process::release_detached_workload_unit(&unit, spawner.xdg_runtime_dir()),
        )
        .await
        .unwrap_or(false);
        if !released {
            tracing::warn!(
                unit = %unit,
                "Sandbox tunnel helper unit is still loaded after cleanup; the next sidecar start may fail"
            );
        }
    }

    /// `Some(true)` only when the runtime positively reports that no
    /// container has the sidecar's name; `None` when the probe itself failed.
    async fn container_absent(&self) -> Option<bool> {
        let output = self
            .output_probe(&[
                "container",
                "inspect",
                "--format",
                "{{.Id}}",
                &self.container_name,
            ])
            .await
            .ok()?;
        container_absent_from_inspect(
            output.status.success(),
            &String::from_utf8_lossy(&output.stderr),
        )
    }

    async fn remove_with_grace(&self, grace_secs: u32) -> bool {
        let owned = self.inspect().await.is_some_and(|(_, _, _, instance)| {
            instance.as_deref() == Some(self.instance_id.as_str())
        });
        if !owned {
            tracing::debug!(
                container = %self.container_name,
                "Not removing a sidecar without this instance's ownership label"
            );
            return false;
        }
        for args in removal_commands(self.runtime, grace_secs, &self.container_name) {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            if let Some(status) = self.runtime_status(&refs).await
                && !status.success()
            {
                tracing::debug!(
                    container = %self.container_name,
                    command = refs.join(" "),
                    "Sandbox tunnel sidecar removal command returned non-zero"
                );
            }
        }
        true
    }

    fn write_secret_env_file(&self) -> Result<Option<SecretEnvFile>> {
        if self.cfg.provider != TunnelProvider::Netbird {
            return Ok(None);
        }
        let url = self
            .cfg
            .netbird_management_url
            .as_deref()
            .unwrap_or_default();
        let key = self
            .cfg
            .netbird_setup_key
            .as_ref()
            .map(|key| key.expose_secret())
            .unwrap_or_default();
        crate::tracing_fmt::register_log_secret(key);
        // NB_LOG_FILE=console: the image default also names a file under
        // /var/log, which the read-only rootfs cannot hold. NetBird's own
        // firewall would insert wt0 accepts into LunarWing's exact chains.
        let contents = SecretString::from(format!(
            "NB_MANAGEMENT_URL={url}\nNB_SETUP_KEY={key}\nNB_LOG_FILE=console\nNB_DISABLE_FIREWALL=true\n"
        ));
        let dir = self.runtime_dir()?;
        SecretEnvFile::write(&dir, &contents)
            .map(Some)
            .map_err(|_| self.unavailable("failed to stage the NetBird credential file"))
    }
}

/// Read a failed `container inspect`: only a "no such container" answer
/// proves absence. Any other failure (runtime down, timeout, storage lock)
/// leaves the state unknown.
fn container_absent_from_inspect(success: bool, stderr: &str) -> Option<bool> {
    if success {
        return Some(false);
    }
    stderr
        .to_ascii_lowercase()
        .contains("no such container")
        .then_some(true)
}

/// Runtime-specific removal: podman's `rm` takes `-t`, Docker's does not, so
/// Docker stops first when a grace period is wanted.
fn removal_commands(
    runtime: ContainerRuntime,
    grace_secs: u32,
    container_name: &str,
) -> Vec<Vec<String>> {
    let grace = grace_secs.to_string();
    let to_strings = |args: &[&str]| args.iter().map(|arg| (*arg).to_string()).collect();
    match runtime {
        ContainerRuntime::Podman => vec![to_strings(&[
            "rm",
            "-f",
            "-t",
            grace.as_str(),
            container_name,
        ])],
        ContainerRuntime::Docker => {
            let mut commands = Vec::new();
            if grace_secs > 0 {
                commands.push(to_strings(&["stop", "-t", grace.as_str(), container_name]));
            }
            commands.push(to_strings(&["rm", "-f", container_name]));
            commands
        }
    }
}

/// The private per-user runtime directory for the credential file and the
/// label salt. Fails closed instead of falling back to a shared temp dir.
#[cfg(target_os = "linux")]
fn secret_dir() -> std::result::Result<PathBuf, String> {
    let dir = crate::sandbox::proxy::relay::relay_parent_dir()
        .map_err(|error| format!("no private runtime directory for tunnel credentials: {error}"))?;
    if dir == std::env::temp_dir() {
        return Err(
            "no private runtime directory for tunnel credentials (XDG_RUNTIME_DIR must point \
             at a directory owned by this user)"
                .into(),
        );
    }
    Ok(dir)
}

#[cfg(not(target_os = "linux"))]
fn secret_dir() -> std::result::Result<PathBuf, String> {
    Err("the sandbox tunnel sidecar requires a Linux container runtime".into())
}

fn read_or_create_identity_salt(base_dir: &Path) -> std::io::Result<Vec<u8>> {
    let path = base_dir.join(IDENTITY_SALT_FILE);
    match read_identity_salt(&path) {
        Ok(salt) => Ok(salt),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => create_identity_salt(&path),
        Err(error) => Err(error),
    }
}

fn read_identity_salt(path: &Path) -> std::io::Result<Vec<u8>> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "identity salt must be a regular, non-symlink file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "identity salt must not be accessible by group or other users",
            ));
        }
    }
    let salt = std::fs::read(path)?;
    if salt.len() != IDENTITY_SALT_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "identity salt must contain exactly 32 bytes",
        ));
    }
    Ok(salt)
}

fn create_identity_salt(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut salt = Vec::with_capacity(IDENTITY_SALT_BYTES);
    salt.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    salt.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            let written = (|| -> std::io::Result<()> {
                file.write_all(&salt)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                }
                file.sync_all()
            })();
            if let Err(error) = written {
                drop(file);
                let _ = std::fs::remove_file(path);
                return Err(error);
            }
            Ok(salt)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => read_identity_salt(path),
        Err(error) => Err(error),
    }
}

fn read_or_create_label_salt(dir: &Path) -> std::io::Result<Vec<u8>> {
    let path = dir.join(LABEL_SALT_FILE);
    match std::fs::read(&path) {
        Ok(salt) if !salt.is_empty() => return Ok(salt),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut salt = Vec::with_capacity(32);
    salt.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    salt.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => {
            file.write_all(&salt)?;
            file.sync_all()?;
            Ok(salt)
        }
        // Another daemon instance created it first: use theirs.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => std::fs::read(&path),
        Err(error) => Err(error),
    }
}

struct SidecarStartCleanup {
    sidecar: Option<TunnelSidecar>,
}

impl SidecarStartCleanup {
    fn new(sidecar: TunnelSidecar) -> Self {
        Self {
            sidecar: Some(sidecar),
        }
    }

    fn disarm(&mut self) {
        self.sidecar = None;
    }
}

impl Drop for SidecarStartCleanup {
    fn drop(&mut self) {
        let Some(sidecar) = self.sidecar.take() else {
            return;
        };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => drop(runtime.spawn(async move { sidecar.remove().await })),
            Err(_) => tracing::error!(
                container = %sidecar.container_name,
                "Cancelled tunnel start could not schedule sidecar cleanup without a Tokio runtime"
            ),
        }
    }
}

struct SecretEnvFile {
    path: PathBuf,
}

impl SecretEnvFile {
    fn write(dir: &Path, contents: &SecretString) -> std::io::Result<Self> {
        let path = dir.join(format!(
            "lunarwing-sandbox-tunnel-env-{}",
            uuid::Uuid::new_v4()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        let staged = Self { path };
        file.write_all(contents.expose_secret().as_bytes())?;
        file.sync_all()?;
        Ok(staged)
    }

    fn as_path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SecretEnvFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SALT: &[u8] = b"test-salt";
    const IMAGE_ID: &str = "sha256:image-one";

    fn netbird_cfg() -> TunnelSidecarConfig {
        TunnelSidecarConfig {
            netbird_management_url: Some("https://netbird.example.com".into()),
            netbird_setup_key: Some(SecretString::from("setup-key-1")),
            ..TunnelSidecarConfig::default()
        }
    }

    fn wireguard_cfg(config: Option<PathBuf>) -> TunnelSidecarConfig {
        TunnelSidecarConfig {
            provider: TunnelProvider::Wireguard,
            wireguard_config: config,
            ..TunnelSidecarConfig::default()
        }
    }

    fn policy() -> EgressPolicy {
        EgressPolicy {
            tunnel_mark: LUNARWING_TUNNEL_FWMARK,
            resolvers: vec!["169.254.1.1".parse().unwrap(), "9.9.9.9".parse().unwrap()],
        }
    }

    /// The listing nft 1.1 prints for the installed policy (named priority,
    /// zero-padded mark), as captured on rootless podman.
    fn listing_for(policy: &EgressPolicy) -> String {
        let mut text = String::from("table inet lunarwing_sandbox_egress {\n\tchain input {\n");
        text.push_str("\t\ttype filter hook input priority filter; policy drop;\n");
        for rule in EgressPolicy::input_rules() {
            text.push_str(&format!("\t\t{rule}\n"));
        }
        text.push_str("\t}\n\n\tchain output {\n");
        text.push_str("\t\ttype filter hook output priority filter; policy drop;\n");
        for rule in policy.output_rules() {
            let printed = rule.replace(
                &format!("0x{:x}", policy.tunnel_mark),
                &format!("0x{:08x}", policy.tunnel_mark),
            );
            text.push_str(&format!("\t\t{printed}\n"));
        }
        text.push_str("\t}\n}\n");
        text
    }

    #[test]
    fn provider_parsing_defaults_and_interfaces() {
        assert_eq!(
            "netbird".parse::<TunnelProvider>().unwrap(),
            TunnelProvider::Netbird
        );
        assert_eq!(
            " WireGuard ".parse::<TunnelProvider>().unwrap(),
            TunnelProvider::Wireguard
        );
        assert_eq!(
            "wg".parse::<TunnelProvider>().unwrap(),
            TunnelProvider::Wireguard
        );
        assert!("openvpn".parse::<TunnelProvider>().is_err());
        assert_eq!(
            TunnelProvider::Netbird.default_image(),
            "docker.io/netbirdio/netbird@sha256:eca5b274c635bfbb04ec6724a0712d1146190dd450eea199eeb83916fdd94965"
        );
        assert_eq!(
            TunnelProvider::Wireguard.default_image(),
            "localhost/lunarwing-tunnel-wireguard:latest"
        );
        assert_eq!(TunnelProvider::Netbird.interface(), "wt0");
        assert_eq!(TunnelProvider::Wireguard.interface(), "wg0");
        assert!(TUNNEL_INTERFACES.contains(&TunnelProvider::Netbird.interface()));
        assert!(TUNNEL_INTERFACES.contains(&TunnelProvider::Wireguard.interface()));
    }

    #[test]
    fn netbird_args_keep_credentials_out_of_argv_and_persist_state() {
        let cfg = netbird_cfg();
        let args = build_sidecar_args(
            &cfg,
            Some(Path::new("/run/user/1000/lw-env")),
            "lunarwing-sandbox-tunnel-test",
            "test-instance",
            "abc123",
            Some("lunarwing-sandbox-tunnel-test-instance-netbird-abc123"),
        );
        let joined = args.join("\n");
        assert!(!joined.contains("setup-key-1"));
        assert!(!joined.contains("netbird.example.com"));
        assert!(joined.contains("--env-file\n/run/user/1000/lw-env"));
        assert!(joined.contains("--device\n/dev/net/tun"));
        assert!(joined.contains(
            "-v\nlunarwing-sandbox-tunnel-test-instance-netbird-abc123:/var/lib/netbird"
        ));
        assert!(!joined.contains("/var/lib/netbird:size="));
        assert!(joined.contains("--cap-drop=ALL"));
        assert!(joined.contains("--cap-add=NET_ADMIN"));
        assert!(joined.contains("--cap-add=NET_RAW"));
        assert!(joined.contains("--cap-add=NET_BIND_SERVICE"));
        assert!(!joined.contains("SYS_ADMIN"));
        assert!(!joined.contains("--privileged"));
        assert!(!joined.contains("--publish"));
        // The image entrypoint runs `netbird up` itself; no dead argument.
        assert_eq!(
            args.last().map(String::as_str),
            Some(
                "docker.io/netbirdio/netbird@sha256:eca5b274c635bfbb04ec6724a0712d1146190dd450eea199eeb83916fdd94965"
            )
        );
    }

    #[test]
    fn wireguard_args_mount_only_the_owner_only_config() {
        let args = build_sidecar_args(
            &wireguard_cfg(Some(PathBuf::from("/etc/lunarwing/wg0.conf"))),
            None,
            "lunarwing-sandbox-tunnel-test",
            "test-instance",
            "abc123",
            None,
        );
        let joined = args.join(" ");
        assert!(joined.contains("/etc/lunarwing/wg0.conf:/etc/wireguard/wg0.conf:ro,Z"));
        assert!(!joined.contains(":ro,z"));
        assert!(joined.contains("wg-quick up /etc/wireguard/wg0.conf"));
        assert!(joined.contains("exec sleep 2147483647"));
        assert!(!joined.contains("sleep infinity"));
        assert!(!joined.contains("--device"));
        assert!(!joined.contains("--env-file"));
        assert!(!joined.contains("--network host"));
        assert!(!joined.contains("/var/lib/netbird"));
    }

    #[test]
    fn sidecar_preserves_hardening_and_separate_label_namespace() {
        let cfg = netbird_cfg();
        let joined = build_sidecar_args(
            &cfg,
            None,
            "lunarwing-sandbox-tunnel-test",
            "test-instance",
            "deadbeef",
            None,
        )
        .join(" ");
        for flag in [
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--read-only",
            "--pids-limit 64",
            "--stop-timeout 10",
            "net.ipv4.ip_forward=0",
            "net.ipv6.conf.all.forwarding=0",
            "net.ipv4.conf.all.src_valid_mark=1",
        ] {
            assert!(joined.contains(flag), "missing {flag}: {joined}");
        }
        assert!(joined.contains(&format!("{SANDBOX_TUNNEL_CONTAINER_LABEL}=netbird")));
        assert!(joined.contains(&format!("{SANDBOX_TUNNEL_INSTANCE_LABEL}=test-instance")));
        assert!(joined.contains(&format!("{SANDBOX_TUNNEL_HASH_LABEL}=deadbeef")));
        assert!(!joined.contains("lunarwing.sandbox="));
    }

    #[test]
    fn container_names_and_netbird_volumes_are_instance_scoped() {
        let first = container_name_for_instance(Path::new("/srv/one/state"));
        let second = container_name_for_instance(Path::new("/srv/two/state"));
        assert_ne!(first, second);
        assert!(first.starts_with("lunarwing-sandbox-tunnel-"));
        assert_eq!(
            first,
            container_name_for_instance(Path::new("/srv/one/state"))
        );
        let cfg = netbird_cfg();
        let first_volume = cfg.netbird_state_volume("first-instance", SALT);
        let second_volume = cfg.netbird_state_volume("second-instance", SALT);
        assert_ne!(first_volume, second_volume);
        assert!(first_volume.starts_with("lunarwing-sandbox-tunnel-first-instance-netbird-"));
    }

    #[test]
    fn config_hash_is_salted_and_covers_rotation_image_and_wireguard_contents() {
        let base = netbird_cfg();
        let rotated = TunnelSidecarConfig {
            netbird_setup_key: Some(SecretString::from("setup-key-2")),
            ..base.clone()
        };
        assert_ne!(
            base.config_hash(SALT, IMAGE_ID),
            rotated.config_hash(SALT, IMAGE_ID)
        );
        assert_eq!(
            base.config_hash(SALT, IMAGE_ID),
            netbird_cfg().config_hash(SALT, IMAGE_ID)
        );
        assert_ne!(
            base.config_hash(SALT, IMAGE_ID),
            base.config_hash(b"other-salt", IMAGE_ID)
        );
        assert_ne!(
            base.config_hash(SALT, IMAGE_ID),
            base.config_hash(SALT, "sha256:image-two")
        );
        // The published label is not a plain digest of the key material.
        let plain = format!("{:x}", Sha256::digest(b"setup-key-1"));
        assert_ne!(base.config_hash(SALT, IMAGE_ID), plain);

        let dir = std::env::temp_dir().join(format!("lw-tunnel-hash-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wg0.conf");
        std::fs::write(&path, "[Interface]\nPrivateKey = first\n").unwrap();
        let first = wireguard_cfg(Some(path.clone())).config_hash(SALT, IMAGE_ID);
        std::fs::write(&path, "[Interface]\nPrivateKey = second\n").unwrap();
        let second = wireguard_cfg(Some(path)).config_hash(SALT, IMAGE_ID);
        assert_ne!(first, second);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn netbird_state_volume_tracks_only_peer_identity() {
        let cfg = netbird_cfg();
        let volume = cfg.netbird_state_volume("test-instance", SALT);
        assert!(volume.starts_with("lunarwing-sandbox-tunnel-test-instance-netbird-"));
        let runtime_change = TunnelSidecarConfig {
            image: Some("localhost/netbird:rotated".into()),
            dns: vec!["100.100.0.53".parse().unwrap()],
            ..cfg.clone()
        };
        assert_eq!(
            volume,
            runtime_change.netbird_state_volume("test-instance", SALT)
        );
        for identity_change in [
            TunnelSidecarConfig {
                netbird_management_url: Some("https://other.example.com".into()),
                ..cfg.clone()
            },
            TunnelSidecarConfig {
                netbird_setup_key: Some(SecretString::from("setup-key-2")),
                ..cfg.clone()
            },
        ] {
            assert_ne!(
                volume,
                identity_change.netbird_state_volume("test-instance", SALT)
            );
        }
    }

    #[test]
    fn netbird_volume_survives_runtime_directory_replacement() {
        let workdir =
            std::env::temp_dir().join(format!("lw-tunnel-reboot-{}", uuid::Uuid::new_v4()));
        let base_dir = workdir.join("state");
        let first_runtime = workdir.join("runtime-first");
        let second_runtime = workdir.join("runtime-second");
        std::fs::create_dir_all(&base_dir).unwrap();
        std::fs::create_dir_all(&first_runtime).unwrap();
        std::fs::create_dir_all(&second_runtime).unwrap();
        let first = TunnelSidecar::for_instance_with_runtime_dir(
            ContainerRuntime::Podman,
            netbird_cfg(),
            &base_dir,
            &first_runtime,
        );
        let second = TunnelSidecar::for_instance_with_runtime_dir(
            ContainerRuntime::Podman,
            netbird_cfg(),
            &base_dir,
            &second_runtime,
        );
        let volume = first.netbird_state_volume().unwrap();
        assert_eq!(volume, second.netbird_state_volume().unwrap());
        assert!(base_dir.join(IDENTITY_SALT_FILE).is_file());
        assert!(!first_runtime.join(IDENTITY_SALT_FILE).exists());
        assert!(!second_runtime.join(IDENTITY_SALT_FILE).exists());
        std::fs::remove_dir_all(&first_runtime).unwrap();
        assert_eq!(volume, first.netbird_state_volume().unwrap());
        std::fs::remove_dir_all(workdir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn identity_salt_is_owner_only_and_reused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("lw-tunnel-identity-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = read_or_create_identity_salt(&dir).unwrap();
        assert_eq!(first.len(), IDENTITY_SALT_BYTES);
        let mode = std::fs::metadata(dir.join(IDENTITY_SALT_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(read_or_create_identity_salt(&dir).unwrap(), first);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn identity_salt_refuses_symlinks_and_group_access() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir =
            std::env::temp_dir().join(format!("lw-tunnel-identity-bad-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(IDENTITY_SALT_FILE);
        let target = dir.join("identity-target");
        std::fs::write(&target, vec![7; IDENTITY_SALT_BYTES]).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&target, &path).unwrap();
        assert!(read_or_create_identity_salt(&dir).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, vec![8; IDENTITY_SALT_BYTES]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(read_or_create_identity_salt(&dir).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            read_or_create_identity_salt(&dir).unwrap(),
            vec![8; IDENTITY_SALT_BYTES]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn netbird_validation_is_strict_without_leaking_values() {
        assert!(netbird_cfg().validate().is_ok());
        for url in [
            "http://netbird.example.com",
            "https://",
            "https://user@netbird.example.com",
            "https://netbird.example.com?token=secret",
            "https://netbird.example.com#fragment",
        ] {
            let cfg = TunnelSidecarConfig {
                netbird_management_url: Some(url.into()),
                ..netbird_cfg()
            };
            let error = cfg.validate().unwrap_err();
            assert!(error.contains("MANAGEMENT_URL"), "{error}");
            assert!(!error.contains("setup-key-1"));
        }
        let missing = TunnelSidecarConfig {
            netbird_setup_key: None,
            ..netbird_cfg()
        };
        assert!(missing.validate().unwrap_err().contains("SETUP_KEY"));
    }

    #[test]
    fn image_digest_references_and_runtime_ids_are_validated() {
        assert!(netbird_cfg().validate().is_ok());
        let digest = format!("sha256:{}", "a".repeat(64));
        let pinned = TunnelSidecarConfig {
            image: Some(format!("registry.example/netbird@{digest}")),
            ..netbird_cfg()
        };
        assert!(pinned.validate().is_ok());
        assert_eq!(parse_image_id(&digest), Some(digest.as_str()));
        assert_eq!(
            parse_image_id(&"b".repeat(64)),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
        for invalid in [
            "",
            " netbird:latest",
            "netbird@sha256:abc",
            "netbird@sha512:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            let cfg = TunnelSidecarConfig {
                image: Some(invalid.into()),
                ..netbird_cfg()
            };
            assert!(cfg.validate().is_err(), "accepted {invalid:?}");
        }
        assert_eq!(parse_image_id("not-an-image-id"), None);
    }

    #[test]
    fn wireguard_validation_checks_structure_and_permissions() {
        let dir = std::env::temp_dir().join(format!("lw-tunnel-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(wireguard_cfg(None).validate().is_err());
        let config = dir.join("wg0.conf");
        std::fs::write(
            &config,
            "[Peer]\nPublicKey = public\nPrivateKey = wrong-section\n[Interface]\nAddress = 10.0.0.2/32\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let error = wireguard_cfg(Some(config.clone())).validate().unwrap_err();
        assert!(
            error.contains("needs a non-empty PrivateKey in [Interface]"),
            "{error}"
        );
        std::fs::write(
            &config,
            "[Interface]\nPrivateKey = abcdefg\nAddress = 10.0.0.2/32\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(wireguard_cfg(Some(config.clone())).validate().is_err());
            std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(wireguard_cfg(Some(config)).validate().is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn peered_wireguard_requires_keepalive_or_canary() {
        let dir =
            std::env::temp_dir().join(format!("lw-tunnel-keepalive-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("wg0.conf");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&config, "").unwrap();
            std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let validate = |keepalive: Option<&str>, canary: bool| {
            let keepalive = keepalive
                .map(|value| format!("PersistentKeepalive = {value}\n"))
                .unwrap_or_default();
            std::fs::write(
                &config,
                format!(
                    "[Interface]\nPrivateKey = private\n[Peer]\nPublicKey = public\n{keepalive}"
                ),
            )
            .unwrap();
            TunnelSidecarConfig {
                provider: TunnelProvider::Wireguard,
                wireguard_config: Some(config.clone()),
                wireguard_canary: canary.then(|| "10.231.0.1".parse().unwrap()),
                ..TunnelSidecarConfig::default()
            }
            .validate()
        };
        for invalid in [None, Some("0"), Some("61")] {
            let error = validate(invalid, false).unwrap_err();
            assert!(error.contains("PersistentKeepalive"), "{error}");
        }
        for valid in [Some("1"), Some("25 # seconds"), Some("60")] {
            assert!(validate(valid, false).is_ok());
        }
        assert!(validate(None, true).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn wireguard_parser_matches_wg_section_and_keepalive_rules() {
        let interface = "[Interface]\nPrivateKey = private\n";
        let valid = [
            (
                "commented header",
                "[Peer] # office\nPublicKey = public\nPersistentKeepalive = 25\n",
                true,
            ),
            (
                "spaced header",
                "[ Peer ]\nPublicKey = public\nPersistentKeepalive = 25\n",
                true,
            ),
            (
                "crlf and uppercase",
                "[PEER]\r\nPublicKey = public\r\nPersistentKeepalive = 25\r\n",
                true,
            ),
            (
                "second peer",
                "[Peer]\nPublicKey = first\n[Peer]\nPublicKey = second\nPersistentKeepalive = 25\n",
                true,
            ),
            (
                "inline comment",
                "[Peer]\nPublicKey = public\nPersistentKeepalive = 25 # seconds\n",
                true,
            ),
            (
                "whitespace forms 600",
                "[Peer]\nPublicKey = public\nPersistentKeepalive = 6 0 0\n",
                false,
            ),
            (
                "last value disables",
                "[Peer]\nPublicKey = public\nPersistentKeepalive = 25\nPersistentKeepalive = 0\n",
                false,
            ),
            (
                "off",
                "[Peer]\nPublicKey = public\nPersistentKeepalive = off\n",
                false,
            ),
        ];
        for (label, peer, suitable) in valid {
            let parsed = parse_wireguard_config(&format!("{interface}{peer}")).unwrap();
            assert!(parsed.has_interface_private_key, "{label}");
            assert!(parsed.has_peers, "{label}");
            assert_eq!(parsed.has_suitable_keepalive, suitable, "{label}");
        }

        let invalid = [
            (
                "plus sign",
                "[Peer]\nPublicKey = public\nPersistentKeepalive = +25\n",
                "digits only",
            ),
            (
                "above u16",
                "[Peer]\nPublicKey = public\nPersistentKeepalive = 65536\n",
                "at most 65535",
            ),
            (
                "semicolon comment",
                "[Peer]\nPublicKey = public\n; text\n",
                "semicolon comments are not accepted at line 5",
            ),
            (
                "semicolon key value",
                "[Peer]\nPublicKey = public\n; AllowedIPs = 10.0.0.0/24\n",
                "semicolon comments are not accepted at line 5",
            ),
            (
                "decorated semicolon header",
                "[Peer]; text\nPublicKey = public\n",
                "invalid section header at line 3",
            ),
            (
                "interface keepalive",
                "PersistentKeepalive = 25\n[Peer]\nPublicKey = public\n",
                "belongs in [Peer]",
            ),
            (
                "missing public key",
                "[Peer]\nPersistentKeepalive = 25\n",
                "[Peer] at line 3 needs a non-empty PublicKey",
            ),
        ];
        for (label, suffix, expected) in invalid {
            let error = parse_wireguard_config(&format!("{interface}{suffix}"))
                .expect_err(label)
                .to_string();
            assert!(error.contains(expected), "{label}: {error}");
        }
    }

    #[test]
    fn health_probe_matches_provider_and_wireguard_peers() {
        assert_eq!(
            build_health_args_for(TunnelProvider::Netbird, "tunnel-test", false, None).join(" "),
            "exec tunnel-test netbird status --check startup"
        );
        assert_eq!(
            build_health_args_for(TunnelProvider::Wireguard, "tunnel-test", false, None).join(" "),
            "exec tunnel-test ip link show wg0"
        );
        assert_eq!(
            build_health_args_for(TunnelProvider::Wireguard, "tunnel-test", true, None).join(" "),
            "exec tunnel-test wg show wg0 latest-handshakes"
        );
        assert_eq!(
            build_health_args_for(
                TunnelProvider::Wireguard,
                "tunnel-test",
                true,
                Some("10.231.0.1".parse().unwrap()),
            )
            .join(" "),
            "exec tunnel-test ping -c 1 -W 2 10.231.0.1"
        );
        assert!(
            parse_wireguard_config(
                "[Interface]\nPrivateKey = private\n[Peer]\nPublicKey = public\n"
            )
            .unwrap()
            .has_peers
        );
        assert!(
            !parse_wireguard_config("[Interface]\nPrivateKey = private\nPublicKey = not-a-peer\n")
                .unwrap()
                .has_peers
        );
        assert!(wireguard_handshake_status("peer-a 1000\n", 1180).is_ok());
        assert!(wireguard_handshake_status("peer-a 0\npeer-b 999\n", 1000).is_ok());
        assert!(wireguard_handshake_status("peer-a 0\n", 1000).is_err());
        let stale = wireguard_handshake_status("peer-a 819\n", 1000).unwrap_err();
        assert!(stale.contains("181s old"), "{stale}");
        assert!(stale.contains("180s liveness budget"), "{stale}");
        assert!(wireguard_handshake_status("peer-a 1001\n", 1000).is_err());
        assert!(wireguard_handshake_status("garbage\n", 1000).is_err());
    }

    #[test]
    fn recovery_backoff_preserves_the_probe_reason_and_expires() {
        let mut state = RecoveryState::default();
        state.record(&RecoveryError::Unavailable {
            reason: "WireGuard handshake is 181s old".into(),
        });
        let failed_at = state.last_failure.as_ref().unwrap().failed_at;
        let cached = state.cached_reason(failed_at).unwrap();
        assert!(
            cached.contains("WireGuard handshake is 181s old"),
            "{cached}"
        );
        assert!(cached.contains("next attempt in 30s"), "{cached}");
        assert!(state.cached_reason(failed_at + RECOVERY_BACKOFF).is_none());

        let mut config_state = RecoveryState::default();
        config_state.record(&RecoveryError::Config {
            reason: "invalid configuration".into(),
        });
        assert!(config_state.cached_reason(failed_at).is_none());
    }

    #[tokio::test]
    async fn recovery_completion_is_recorded_before_waiters_are_notified() {
        let (sender, receiver) = tokio::sync::watch::channel(None);
        let mut waiting = receiver.clone();
        let mut state = RecoveryState {
            active: Some(receiver),
            ..RecoveryState::default()
        };
        let result = Err(RecoveryError::Unavailable {
            reason: "dead peer".into(),
        });

        state.complete(&result);
        assert!(state.active.is_none());
        assert!(
            state
                .cached_reason(tokio::time::Instant::now())
                .is_some_and(|reason| reason.contains("dead peer"))
        );
        sender.send_replace(Some(result));
        let completed = waiting
            .wait_for(Option::is_some)
            .await
            .ok()
            .and_then(|result| result.as_ref().cloned());
        assert!(matches!(
            completed,
            Some(Err(RecoveryError::Unavailable { reason })) if reason == "dead peer"
        ));
    }

    fn recovery_test_sidecar(label: &str) -> TunnelSidecar {
        let base = std::env::temp_dir().join(format!(
            "lw-tunnel-recovery-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        TunnelSidecar::for_instance(ContainerRuntime::Podman, netbird_cfg(), &base)
    }

    #[tokio::test]
    async fn abandoned_recovery_is_recorded_and_can_be_retried() {
        let sidecar = recovery_test_sidecar("abandoned");
        let (sender, receiver) = tokio::sync::watch::channel(None);
        sidecar.lifecycle.state.lock().await.active = Some(receiver.clone());
        drop(sender);

        let error = sidecar.await_recovery(receiver).await.unwrap_err();
        assert!(
            error.to_string().contains("ended without a result"),
            "{error}"
        );
        let lifecycle = sidecar.lifecycle.state.lock().await;
        assert!(lifecycle.active.is_none());
        assert!(
            lifecycle
                .cached_reason(tokio::time::Instant::now())
                .is_some_and(|reason| reason.contains("ended without a result"))
        );
    }

    #[tokio::test]
    async fn stale_recovery_waiter_does_not_clear_a_new_attempt() {
        let sidecar = recovery_test_sidecar("stale-waiter");
        let (sender, stale) = tokio::sync::watch::channel(None);
        let (_replacement_sender, replacement) = tokio::sync::watch::channel(None);
        sidecar.lifecycle.state.lock().await.active = Some(replacement);
        drop(sender);

        let error = sidecar.await_recovery(stale).await.unwrap_err();
        assert!(
            error.to_string().contains("ended without a result"),
            "{error}"
        );
        let lifecycle = sidecar.lifecycle.state.lock().await;
        assert!(lifecycle.active.is_some());
        assert!(lifecycle.last_failure.is_none());
    }

    #[tokio::test]
    async fn stopped_sidecar_refuses_per_run_recovery_without_runtime_calls() {
        let sidecar = recovery_test_sidecar("stopped");
        sidecar.lifecycle.state.lock().await.stopping = true;

        let error = sidecar.recover_for_run().await.unwrap_err();
        assert!(error.to_string().contains("sidecar is stopped"), "{error}");
        assert!(sidecar.lifecycle.state.lock().await.active.is_none());
    }

    #[test]
    fn netbird_custom_dns_is_rejected_without_leaking_credentials() {
        assert!(netbird_cfg().validate().is_ok());
        let cfg = TunnelSidecarConfig {
            dns: vec!["100.100.0.53".parse().unwrap()],
            ..netbird_cfg()
        };
        let error = cfg.validate().unwrap_err();
        assert_eq!(
            error,
            "SANDBOX_TUNNEL_DNS is unsupported with the netbird provider; leave it unset to use runtime DNS"
        );
        assert!(!error.contains("setup-key-1"));
        let misplaced_canary = TunnelSidecarConfig {
            wireguard_canary: Some("10.0.0.1".parse().unwrap()),
            ..netbird_cfg()
        };
        assert!(misplaced_canary.validate().is_err());
    }

    #[test]
    fn wireguard_custom_dns_skips_sidecar_and_closes_uplink_dns() {
        let cfg = TunnelSidecarConfig {
            dns: vec!["100.100.0.53".parse().unwrap(), "fd00::53".parse().unwrap()],
            ..wireguard_cfg(None)
        };
        let args = build_sidecar_args(&cfg, None, "tunnel-test", "test-instance", "deadbeef", None)
            .join(" ");
        assert!(!args.contains("--dns"));
        assert!(cfg.uplink_resolvers("nameserver 169.254.1.1\n").is_empty());
        assert_eq!(
            netbird_cfg().uplink_resolvers("nameserver 169.254.1.1\n"),
            vec!["169.254.1.1".parse::<IpAddr>().unwrap()]
        );
        let invalid = TunnelSidecarConfig {
            dns: vec!["0.0.0.0".parse().unwrap()],
            ..wireguard_cfg(None)
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn tunnel_addresses_reject_local_and_non_unicast_destinations() {
        for invalid in [
            "0.0.0.0",
            "127.0.0.1",
            "169.254.1.1",
            "255.255.255.255",
            "224.0.0.1",
            "::",
            "::1",
            "fe80::1",
            "ff02::1",
        ] {
            let address: IpAddr = invalid.parse().unwrap();
            assert!(!valid_tunnel_address(&address), "accepted {address}");
            let dns = TunnelSidecarConfig {
                dns: vec![address],
                ..wireguard_cfg(None)
            };
            assert!(dns.validate().is_err(), "DNS accepted {address}");
            let canary = TunnelSidecarConfig {
                provider: TunnelProvider::Wireguard,
                wireguard_canary: Some(address),
                ..TunnelSidecarConfig::default()
            };
            assert!(canary.validate().is_err(), "canary accepted {address}");
        }
        for valid in ["100.100.0.53", "10.231.0.1", "fd00::53"] {
            assert!(valid_tunnel_address(&valid.parse().unwrap()));
        }
    }

    #[test]
    fn firewall_install_is_one_atomic_transaction_covering_both_directions() {
        let (args, ruleset) = firewall_install_command(&policy(), "tunnel-test");
        assert_eq!(args.join(" "), "exec -i tunnel-test nft -f -");
        assert_eq!(ruleset.matches("add table inet").count(), 1);
        assert_eq!(ruleset.matches("delete table inet").count(), 1);
        assert!(!ruleset.contains("add rule"));
        let add = ruleset.find("add table inet").unwrap();
        let delete = ruleset.find("delete table inet").unwrap();
        let definition = ruleset
            .find("table inet lunarwing_sandbox_egress {")
            .unwrap();
        assert!(add < delete && delete < definition, "{ruleset}");
        for expected in [
            "chain input {\n        type filter hook input priority 0; policy drop;",
            "chain output {\n        type filter hook output priority 0; policy drop;",
            "iifname \"lo\" accept",
            "ct state established,related accept",
            "oifname \"lo\" accept",
            "oifname \"wg0\" accept",
            "oifname \"wt0\" accept",
            "ip daddr 169.254.1.1 udp dport 53 accept",
            "ip daddr 9.9.9.9 tcp dport 53 accept",
            "ip daddr 169.254.0.0/16 drop",
            "meta mark 0x4c57 accept",
        ] {
            assert!(ruleset.contains(expected), "missing {expected}: {ruleset}");
        }
        assert!(!ruleset.contains("output ct state"));
        assert!(!ruleset.contains("output accept"));
        let rules = policy().output_rules();
        let dns = rules
            .iter()
            .position(|r| r.contains("169.254.1.1 udp"))
            .unwrap();
        let drop = rules.iter().position(|r| r.ends_with(" drop")).unwrap();
        let mark = rules
            .iter()
            .position(|r| r.starts_with("meta mark"))
            .unwrap();
        assert!(dns < drop && drop < mark);
    }

    #[test]
    fn output_rules_handle_ipv6_resolvers_and_no_resolvers() {
        let v6 = EgressPolicy {
            tunnel_mark: NETBIRD_CONTROL_PLANE_MARK,
            resolvers: vec!["2606:4700:4700::1111".parse().unwrap()],
        };
        let rules = v6.output_rules();
        assert!(rules.contains(&"ip6 daddr 2606:4700:4700::1111 udp dport 53 accept".to_string()));
        assert!(rules.contains(&"meta mark 0x1bd00 accept".to_string()));
        let none = EgressPolicy {
            tunnel_mark: LUNARWING_TUNNEL_FWMARK,
            resolvers: Vec::new(),
        };
        assert!(none.output_rules().iter().all(|r| !r.contains("dport 53")));
    }

    #[test]
    fn firewall_verification_accepts_nft_listing_format() {
        let policy = policy();
        let listing = listing_for(&policy);
        assert!(listing.contains("priority filter;"), "{listing}");
        assert!(listing.contains("meta mark 0x00004c57 accept"), "{listing}");
        assert_eq!(firewall_matches(&listing, &policy), Ok(()));
        // Older nft prints the numeric priority; handles and comments on
        // expected rules are cosmetic.
        let numeric = listing
            .replace("priority filter;", "priority 0;")
            .replace("oifname \"lo\" accept", "oifname \"lo\" accept # handle 7");
        assert_eq!(firewall_matches(&numeric, &policy), Ok(()));
    }

    #[test]
    fn firewall_verification_rejects_every_weakening() {
        let policy = policy();
        let valid = listing_for(&policy);
        let cases = [
            (
                "policy accept in input",
                valid.replacen(
                    "input priority filter; policy drop;",
                    "input priority filter; policy accept;",
                    1,
                ),
            ),
            (
                "policy accept in output",
                valid.replacen(
                    "output priority filter; policy drop;",
                    "output priority filter; policy accept;",
                    1,
                ),
            ),
            (
                "extra inbound accept",
                valid.replace(
                    "\t}\n\n\tchain output",
                    "\t\tiifname \"wg0\" accept\n\t}\n\n\tchain output",
                ),
            ),
            (
                "extra outbound accept with a comment",
                valid.replace(
                    "\t\tmeta mark 0x00004c57 accept\n",
                    "\t\tmeta mark 0x00004c57 accept\n\t\tip daddr 1.1.1.1 accept comment \"tamper\"\n",
                ),
            ),
            (
                "missing link-local drop",
                valid.replace("\t\tip daddr 169.254.0.0/16 drop\n", ""),
            ),
            (
                "missing established rule",
                valid.replace("\t\tct state established,related accept\n", ""),
            ),
            ("changed mark", valid.replace("0x00004c57", "0x00001bd00")),
            ("foreign resolver", valid.replace("9.9.9.9", "8.8.8.8")),
            (
                "output chain missing",
                valid.split("\n\n\tchain output").next().unwrap().to_string() + "\n}\n",
            ),
            (
                "extra chain",
                valid.replace(
                    "\t}\n}\n",
                    "\t}\n\n\tchain forward {\n\t\ttype filter hook forward priority filter; policy accept;\n\t}\n}\n",
                ),
            ),
            ("empty listing", String::new()),
            (
                "other table",
                valid.replace("table inet lunarwing_sandbox_egress", "table inet other"),
            ),
        ];
        for (label, tampered) in cases {
            assert!(
                firewall_matches(&tampered, &policy).is_err(),
                "{label} must be rejected:\n{tampered}"
            );
        }
    }

    #[test]
    fn canonical_line_normalises_marks_priority_handles_and_comments() {
        assert_eq!(
            canonical_nft_line("\t\tmeta mark 0x00004c57 accept # handle 12"),
            "meta mark 0x4c57 accept"
        );
        assert_eq!(
            canonical_nft_line("mark 0x00004c57 accept"),
            "meta mark 0x4c57 accept"
        );
        assert_eq!(
            canonical_nft_line("ip daddr 169.254.1.1 udp dport domain accept"),
            "ip daddr 169.254.1.1 udp dport 53 accept"
        );
        assert_eq!(
            canonical_nft_line("type filter hook output priority filter; policy drop;"),
            "type filter hook output priority 0; policy drop;"
        );
        assert_eq!(
            canonical_nft_line("ip daddr 1.1.1.1 accept comment \"x\""),
            "ip daddr 1.1.1.1 accept"
        );
    }

    #[test]
    fn resolver_and_fwmark_parsing() {
        let resolv = "# generated\nnameserver 169.254.1.1\nnameserver 9.9.9.9\nnameserver 9.9.9.9\nsearch example.com\nnameserver fe80::1%eth0\nnameserver not-an-ip\n";
        let parsed = parse_resolvers(resolv);
        assert_eq!(
            parsed,
            vec![
                "169.254.1.1".parse::<IpAddr>().unwrap(),
                "9.9.9.9".parse().unwrap(),
                "fe80::1".parse().unwrap()
            ]
        );
        assert_eq!(parse_wireguard_fwmark("off\n"), None);
        assert_eq!(parse_wireguard_fwmark("0x4c57\n"), Some(0x4c57));
        assert_eq!(parse_wireguard_fwmark("0xca6c"), Some(51820));
        assert_eq!(parse_wireguard_fwmark("garbage"), None);
    }

    /// WI#601: the helper unit may be signalled only when the runtime says
    /// the container does not exist. A failed probe proves nothing.
    #[test]
    fn helper_unit_cleanup_requires_a_positively_absent_container() {
        assert_eq!(container_absent_from_inspect(true, ""), Some(false));
        assert_eq!(
            container_absent_from_inspect(false, "Error: no such container \"tunnel-test\"\n"),
            Some(true)
        );
        assert_eq!(
            container_absent_from_inspect(
                false,
                "Error response from daemon: No such container: tunnel-test\n"
            ),
            Some(true)
        );
        for stderr in [
            "",
            "Error: has dependent containers which must be removed before it\n",
            "Cannot connect to the Docker daemon at unix:///var/run/docker.sock.\n",
            "Failed to start transient service unit: Connection refused\n",
        ] {
            assert_eq!(
                container_absent_from_inspect(false, stderr),
                None,
                "{stderr}"
            );
        }
    }

    #[test]
    fn removal_commands_match_the_runtime() {
        let podman = removal_commands(ContainerRuntime::Podman, 10, "tunnel-test");
        assert_eq!(podman.len(), 1);
        assert_eq!(podman[0].join(" "), "rm -f -t 10 tunnel-test");
        let docker_stop = removal_commands(ContainerRuntime::Docker, 10, "tunnel-test");
        assert_eq!(docker_stop.len(), 2);
        assert_eq!(docker_stop[0].join(" "), "stop -t 10 tunnel-test");
        assert_eq!(docker_stop[1].join(" "), "rm -f tunnel-test");
        let docker_kill = removal_commands(ContainerRuntime::Docker, 0, "tunnel-test");
        assert_eq!(docker_kill.len(), 1);
        assert_eq!(docker_kill[0].join(" "), "rm -f tunnel-test");
        assert!(docker_kill.iter().all(|c| !c.iter().any(|a| a == "-t")));
    }

    #[cfg(unix)]
    #[test]
    fn secret_env_file_is_owner_only_from_creation_and_removed_on_drop() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("lw-tunnel-env-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let secret = SecretString::from("NB_SETUP_KEY=never-log-this\n");
        let path = {
            let file = SecretEnvFile::write(&dir, &secret).unwrap();
            let mode = std::fs::metadata(file.as_path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
            assert_eq!(
                std::fs::read_to_string(file.as_path()).unwrap(),
                "NB_SETUP_KEY=never-log-this\n"
            );
            file.as_path().to_path_buf()
        };
        assert!(!path.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn label_salt_is_created_once_owner_only_and_reused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("lw-tunnel-salt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = read_or_create_label_salt(&dir).unwrap();
        assert_eq!(first.len(), 32);
        let mode = std::fs::metadata(dir.join(LABEL_SALT_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(read_or_create_label_salt(&dir).unwrap(), first);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
