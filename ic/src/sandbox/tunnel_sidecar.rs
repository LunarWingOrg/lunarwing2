//! Egress-only NetBird and WireGuard sidecars for the generic sandbox (WI#248).
//!
//! Sandboxed commands join this sidecar's network namespace while retaining
//! their own filesystem, user namespace, and hardening flags. The sidecar owns
//! all tunnel credentials. A netfilter input chain drops new inbound flows so
//! joining the namespace cannot expose a listener to the host, LAN, or overlay.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};

use crate::sandbox::cli_runner::ContainerRuntime;
use crate::sandbox::error::{Result, SandboxError};

pub const SANDBOX_TUNNEL_CONTAINER: &str = "lunarwing-sandbox-tunnel";
pub const SANDBOX_TUNNEL_CONTAINER_LABEL: &str = "lunarwing.sandbox-tunnel";
pub const SANDBOX_TUNNEL_HASH_LABEL: &str = "lunarwing.sandbox-tunnel-hash";

const SIDECAR_MEMORY_MB: u64 = 256;
const SIDECAR_PIDS_LIMIT: u32 = 64;
const HEALTH_POLL_INTERVAL: Duration = Duration::from_secs(2);
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_WIREGUARD_CONFIG_BYTES: u64 = 1024 * 1024;
const NFT_TABLE: &str = "lunarwing_sandbox_egress";
const NFT_CHAIN: &str = "input";

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
            Self::Netbird => "docker.io/netbirdio/netbird:latest",
            Self::Wireguard => "localhost/lunarwing-tunnel-wireguard:latest",
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
    pub startup_timeout: Duration,
}

impl Default for TunnelSidecarConfig {
    fn default() -> Self {
        Self {
            provider: TunnelProvider::Netbird,
            image: None,
            netbird_management_url: None,
            netbird_setup_key: None,
            wireguard_config: None,
            startup_timeout: Duration::from_secs(90),
        }
    }
}

impl TunnelSidecarConfig {
    pub fn image(&self) -> &str {
        self.image
            .as_deref()
            .unwrap_or(self.provider.default_image())
    }

