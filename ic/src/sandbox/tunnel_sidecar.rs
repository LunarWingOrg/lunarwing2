//! Tunnel sidecar for sandbox egress over NetBird or kernel WireGuard (WI#248).
//!
//! `SANDBOX_NETWORK=tunnel` runs one long-lived detached container — the
//! **tunnel sidecar** — that establishes the tunnel inside its own
//! (user-namespace-owned) network namespace. Every sandbox run then joins
//! that namespace with `--network container:lunarwing-sandbox-tunnel`
//! (see [`crate::sandbox::cli_runner`]), keeping all of its own hardening
//! flags; only the network stack is shared.
//!
//! The sidecar is the single credential holder for the tunnel (NetBird setup
//! key, WireGuard private key). Credentials reach it through a mode-0600
//! `--env-file` (NetBird) or a mode-0600 config file mounted `:ro` — never
//! through the podman argv, which the systemd transient unit would persist
//! in the journal. Sandboxed commands never see them: they share the
//! network namespace, not the filesystem or environment.
//!
//! Lifecycle is manager-owned ([`crate::sandbox::manager::SandboxManager`]):
//! health-gated start at `initialize()`, stop at `shutdown()`, and a config
//! hash label so a settings change recreates the sidecar instead of reusing
//! a stale tunnel. Fail closed: a sidecar that cannot start or pass its
//! health gate within the startup timeout is removed and surfaces
//! [`SandboxError::TunnelUnavailable`] — execution never degrades to another
//! egress mode.
//!
//! BOUNDARY: same as the rest of `crate::sandbox` — this belongs to the
//! generic shell-tool sandbox, not the orchestrator's worker containers.

use std::path::{Path, PathBuf};
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};

use crate::sandbox::cli_runner::ContainerRuntime;
use crate::sandbox::error::{Result, SandboxError};

/// Fixed sidecar container name. Sandbox runs reference it in
/// `--network container:<name>`, so it must be deterministic.
pub const SANDBOX_TUNNEL_CONTAINER: &str = "lunarwing-sandbox-tunnel";

/// Label namespace for the tunnel sidecar. Deliberately distinct from
/// [`crate::sandbox::cli_runner::SANDBOX_CONTAINER_LABEL`] so the reaper's
/// one-shot sandbox sweeps never touch the (long-lived) sidecar.
pub const SANDBOX_TUNNEL_CONTAINER_LABEL: &str = "lunarwing.sandbox-tunnel";

/// Label carrying the config hash; a mismatch means the sidecar predates the
/// current settings and must be recreated.
pub const SANDBOX_TUNNEL_HASH_LABEL: &str = "lunarwing.sandbox-tunnel-hash";

/// Default sidecar memory limit (tunnel daemons are small; the sandbox
/// containers carry their own limit).
const SIDECAR_MEMORY_MB: u64 = 256;

/// Default sidecar process limit.
const SIDECAR_PIDS_LIMIT: u32 = 64;

/// Which tunnel technology the sidecar establishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TunnelProvider {
    /// NetBird overlay: runs `netbird up` in the upstream image; needs a
    /// management URL and a setup key (both delivered via `--env-file`) plus
    /// `/dev/net/tun` (NetBird's userspace TUN interface).
    #[default]
    Netbird,
    /// Plain kernel WireGuard: `wg-quick up` against an operator-supplied
    /// wg-quick-format config mounted read-only at `/etc/wireguard/wg0.conf`.
    /// Works rootless because the container's user namespace owns its
    /// network namespace (CAP_NET_ADMIN inside the userns may create the
    /// `wireguard` interface; no host kernel module load happens — the
    /// module must already be loaded on the host).
    Wireguard,
}

impl TunnelProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            TunnelProvider::Netbird => "netbird",
            TunnelProvider::Wireguard => "wireguard",
        }
    }

    /// Image used when the operator does not override `SANDBOX_TUNNEL_IMAGE`.
    pub fn default_image(&self) -> &'static str {
        match self {
            TunnelProvider::Netbird => "docker.io/netbirdio/netbird:latest",
            // Operator-built image (guide ships a Containerfile): needs
            // wireguard-tools (wg-quick/wg), iproute2, /bin/sh, sleep.
            TunnelProvider::Wireguard => "localhost/lunarwing-tunnel-wireguard:latest",
        }
    }
}

impl std::str::FromStr for TunnelProvider {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "netbird" => Ok(TunnelProvider::Netbird),
            "wireguard" | "wg" => Ok(TunnelProvider::Wireguard),
            _ => Err(format!(
                "invalid tunnel provider '{s}', expected 'netbird' or 'wireguard'"
            )),
        }
    }
}

/// Settings for the tunnel sidecar. Populated from `SANDBOX_TUNNEL_*` env
/// vars (env-only, like the reaper knobs — mt-admin renders tenant env
/// files); validated in [`TunnelSidecarConfig::validate`] and again at
/// sidecar start.
#[derive(Debug, Clone)]
pub struct TunnelSidecarConfig {
    pub provider: TunnelProvider,
    /// Overrides the per-provider default image.
    pub image: Option<String>,
    /// NetBird management URL (`https://…`); required for NetBird.
    pub netbird_management_url: Option<String>,
    /// NetBird setup key; required for NetBird. Never logged.
    pub netbird_setup_key: Option<SecretString>,
    /// Path to a wg-quick-format config (mode 0600); required for WireGuard.
    pub wireguard_config: Option<PathBuf>,
    /// How long the health gate waits for the tunnel to come up.
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
    /// Effective image (override or provider default).
    pub fn image(&self) -> &str {
        self.image
            .as_deref()
            .unwrap_or(self.provider.default_image())
    }

    /// Hash of the settings that define the tunnel. Stored as a container
    /// label; a mismatch on reuse triggers recreation. Includes the setup
    /// key bytes so key rotation recreates the sidecar. Not
    /// security-sensitive — it is drift detection, not authentication.
    pub fn config_hash(&self) -> String {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.provider.as_str().hash(&mut hasher);
        self.image().hash(&mut hasher);
        self.netbird_management_url.hash(&mut hasher);
        self.netbird_setup_key
            .as_ref()
            .map(|k| k.expose_secret().to_string())
            .hash(&mut hasher);
        self.wireguard_config.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    /// Validate the configuration for the selected provider. Fail closed:
    /// every refusal message names the missing knob.
    pub fn validate(&self) -> std::result::Result<(), String> {
        match self.provider {
            TunnelProvider::Netbird => {
                let Some(url) = self.netbird_management_url.as_deref() else {
                    return Err(
                        "SANDBOX_TUNNEL_NETBIRD_MANAGEMENT_URL is required for the netbird \
                         tunnel provider"
                            .into(),
                    );
                };
                if !url.starts_with("https://") || url.contains(char::is_whitespace) {
                    return Err(
                        "SANDBOX_TUNNEL_NETBIRD_MANAGEMENT_URL must be an https:// URL without \
                         whitespace"
                            .into(),
                    );
                }
                let Some(key) = self.netbird_setup_key.as_ref() else {
                    return Err(
                        "SANDBOX_TUNNEL_NETBIRD_SETUP_KEY is required for the netbird tunnel \
                         provider"
                            .into(),
                    );
                };
                if key.expose_secret().trim().is_empty()
                    || key.expose_secret().contains(char::is_whitespace)
                {
                    return Err("SANDBOX_TUNNEL_NETBIRD_SETUP_KEY must be a single token".into());
                }
            }
            TunnelProvider::Wireguard => {
                let Some(path) = self.wireguard_config.as_deref() else {
                    return Err(
                        "SANDBOX_TUNNEL_WIREGUARD_CONFIG (path to a wg-quick config) is \
                         required for the wireguard tunnel provider"
                            .into(),
                    );
                };
                validate_wireguard_config(path)?;
            }
        }
        if self.startup_timeout.is_zero() {
            return Err("tunnel startup timeout must be greater than zero".into());
        }
        Ok(())
    }
}

/// Check the wg-quick config file exists, is owner-only readable, and looks
/// like a WireGuard interface config. The private key never enters logs or
/// errors — only structural facts are reported.
fn validate_wireguard_config(path: &Path) -> std::result::Result<(), String> {
    let meta = std::fs::metadata(path).map_err(|e| {
        format!(
            "SANDBOX_TUNNEL_WIREGUARD_CONFIG {} unreadable: {e}",
            path.display()
        )
    })?;
    if !meta.is_file() {
        return Err(format!(
            "SANDBOX_TUNNEL_WIREGUARD_CONFIG {} is not a regular file",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(format!(
                "SANDBOX_TUNNEL_WIREGUARD_CONFIG {} must be mode 0600 (owner-only); it holds \
                 the WireGuard private key. Fix with: chmod 600 {}",
                path.display(),
                path.display()
            ));
        }
    }
    let content = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "SANDBOX_TUNNEL_WIREGUARD_CONFIG {} unreadable: {e}",
            path.display()
        )
    })?;
    if !content.contains("PrivateKey") {
        return Err(format!(
            "SANDBOX_TUNNEL_WIREGUARD_CONFIG {} does not look like a wg-quick config (no \
             PrivateKey under [Interface])",
            path.display()
        ));
    }
    Ok(())
}