    /// A stable drift marker, never an authentication value. WireGuard file
    /// contents are included so rotating a private key recreates the sidecar.
    pub fn config_hash(&self) -> String {
        let mut hasher = Sha256::new();
        hash_field(&mut hasher, self.provider.as_str().as_bytes());
        hash_field(&mut hasher, self.image().as_bytes());
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

    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.image().trim().is_empty() {
            return Err("SANDBOX_TUNNEL_IMAGE must not be empty".into());
        }
        match self.provider {
            TunnelProvider::Netbird => self.validate_netbird(),
            TunnelProvider::Wireguard => {
                let path = self.wireguard_config.as_deref().ok_or_else(|| {
                    "SANDBOX_TUNNEL_WIREGUARD_CONFIG is required for the wireguard provider"
                        .to_string()
                })?;
                validate_wireguard_config(path)
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

fn validate_wireguard_config(path: &Path) -> std::result::Result<(), String> {
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
    if !has_interface_private_key(&content) {
        return Err(format!(
            "SANDBOX_TUNNEL_WIREGUARD_CONFIG {} needs a non-empty PrivateKey in [Interface]",
            path.display()
        ));
    }
    Ok(())
}

fn has_interface_private_key(content: &str) -> bool {
    let mut in_interface = false;
    content.lines().any(|raw| {
        let line = raw.trim();
        if line.starts_with('[') {
            in_interface = line.eq_ignore_ascii_case("[interface]");
            return false;
        }
        if !in_interface || line.starts_with('#') || line.starts_with(';') {
            return false;
        }
        line.split_once('=').is_some_and(|(key, value)| {
            key.trim().eq_ignore_ascii_case("privatekey") && !value.trim().is_empty()
        })
    })
}

pub(crate) fn build_sidecar_args(
    cfg: &TunnelSidecarConfig,
    secret_env_file: Option<&Path>,
) -> Vec<String> {
    let mut args = vec![
        "run".into(),
        "-d".into(),
        "--rm".into(),
        "--name".into(),
        SANDBOX_TUNNEL_CONTAINER.into(),
        "--label".into(),
        format!(
            "{}={}",
            SANDBOX_TUNNEL_CONTAINER_LABEL,
            cfg.provider.as_str()
        ),
        "--label".into(),
        format!("{}={}", SANDBOX_TUNNEL_HASH_LABEL, cfg.config_hash()),
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
        "--tmpfs".into(),
        "/tmp:size=16m".into(),
        "--tmpfs".into(),
        "/run:size=16m".into(),
    ];

    match cfg.provider {
        TunnelProvider::Netbird => {
            args.extend([
                "--device".into(),
                "/dev/net/tun".into(),
                "--tmpfs".into(),
                "/var/lib/netbird:size=16m".into(),
            ]);
            if let Some(file) = secret_env_file {
                args.extend(["--env-file".into(), file.display().to_string()]);
            }
            args.extend([cfg.image().into(), "up".into()]);
        }
        TunnelProvider::Wireguard => {
            if let Some(config) = cfg.wireguard_config.as_deref() {
                args.extend([
                    "-v".into(),
                    format!("{}:/etc/wireguard/wg0.conf:ro,z", config.display()),
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

fn build_health_args_for(provider: TunnelProvider) -> Vec<String> {
    let probe: &[&str] = match provider {
        TunnelProvider::Netbird => &["netbird", "status", "--check", "startup"],
        TunnelProvider::Wireguard => &["ip", "link", "show", "wg0"],
    };
    let mut args = vec!["exec".into(), SANDBOX_TUNNEL_CONTAINER.into()];
    args.extend(probe.iter().map(|arg| (*arg).into()));
    args
}

fn nft_exec_args(nft_args: &[&str]) -> Vec<String> {
    let mut args = vec!["exec".into(), SANDBOX_TUNNEL_CONTAINER.into(), "nft".into()];
    args.extend(nft_args.iter().map(|arg| (*arg).into()));
    args
}

fn firewall_install_commands() -> Vec<Vec<String>> {
    [
        &["delete", "table", "inet", NFT_TABLE][..],
        &["add", "table", "inet", NFT_TABLE],
        &[
            "add",
            "chain",
            "inet",
            NFT_TABLE,
            NFT_CHAIN,
            "{ type filter hook input priority 0; policy drop; }",
        ],
        &[
            "add",
            "rule",
            "inet",
            NFT_TABLE,
            NFT_CHAIN,
            "iifname \"lo\" accept",
        ],
        &[
            "add",
            "rule",
            "inet",
            NFT_TABLE,
            NFT_CHAIN,
            "ct state established,related accept",
        ],
    ]
    .iter()
    .map(|args| nft_exec_args(args))
    .collect()
}

fn firewall_list_args() -> Vec<String> {
    nft_exec_args(&["list", "chain", "inet", NFT_TABLE, NFT_CHAIN])
}

fn firewall_listing_is_egress_only(output: &str) -> bool {
    let normalized = output.split_whitespace().collect::<Vec<_>>().join(" ");
    let accept_rules = output
        .lines()
        .map(str::trim)
        .filter(|line| line.ends_with(" accept"))
        .count();
    normalized.contains("type filter hook input priority 0; policy drop;")
        && normalized.contains("iifname \"lo\" accept")
        && normalized.contains("ct state established,related accept")
        && accept_rules == 2
}

pub struct TunnelSidecar {
    bin: &'static str,
    cfg: TunnelSidecarConfig,
}

impl TunnelSidecar {
    pub fn new(runtime: ContainerRuntime, cfg: TunnelSidecarConfig) -> Self {
        Self {
            bin: runtime.binary(),
            cfg,
        }
    }

    pub async fn ensure_started(&self) -> Result<()> {
        self.cfg
            .validate()
            .map_err(|reason| SandboxError::Config { reason })?;
        if self.reuse_if_matching().await {
            return self.wait_healthy().await;
        }
        self.remove().await;
        self.start_fresh().await
    }

    /// Recheck the transport and its egress-only firewall before every run.
    pub async fn ensure_healthy(&self) -> Result<()> {
        if !self.reuse_if_matching().await {
            return Err(self.unavailable("sidecar is not running with the current configuration"));
        }
        self.check_egress_firewall().await?;
        if self.provider_probe().await {
            Ok(())
        } else {
            Err(self.unavailable("sidecar provider health check failed"))
        }
    }

    async fn start_fresh(&self) -> Result<()> {
        let env_file = self.write_secret_env_file()?;
        let args = build_sidecar_args(&self.cfg, env_file.as_ref().map(SecretEnvFile::as_path));
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let started = tokio::time::timeout(
            self.cfg.startup_timeout,
            crate::sandbox::cli_runner::output_isolated(self.bin, &arg_refs),
        )
        .await;
        drop(env_file);
        let output = match started {
            Ok(Ok(output)) => output,
            Ok(Err(_)) => {
                self.remove().await;
                return Err(self.unavailable("failed to execute the container runtime"));
            }
            Err(_) => {
                self.remove().await;
                return Err(self.unavailable("sidecar start exceeded the startup timeout"));
            }
        };
        if !output.status.success() {
            if self.reuse_if_matching().await {
                return self.wait_healthy().await;
            }
            self.remove().await;
            return Err(self.unavailable(
                "container runtime rejected sidecar start; verify the image and host prerequisites",
            ));
        }
        tracing::info!(
            container = SANDBOX_TUNNEL_CONTAINER,
            provider = self.cfg.provider.as_str(),
            "Sandbox tunnel sidecar started"
        );
        if let Err(error) = self.install_egress_firewall().await {
            self.remove().await;
            return Err(error);
        }
        self.wait_healthy().await
    }

    async fn wait_healthy(&self) -> Result<()> {
        let result = self.wait_healthy_inner().await;
        if result.is_err() {
            self.remove().await;
        }
        result
    }

    async fn wait_healthy_inner(&self) -> Result<()> {
        let deadline = tokio::time::Instant::now() + self.cfg.startup_timeout;
        loop {
            self.check_egress_firewall().await?;
            if self.provider_probe().await {
                tracing::info!(
                    container = SANDBOX_TUNNEL_CONTAINER,
                    provider = self.cfg.provider.as_str(),
                    "Sandbox tunnel sidecar healthy; new inbound flows denied"
                );
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(self.unavailable(&format!(
                    "sidecar did not become healthy within {}s",
                    self.cfg.startup_timeout.as_secs()
                )));
            }
            tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
        }
    }

    async fn reuse_if_matching(&self) -> bool {
        let Some((running, hash)) = self.inspect().await else {
            return false;
        };
        running && hash.as_deref() == Some(self.cfg.config_hash().as_str())
    }

    async fn inspect(&self) -> Option<(bool, Option<String>)> {
        let format = format!(
            "{{{{.State.Running}}}}|{{{{index .Config.Labels \"{SANDBOX_TUNNEL_HASH_LABEL}\"}}}}"
        );
        let output = self
            .output_probe(&["inspect", "--format", &format, SANDBOX_TUNNEL_CONTAINER])
            .await
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut parts = text.trim().splitn(2, '|');
        let running = parts.next()? == "true";
        let hash = parts
            .next()
            .map(str::trim)
            .filter(|hash| !hash.is_empty())
            .map(str::to_string);
        Some((running, hash))
    }

    async fn provider_probe(&self) -> bool {
        let args = build_health_args_for(self.cfg.provider);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.status_probe(&refs).await.is_some_and(|s| s.success())
    }

    async fn install_egress_firewall(&self) -> Result<()> {
        for (index, args) in firewall_install_commands().iter().enumerate() {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let output = self.output_probe(&refs).await.map_err(|_| {
                self.unavailable("failed to execute nftables in the tunnel sidecar")
            })?;
            if index > 0 && !output.status.success() {
                return Err(self.unavailable(
                    "egress-only firewall setup failed; the sidecar image must provide nftables",
                ));
            }
        }
        self.check_egress_firewall().await
    }

    async fn check_egress_firewall(&self) -> Result<()> {
        let args = firewall_list_args();
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = self
            .output_probe(&refs)
            .await
            .map_err(|_| self.unavailable("egress-only firewall verification could not run"))?;
        let listing = String::from_utf8_lossy(&output.stdout);
        if output.status.success() && firewall_listing_is_egress_only(&listing) {
            Ok(())
        } else {
            Err(self.unavailable(
                "egress-only firewall is absent or altered; refusing sandbox execution",
            ))
        }
    }

    async fn status_probe(&self, args: &[&str]) -> Option<std::process::ExitStatus> {
        tokio::time::timeout(
            PROBE_TIMEOUT,
            crate::sandbox::cli_runner::run_status_isolated(self.bin, args),
        )
        .await
        .ok()
        .flatten()
    }

    async fn output_probe(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        tokio::time::timeout(
            PROBE_TIMEOUT,
            crate::sandbox::cli_runner::output_isolated(self.bin, args),
        )
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "probe timed out"))?
    }

    fn unavailable(&self, detail: &str) -> SandboxError {
        SandboxError::TunnelUnavailable {
            reason: format!("{} sidecar: {detail}", self.cfg.provider.as_str()),
        }
    }

    pub async fn remove(&self) {
        let _ = tokio::time::timeout(
            CLEANUP_TIMEOUT,
            crate::sandbox::cli_runner::run_status_isolated(
                self.bin,
                &["rm", "-f", "-t", "0", SANDBOX_TUNNEL_CONTAINER],
            ),
        )
        .await;
    }

    pub async fn stop(&self) {
        let _ = tokio::time::timeout(
            CLEANUP_TIMEOUT,
            crate::sandbox::cli_runner::run_status_isolated(
                self.bin,
                &["rm", "-f", "-t", "10", SANDBOX_TUNNEL_CONTAINER],
            ),
        )
        .await;
        tracing::info!(
            container = SANDBOX_TUNNEL_CONTAINER,
            "Sandbox tunnel sidecar stopped"
        );
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
        let contents = SecretString::from(format!("NB_MANAGEMENT_URL={url}\nNB_SETUP_KEY={key}\n"));
        SecretEnvFile::write(&contents)
            .map(Some)
            .map_err(|_| self.unavailable("failed to stage the NetBird credential file"))
    }
}

struct SecretEnvFile {
    path: PathBuf,
}

impl SecretEnvFile {
    fn write(contents: &SecretString) -> std::io::Result<Self> {
        let base = crate::sandbox::cli_runner::shared_mount_source_parent();
        let path = base.join(format!(
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

    #[test]
    fn provider_parsing_and_defaults() {
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
            "docker.io/netbirdio/netbird:latest"
        );
        assert_eq!(
            TunnelProvider::Wireguard.default_image(),
            "localhost/lunarwing-tunnel-wireguard:latest"
        );
    }

    #[test]
    fn netbird_args_keep_credentials_out_of_argv() {
        let args = build_sidecar_args(&netbird_cfg(), Some(Path::new("/run/user/1000/lw-env")));
        let joined = args.join("\n");
        assert!(!joined.contains("setup-key-1"));
        assert!(!joined.contains("netbird.example.com"));
        assert!(joined.contains("--env-file\n/run/user/1000/lw-env"));
        assert!(joined.contains("--device\n/dev/net/tun"));
        assert!(joined.contains("--cap-drop=ALL"));
        assert!(joined.contains("--cap-add=NET_ADMIN"));
        assert!(!joined.contains("SYS_ADMIN"));
        assert!(!joined.contains("--privileged"));
        assert!(!joined.contains("--publish"));
        assert_eq!(args.last().map(String::as_str), Some("up"));
    }

    #[test]
    fn wireguard_args_mount_only_the_owner_only_config() {
        let args = build_sidecar_args(
            &wireguard_cfg(Some(PathBuf::from("/etc/lunarwing/wg0.conf"))),
            None,
        );
        let joined = args.join(" ");
        assert!(joined.contains("/etc/lunarwing/wg0.conf:/etc/wireguard/wg0.conf:ro,z"));
        assert!(joined.contains("wg-quick up /etc/wireguard/wg0.conf"));
        assert!(joined.contains("exec sleep 2147483647"));
        assert!(!joined.contains("sleep infinity"));
        assert!(!joined.contains("--device"));
        assert!(!joined.contains("--env-file"));
        assert!(!joined.contains("--network host"));
    }

    #[test]
    fn sidecar_preserves_hardening_and_separate_label_namespace() {
        let cfg = netbird_cfg();
        let joined = build_sidecar_args(&cfg, None).join(" ");
        for flag in [
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--read-only",
            "--pids-limit 64",
            "--stop-timeout 10",
            "net.ipv4.ip_forward=0",
            "net.ipv6.conf.all.forwarding=0",
        ] {
            assert!(joined.contains(flag), "missing {flag}: {joined}");
        }
        assert!(joined.contains(&format!("{SANDBOX_TUNNEL_CONTAINER_LABEL}=netbird")));
        assert!(joined.contains(&format!(
            "{SANDBOX_TUNNEL_HASH_LABEL}={}",
            cfg.config_hash()
        )));
        assert!(!joined.contains("lunarwing.sandbox="));
    }

    #[test]
    fn config_hash_covers_rotation_and_wireguard_contents() {
        let base = netbird_cfg();
        let rotated = TunnelSidecarConfig {
            netbird_setup_key: Some(SecretString::from("setup-key-2")),
            ..base.clone()
        };
        assert_ne!(base.config_hash(), rotated.config_hash());
        assert_eq!(base.config_hash(), netbird_cfg().config_hash());

        let dir = std::env::temp_dir().join(format!("lw-tunnel-hash-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wg0.conf");
        std::fs::write(&path, "[Interface]\nPrivateKey = first\n").unwrap();
        let first = wireguard_cfg(Some(path.clone())).config_hash();
        std::fs::write(&path, "[Interface]\nPrivateKey = second\n").unwrap();
        let second = wireguard_cfg(Some(path)).config_hash();
        assert_ne!(first, second);
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
    fn wireguard_validation_checks_structure_and_permissions() {
        let dir = std::env::temp_dir().join(format!("lw-tunnel-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(wireguard_cfg(None).validate().is_err());
        let config = dir.join("wg0.conf");
        std::fs::write(
            &config,
            "[Peer]\nPrivateKey = wrong-section\n[Interface]\nAddress = 10.0.0.2/32\n",
        )
        .unwrap();
        assert!(wireguard_cfg(Some(config.clone())).validate().is_err());
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
    fn health_probe_matches_provider() {
        assert_eq!(
            build_health_args_for(TunnelProvider::Netbird).join(" "),
            "exec lunarwing-sandbox-tunnel netbird status --check startup"
        );
        assert_eq!(
            build_health_args_for(TunnelProvider::Wireguard).join(" "),
            "exec lunarwing-sandbox-tunnel ip link show wg0"
        );
    }

    #[test]
    fn firewall_install_drops_new_inbound_and_allows_only_return_flows() {
        let commands = firewall_install_commands();
        let joined = commands
            .iter()
            .map(|command| command.join(" "))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("{ type filter hook input priority 0; policy drop; }"));
        assert!(joined.contains("iifname \"lo\" accept"));
        assert!(joined.contains("ct state established,related accept"));
        assert_eq!(commands.len(), 5);
        assert!(!joined.contains("output"));
    }

    #[test]
    fn firewall_verification_rejects_missing_or_extra_inbound_accepts() {
        let valid = r#"
table inet lunarwing_sandbox_egress {
    chain input {
        type filter hook input priority 0; policy drop;
        iifname "lo" accept
        ct state established,related accept
    }
}
"#;
        assert!(firewall_listing_is_egress_only(valid));
        assert!(!firewall_listing_is_egress_only(
            &valid.replace("policy drop", "policy accept")
        ));
        assert!(!firewall_listing_is_egress_only(&format!(
            "{valid}\niifname \"wg0\" accept"
        )));
        assert!(!firewall_listing_is_egress_only(
            &valid.replace("ct state established,related accept", "")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn secret_env_file_is_owner_only_from_creation_and_removed_on_drop() {
        use std::os::unix::fs::PermissionsExt;
        let secret = SecretString::from("NB_SETUP_KEY=never-log-this\n");
        let path = {
            let file = SecretEnvFile::write(&secret).unwrap();
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
    }
}