/// Build the `<runtime> run …` argument vector for the tunnel sidecar.
/// Pure function — unit-testable without any runtime installed. The NetBird
/// secrets arrive via `secret_env_file` (a host-side mode-0600 file passed
/// with `--env-file`), never through argv.
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
        "--memory".into(),
        format!("{SIDECAR_MEMORY_MB}m"),
        "--pids-limit".into(),
        SIDECAR_PIDS_LIMIT.to_string(),
        "--stop-timeout".into(),
        "10".into(),
    ];

    match cfg.provider {
        TunnelProvider::Netbird => {
            // NetBird's userspace TUN interface plus the secret env file.
            args.push("--device".into());
            args.push("/dev/net/tun".into());
            if let Some(file) = secret_env_file {
                args.push("--env-file".into());
                args.push(file.display().to_string());
            }
            args.push(cfg.image().into());
            // The upstream image entrypoint is the netbird CLI.
            args.push("up".into());
        }
        TunnelProvider::Wireguard => {
            // The 0600 config is mounted read-only; wg-quick brings the
            // interface and routes up, then the sidecar idles.
            if let Some(conf) = cfg.wireguard_config.as_deref() {
                args.push("-v".into());
                args.push(format!("{}:/etc/wireguard/wg0.conf:ro,z", conf.display()));
            }
            args.push(cfg.image().into());
            args.push("/bin/sh".into());
            args.push("-c".into());
            args.push("wg-quick up /etc/wireguard/wg0.conf && exec sleep infinity".into());
        }
    }
    args
}

/// Wait between health polls.
const HEALTH_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Owns the tunnel sidecar lifecycle for a detected runtime.
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

    /// Ensure the sidecar is running (reusing a healthy container whose
    /// config-hash label matches), health-gated within the startup timeout.
    ///
    /// Fail closed: any failure removes the sidecar and returns
    /// [`SandboxError::TunnelUnavailable`] — callers must not fall back to
    /// another egress mode.
    pub async fn ensure_started(&self) -> Result<()> {
        if let Err(reason) = self.cfg.validate() {
            return Err(SandboxError::Config { reason });
        }
        if self.reuse_if_matching().await {
            return self.wait_healthy().await;
        }
        self.remove().await;

        let env_file = self.write_secret_env_file()?;
        let args = build_sidecar_args(&self.cfg, env_file.as_ref().map(SecretEnvFile::as_path));
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = crate::sandbox::cli_runner::output_isolated(self.bin, &arg_refs)
            .await
            .map_err(|e| SandboxError::TunnelUnavailable {
                reason: format!("failed to spawn sidecar: {e}"),
            })?;
        drop(env_file);

        if !output.status.success() {
            let reason = String::from_utf8_lossy(&output.stderr).trim().to_string();
            // Another manager instance may have won the name race; accept it
            // only when the result is healthy, otherwise fail closed.
            if reason.contains("already in use")
                && self.reuse_if_matching().await
                && self.wait_healthy().await.is_ok()
            {
                return Ok(());
            }
            self.remove().await;
            return Err(SandboxError::TunnelUnavailable {
                reason: format!(
                    "sidecar container failed to start ({}): {reason}",
                    self.cfg.image()
                ),
            });
        }
        tracing::info!(
            container = SANDBOX_TUNNEL_CONTAINER,
            provider = self.cfg.provider.as_str(),
            "Sandbox tunnel sidecar started"
        );
        self.wait_healthy().await
    }

    /// True when a sidecar with our exact config hash is already running.
    async fn reuse_if_matching(&self) -> bool {
        let Some((running, hash)) = self.inspect().await else {
            return false;
        };
        running && hash == Some(self.cfg.config_hash())
    }

    /// `(running, hash-label)` of the existing sidecar container, if any.
    async fn inspect(&self) -> Option<(bool, Option<String>)> {
        let format = format!(
            "{{{{.State.Running}}}}|{{{{index .Config.Labels \"{SANDBOX_TUNNEL_HASH_LABEL}\"}}}}"
        );
        let output = crate::sandbox::cli_runner::output_isolated(
            self.bin,
            &["inspect", "--format", &format, SANDBOX_TUNNEL_CONTAINER],
        )
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
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty());
        Some((running, hash))
    }

    /// Poll the provider health probe until the startup timeout; on timeout
    /// remove the sidecar and fail closed.
    async fn wait_healthy(&self) -> Result<()> {
        let probe = build_health_args_for(self.cfg.provider);
        let arg_refs: Vec<&str> = probe.iter().map(String::as_str).collect();
        let deadline = tokio::time::Instant::now() + self.cfg.startup_timeout;
        loop {
            if let Some(status) =
                crate::sandbox::cli_runner::run_status_isolated(self.bin, &arg_refs).await
                && status.success()
            {
                tracing::info!(
                    container = SANDBOX_TUNNEL_CONTAINER,
                    provider = self.cfg.provider.as_str(),
                    "Sandbox tunnel sidecar healthy"
                );
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                self.remove().await;
                return Err(SandboxError::TunnelUnavailable {
                    reason: format!(
                        "sidecar did not become healthy within {}s (provider: {})",
                        self.cfg.startup_timeout.as_secs(),
                        self.cfg.provider.as_str()
                    ),
                });
            }
            tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
        }
    }

    /// Best-effort `rm -f -t 0` of the sidecar.
    pub async fn remove(&self) {
        let _ = crate::sandbox::cli_runner::run_status_isolated(
            self.bin,
            &["rm", "-f", "-t", "0", SANDBOX_TUNNEL_CONTAINER],
        )
        .await;
    }

    /// Stop the sidecar (graceful, then force).
    pub async fn stop(&self) {
        let _ = crate::sandbox::cli_runner::run_status_isolated(
            self.bin,
            &["rm", "-f", "-t", "10", SANDBOX_TUNNEL_CONTAINER],
        )
        .await;
        tracing::info!(
            container = SANDBOX_TUNNEL_CONTAINER,
            "Sandbox tunnel sidecar stopped"
        );
    }

    /// Write the NetBird secrets to a mode-0600 env file under the shared
    /// mount-source parent (resolvable from the manager-forked CLI context).
    /// Deleted when the returned guard drops — after `podman run` has read
    /// it. Returns `None` for the WireGuard provider (secrets live in the
    /// mounted config).
    fn write_secret_env_file(&self) -> Result<Option<SecretEnvFile>> {
        if self.cfg.provider != TunnelProvider::Netbird {
            return Ok(None);
        }
        let (url, key) = (
            self.cfg
                .netbird_management_url
                .as_deref()
                .unwrap_or_default(),
            self.cfg
                .netbird_setup_key
                .as_ref()
                .map(|k| k.expose_secret().to_string())
                .unwrap_or_default(),
        );
        SecretEnvFile::write(format!("NB_MANAGEMENT_URL={url}\nNB_SETUP_KEY={key}\n"))
            .map(Some)
            .map_err(|e| SandboxError::TunnelUnavailable {
                reason: format!("failed to stage netbird secret env file: {e}"),
            })
    }
}

/// Provider-specific health probe. NetBird reports peer readiness through
/// its CLI; WireGuard is healthy once its interface exists in the namespace.
fn build_health_args_for(provider: TunnelProvider) -> Vec<String> {
    match provider {
        TunnelProvider::Netbird => vec![
            "exec".into(),
            SANDBOX_TUNNEL_CONTAINER.into(),
            "netbird".into(),
            "status".into(),
            "--check".into(),
            "startup".into(),
        ],
        TunnelProvider::Wireguard => vec![
            "exec".into(),
            SANDBOX_TUNNEL_CONTAINER.into(),
            "ip".into(),
            "link".into(),
            "show".into(),
            "wg0".into(),
        ],
    }
}

/// Mode-0600 env file holding the NetBird credentials, removed on drop.
struct SecretEnvFile {
    path: PathBuf,
}

impl SecretEnvFile {
    fn write(content: String) -> std::io::Result<Self> {
        let base = crate::sandbox::cli_runner::shared_mount_source_parent();
        let path = base.join(format!(
            "lunarwing-sandbox-tunnel-env-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, content)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self { path })
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

    fn wireguard_cfg(conf: Option<PathBuf>) -> TunnelSidecarConfig {
        TunnelSidecarConfig {
            provider: TunnelProvider::Wireguard,
            wireguard_config: conf,
            ..TunnelSidecarConfig::default()
        }
    }

    #[test]
    fn provider_parsing() {
        assert_eq!(
            "netbird".parse::<TunnelProvider>().unwrap(),
            TunnelProvider::Netbird
        );
        assert_eq!(
            "WireGuard".parse::<TunnelProvider>().unwrap(),
            TunnelProvider::Wireguard
        );
        assert_eq!(
            "wg".parse::<TunnelProvider>().unwrap(),
            TunnelProvider::Wireguard
        );
        assert!("openvpn".parse::<TunnelProvider>().is_err());
    }

    #[test]
    fn default_images_per_provider() {
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
    fn netbird_args_use_env_file_not_argv() {
        let cfg = netbird_cfg();
        let args = build_sidecar_args(&cfg, Some(Path::new("/run/user/1000/lw-env")));
        let joined = args.join("\n");
        // Secrets and URL only ever travel via the env file reference.
        assert!(!joined.contains("setup-key-1"));
        assert!(!joined.contains("netbird.example.com"));
        assert!(joined.contains("--env-file\n/run/user/1000/lw-env"));
        assert!(joined.contains("--device\n/dev/net/tun"));
        assert!(joined.contains("--cap-add=NET_ADMIN"));
        assert!(joined.contains("--cap-drop=ALL"));
        assert!(joined.contains("docker.io/netbirdio/netbird:latest"));
        // Last positional is the netbird CLI subcommand.
        assert_eq!(*args.last().unwrap(), "up");
    }

    #[test]
    fn wireguard_args_mount_config_and_run_wg_quick() {
        let cfg = wireguard_cfg(Some(PathBuf::from("/etc/lunarwing/wg0.conf")));
        let args = build_sidecar_args(&cfg, None);
        let joined = args.join(" ");
        assert!(joined.contains("/etc/lunarwing/wg0.conf:/etc/wireguard/wg0.conf:ro,z"));
        assert!(!joined.contains("--device"));
        assert!(!joined.contains("--env-file"));
        assert!(joined.contains("wg-quick up /etc/wireguard/wg0.conf"));
        assert!(joined.contains("localhost/lunarwing-tunnel-wireguard:latest"));
    }

    #[test]
    fn sidecar_carries_own_label_namespace_and_hash() {
        let cfg = netbird_cfg();
        let args = build_sidecar_args(&cfg, None);
        let joined = args.join("\n");
        assert!(joined.contains(&format!("{SANDBOX_TUNNEL_CONTAINER_LABEL}=netbird")));
        assert!(joined.contains(&format!(
            "{SANDBOX_TUNNEL_HASH_LABEL}={}",
            cfg.config_hash()
        )));
        assert!(!joined.contains("lunarwing.sandbox="));
    }

    #[test]
    fn config_hash_changes_with_settings() {
        let base = netbird_cfg();
        let rotated_key = TunnelSidecarConfig {
            netbird_setup_key: Some(SecretString::from("setup-key-2")),
            ..base.clone()
        };
        let other_image = TunnelSidecarConfig {
            image: Some("localhost/mirror/netbird:latest".into()),
            ..base.clone()
        };
        let other_provider = wireguard_cfg(Some(PathBuf::from("/etc/wg0.conf")));
        assert_ne!(base.config_hash(), rotated_key.config_hash());
        assert_ne!(base.config_hash(), other_image.config_hash());
        assert_ne!(base.config_hash(), other_provider.config_hash());
        assert_eq!(base.config_hash(), netbird_cfg().config_hash());
    }

    #[test]
    fn validate_netbird_requires_url_and_key() {
        assert!(netbird_cfg().validate().is_ok());

        let missing_url = TunnelSidecarConfig {
            netbird_management_url: None,
            ..netbird_cfg()
        };
        let err = missing_url.validate().unwrap_err();
        assert!(err.contains("MANAGEMENT_URL"));

        let missing_key = TunnelSidecarConfig {
            netbird_setup_key: None,
            ..netbird_cfg()
        };
        assert!(missing_key.validate().unwrap_err().contains("SETUP_KEY"));

        let plain_http = TunnelSidecarConfig {
            netbird_management_url: Some("http://netbird.example.com".into()),
            ..netbird_cfg()
        };
        assert!(plain_http.validate().unwrap_err().contains("https"));
    }

    #[test]
    fn validate_wireguard_requires_owner_only_config() {
        let dir = std::env::temp_dir().join(format!("lw-tunnel-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // Missing path.
        assert!(
            wireguard_cfg(None)
                .validate()
                .unwrap_err()
                .contains("WIREGUARD_CONFIG")
        );

        // Nonexistent file.
        let missing = dir.join("missing.conf");
        assert!(
            wireguard_cfg(Some(missing.clone()))
                .validate()
                .unwrap_err()
                .contains("unreadable")
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let conf = dir.join("wg0.conf");
            let body = "[Interface]\nPrivateKey = abcdefg\nAddress = 10.10.0.2/32\n";
            std::fs::write(&conf, body).unwrap();

            // Group/other-readable: refused (holds the private key).
            std::fs::set_permissions(&conf, std::fs::Permissions::from_mode(0o644)).unwrap();
            let err = wireguard_cfg(Some(conf.clone())).validate().unwrap_err();
            assert!(err.contains("0600"), "expected 0600 guidance, got: {err}");

            // Owner-only + PrivateKey: accepted.
            std::fs::set_permissions(&conf, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert!(wireguard_cfg(Some(conf.clone())).validate().is_ok());

            // No PrivateKey: refused.
            std::fs::write(&conf, "[Interface]\nAddress = 10.10.0.2/32\n").unwrap();
            assert!(
                wireguard_cfg(Some(conf))
                    .validate()
                    .unwrap_err()
                    .contains("wg-quick")
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn health_probe_matches_provider() {
        let nb = build_health_args_for(TunnelProvider::Netbird);
        assert_eq!(
            nb.join(" "),
            "exec lunarwing-sandbox-tunnel netbird status --check startup"
        );
        let wg = build_health_args_for(TunnelProvider::Wireguard);
        assert_eq!(
            wg.join(" "),
            "exec lunarwing-sandbox-tunnel ip link show wg0"
        );
    }
}
