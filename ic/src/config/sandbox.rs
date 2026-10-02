use std::path::PathBuf;

use crate::config::helpers::{optional_env, parse_bool_env, parse_optional_env, parse_string_env};
use crate::error::ConfigError;
pub use lunarwing_common::PromptContextLimits;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use tracing;

/// Sandbox configuration (generic podman-first sandbox).
#[derive(Debug, Clone)]
pub struct SandboxModeConfig {
    /// Whether the sandbox is enabled.
    pub enabled: bool,
    /// Sandbox policy: "readonly", "workspace_write", or "full_access".
    pub policy: String,
    /// Container runtime CLI: "auto" (podman preferred), "podman", or "docker".
    pub runtime: String,
    /// Network egress mode for sandboxed policies: "none" (default),
    /// "proxy-enforced" (isolated allowlist web access), or legacy
    /// advisory "proxy".
    pub network: String,
    /// Force sandboxed shell execution even in dev mode (ALLOW_LOCAL_TOOLS).
    /// Default false: dev boxes keep direct host execution.
    pub enforce: bool,
    /// Explicit opt-in for `FullAccess` policy.
    ///
    /// When `policy` is `full_access` but this is `false`, the policy is
    /// downgraded to `workspace_write` with a loud error log. This prevents
    /// accidental host-level command execution from a single misconfigured
    /// env var.
    pub allow_full_access: bool,
    /// Command timeout in seconds.
    pub timeout_secs: u64,
    /// Memory limit in megabytes.
    pub memory_limit_mb: u64,
    /// CPU shares (relative weight).
    pub cpu_shares: u32,
    /// Container image for the sandbox.
    pub image: String,
    /// Whether to auto-pull the image if not found.
    pub auto_pull_image: bool,
    /// Additional domains to allow through the network proxy.
    pub extra_allowed_domains: Vec<String>,
    /// How often the reaper scans for orphaned containers (seconds). Default: 300 (5 min).
    pub reaper_interval_secs: u64,
    /// Containers older than this with no active job are reaped (seconds). Default: 600 (10 min).
    pub orphan_threshold_secs: u64,
    /// Tunnel sidecar settings (WI#248, `SANDBOX_NETWORK=tunnel`). Env-only
    /// — no Settings counterpart, like the reaper knobs.
    pub tunnel: SandboxTunnelModeConfig,
}

/// Env-resolved tunnel sidecar settings (raw form; validated when the
/// network mode is `tunnel`). See `docs/guides/SANDBOX_TUNNEL_NETWORKING.md`.
#[derive(Debug, Clone, Default)]
pub struct SandboxTunnelModeConfig {
    /// `netbird` (default) or `wireguard`.
    pub provider: Option<String>,
    /// Overrides the per-provider default sidecar image.
    pub image: Option<String>,
    /// NetBird management URL (`https://…`).
    pub netbird_management_url: Option<String>,
    /// NetBird setup key (secret).
    pub netbird_setup_key: Option<SecretString>,
    /// Path to a wg-quick-format config (mode 0600).
    pub wireguard_config: Option<PathBuf>,
    /// Health-gate timeout in seconds (default 90).
    pub startup_timeout_secs: Option<u64>,
}

impl SandboxTunnelModeConfig {
    fn resolve_env() -> Result<Self, ConfigError> {
        Ok(Self {
            provider: optional_env("SANDBOX_TUNNEL_PROVIDER")?,
            image: optional_env("SANDBOX_TUNNEL_IMAGE")?,
            netbird_management_url: optional_env("SANDBOX_TUNNEL_NETBIRD_MANAGEMENT_URL")?,
            netbird_setup_key: optional_env("SANDBOX_TUNNEL_NETBIRD_SETUP_KEY")?
                .map(SecretString::from),
            wireguard_config: optional_env("SANDBOX_TUNNEL_WIREGUARD_CONFIG")?.map(PathBuf::from),
            startup_timeout_secs: optional_env("SANDBOX_TUNNEL_STARTUP_TIMEOUT_SECS")?
                .map(|s| {
                    s.parse::<u64>().map_err(|_| ConfigError::InvalidValue {
                        key: "SANDBOX_TUNNEL_STARTUP_TIMEOUT_SECS".to_string(),
                        message: "must be a positive number of seconds".to_string(),
                    })
                })
                .transpose()?,
        })
    }

    /// Parsed module-level tunnel settings.
    fn to_tunnel_sidecar_config(&self) -> Result<crate::sandbox::TunnelSidecarConfig, ConfigError> {
        let provider = self.provider.as_deref().unwrap_or("netbird");
        let provider = provider
            .parse::<crate::sandbox::TunnelProvider>()
            .map_err(|e| ConfigError::InvalidValue {
                key: "SANDBOX_TUNNEL_PROVIDER".to_string(),
                message: e,
            })?;
        Ok(crate::sandbox::TunnelSidecarConfig {
            provider,
            image: self.image.clone(),
            netbird_management_url: self.netbird_management_url.clone(),
            netbird_setup_key: self.netbird_setup_key.clone(),
            wireguard_config: self.wireguard_config.clone(),
            startup_timeout: std::time::Duration::from_secs(
                self.startup_timeout_secs.unwrap_or(90),
            ),
        })
    }
}

impl Default for SandboxModeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            policy: "readonly".to_string(),
            runtime: "auto".to_string(),
            network: "none".to_string(),
            enforce: false,
            allow_full_access: false,
            timeout_secs: 120,
            memory_limit_mb: 2048,
            cpu_shares: 1024,
            image: "lunarwing-worker:latest".to_string(),
            auto_pull_image: true,
            extra_allowed_domains: Vec::new(),
            reaper_interval_secs: 300,
            orphan_threshold_secs: 600,
            tunnel: SandboxTunnelModeConfig::default(),
        }
    }
}

impl SandboxModeConfig {
    pub(crate) fn resolve(settings: &crate::settings::Settings) -> Result<Self, ConfigError> {
        let ss = &settings.sandbox;

        let extra_domains = optional_env("SANDBOX_EXTRA_DOMAINS")?
            .map(|s| s.split(',').map(|d| d.trim().to_string()).collect())
            .unwrap_or_else(|| {
                if ss.extra_allowed_domains.is_empty() {
                    Vec::new()
                } else {
                    ss.extra_allowed_domains.clone()
                }
            });

        // reaper/orphan fields have no Settings counterpart — env > default only.
        let reaper_interval_secs: u64 = parse_optional_env("SANDBOX_REAPER_INTERVAL_SECS", 300)?;
        let orphan_threshold_secs: u64 = parse_optional_env("SANDBOX_ORPHAN_THRESHOLD_SECS", 600)?;

        // Validate that reaper timings are non-zero to prevent tokio::time::interval panics
        if reaper_interval_secs == 0 {
            return Err(ConfigError::InvalidValue {
                key: "SANDBOX_REAPER_INTERVAL_SECS".to_string(),
                message: "must be greater than 0".to_string(),
            });
        }

        if orphan_threshold_secs == 0 {
            return Err(ConfigError::InvalidValue {
                key: "SANDBOX_ORPHAN_THRESHOLD_SECS".to_string(),
                message: "must be greater than 0".to_string(),
            });
        }

        let network = parse_string_env("SANDBOX_NETWORK", ss.network.clone())?;
        let tunnel = SandboxTunnelModeConfig::resolve_env()?;
        // Fail closed at resolve time: selecting the tunnel mode without a
        // complete provider configuration must stop startup with a named
        // knob, not surface later as per-command sandbox failures.
        if network.parse::<crate::sandbox::SandboxNetworkMode>()
            == Ok(crate::sandbox::SandboxNetworkMode::Tunnel)
        {
            tunnel
                .to_tunnel_sidecar_config()?
                .validate()
                .map_err(|message| ConfigError::InvalidValue {
                    key: "SANDBOX_TUNNEL_*".to_string(),
                    message,
                })?;
        }

        Ok(Self {
            enabled: parse_bool_env("SANDBOX_ENABLED", ss.enabled)?,
            policy: parse_string_env("SANDBOX_POLICY", ss.policy.clone())?,
            runtime: parse_string_env("SANDBOX_RUNTIME", ss.runtime.clone())?,
            network,
            // enforce: env > settings, default false.
            enforce: parse_bool_env("SANDBOX_ENFORCE", ss.enforce)?,
            // allow_full_access has no Settings counterpart — env > default only.
            allow_full_access: parse_bool_env("SANDBOX_ALLOW_FULL_ACCESS", false)?,
            timeout_secs: parse_optional_env("SANDBOX_TIMEOUT_SECS", ss.timeout_secs)?,
            memory_limit_mb: parse_optional_env("SANDBOX_MEMORY_LIMIT_MB", ss.memory_limit_mb)?,
            cpu_shares: parse_optional_env("SANDBOX_CPU_SHARES", ss.cpu_shares)?,
            image: parse_string_env("SANDBOX_IMAGE", ss.image.clone())?,
            auto_pull_image: parse_bool_env("SANDBOX_AUTO_PULL", ss.auto_pull_image)?,
            extra_allowed_domains: extra_domains,
            reaper_interval_secs,
            orphan_threshold_secs,
            tunnel,
        })
    }

    /// Convert to SandboxConfig for the sandbox module.
    ///
    /// If `policy` is `FullAccess` but `allow_full_access` is `false`,
    /// the policy is downgraded to `WorkspaceWrite` and an error is logged.
    pub fn to_sandbox_config(&self) -> crate::sandbox::SandboxConfig {
        use crate::sandbox::SandboxPolicy;
        use std::time::Duration;

        let mut policy = self.policy.parse().unwrap_or(SandboxPolicy::ReadOnly);

        // Double opt-in guard: FullAccess requires SANDBOX_ALLOW_FULL_ACCESS=true
        if policy == SandboxPolicy::FullAccess && !self.allow_full_access {
            tracing::error!(
                "SANDBOX_POLICY=full_access is set but SANDBOX_ALLOW_FULL_ACCESS is not \
                 set to 'true'. FullAccess bypasses the container and runs commands \
                 directly on the host. Downgrading to WorkspaceWrite for safety. Set \
                 SANDBOX_ALLOW_FULL_ACCESS=true to explicitly enable FullAccess."
            );
            policy = SandboxPolicy::WorkspaceWrite;
        }

        let runtime = self.runtime.parse().unwrap_or_else(|e| {
            tracing::error!(
                value = %self.runtime,
                error = %e,
                "Invalid SANDBOX_RUNTIME; falling back to 'auto' (podman preferred)"
            );
            crate::sandbox::SandboxRuntime::Auto
        });

        let network = self.network.parse().unwrap_or_else(|e| {
            tracing::error!(
                value = %self.network,
                error = %e,
                "Invalid SANDBOX_NETWORK; falling back to 'none'"
            );
            crate::sandbox::SandboxNetworkMode::None
        });

        // Tunnel settings ride along raw; validation already happened at
        // resolve time and the sidecar re-validates before starting.
        let tunnel = match self.tunnel.to_tunnel_sidecar_config() {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "Invalid SANDBOX_TUNNEL_* settings; ignoring (only used with \
                     SANDBOX_NETWORK=tunnel)"
                );
                None
            }
        };

        let mut allowlist = crate::sandbox::default_allowlist();
        allowlist.extend(self.extra_allowed_domains.clone());

        crate::sandbox::SandboxConfig {
            enabled: self.enabled,
            policy,
            allow_full_access: self.allow_full_access,
            timeout: Duration::from_secs(self.timeout_secs),
            memory_limit_mb: self.memory_limit_mb,
            cpu_shares: self.cpu_shares,
            network_allowlist: allowlist,
            image: self.image.clone(),
            auto_pull_image: self.auto_pull_image,
            proxy_port: 0, // Auto-assign
            runtime,
            network,
            tunnel,
            protected_paths: crate::sandbox::mount_guard::resolve_protected_paths(),
        }
    }
}

/// A single endpoint for a named external worker (URL + optional auth).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerEndpoint {
    pub url: String,
    // Secret-bearing: skipped by serde so JSON/API transports cannot expose it;
    // `Debug` auto-redacts via secrecy's `[REDACTED]`. `Settings::save_toml`
    // explicitly re-injects it into the on-disk config, while `merge_from`
    // preserves it across its JSON merge.
    #[serde(skip_serializing, default)]
    pub auth_token: Option<SecretString>,
    pub weight: Option<u32>,
}

/// Load balancing strategy for multi-instance workers.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LoadBalanceStrategy {
    #[default]
    RoundRobin,
    LeastConnections,
}

/// Configuration for a named external worker endpoint.
#[derive(Debug, Clone)]
pub struct ExternalWorkerConfig {
    pub name: String,
    pub url: String,
    pub auth_token: Option<SecretString>,
    pub timeout_ms: u64,
    /// Multiple endpoints for load-balanced workers.
    /// When non-empty, `url`/`auth_token` are treated as fallback only.
    pub endpoints: Vec<WorkerEndpoint>,
    /// Load balancing strategy (defaults to RoundRobin).
    pub load_balance: LoadBalanceStrategy,
    /// Bounds applied when recent conversation context is requested.
    pub prompt_context: PromptContextLimits,
}

impl ExternalWorkerConfig {
    pub fn resolve_from_settings(settings: &crate::settings::Settings) -> Vec<Self> {
        settings
            .sandbox
            .external_workers
            .iter()
            .map(|ew| {
                let prompt_context = ew.prompt_context.bounded();
                if prompt_context != ew.prompt_context {
                    tracing::warn!(
                        worker = %ew.name,
                        requested_max_messages = ew.prompt_context.max_messages,
                        requested_max_chars_per_message = ew.prompt_context.max_chars_per_message,
                        requested_max_total_chars = ew.prompt_context.max_total_chars,
                        "External worker prompt-context limits exceeded hard ceilings; clamped"
                    );
                }
                Self {
                    name: ew.name.clone(),
                    url: ew.url.clone(),
                    auth_token: ew.auth_token.clone(),
                    timeout_ms: ew.timeout_ms,
                    endpoints: ew.endpoints.clone(),
                    load_balance: ew.load_balance.clone(),
                    prompt_context,
                }
            })
            .collect()
    }

    /// Returns the canonical endpoint list for this worker.
    ///
    /// When `endpoints` is non-empty those are used directly; otherwise a
    /// single `WorkerEndpoint` is synthesized from the legacy `url` and
    /// `auth_token` fields.
    pub fn endpoints(&self) -> Vec<WorkerEndpoint> {
        if !self.endpoints.is_empty() {
            self.endpoints.clone()
        } else {
            vec![WorkerEndpoint {
                url: self.url.clone(),
                auth_token: self.auth_token.clone(),
                weight: None,
            }]
        }
    }
}

/// ACP (Agent Client Protocol) mode configuration.
#[derive(Debug, Clone)]
pub struct AcpModeConfig {
    pub enabled: bool,
    pub memory_limit_mb: u64,
    pub timeout_secs: u64,
}

impl Default for AcpModeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            memory_limit_mb: 4096,
            timeout_secs: 1800,
        }
    }
}

impl AcpModeConfig {
    pub fn from_env() -> Self {
        match Self::resolve_env_only() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Failed to resolve AcpModeConfig: {e}, using defaults");
                Self::default()
            }
        }
    }

    pub(crate) fn resolve(settings: &crate::settings::Settings) -> Result<Self, ConfigError> {
        let defaults = Self::default();
        Ok(Self {
            enabled: parse_bool_env("ACP_ENABLED", settings.sandbox.acp_enabled)?,
            memory_limit_mb: parse_optional_env("ACP_MEMORY_LIMIT_MB", defaults.memory_limit_mb)?,
            timeout_secs: parse_optional_env("ACP_TIMEOUT_SECS", defaults.timeout_secs)?,
        })
    }

    fn resolve_env_only() -> Result<Self, ConfigError> {
        let defaults = Self::default();
        Ok(Self {
            enabled: parse_bool_env("ACP_ENABLED", defaults.enabled)?,
            memory_limit_mb: parse_optional_env("ACP_MEMORY_LIMIT_MB", defaults.memory_limit_mb)?,
            timeout_secs: parse_optional_env("ACP_TIMEOUT_SECS", defaults.timeout_secs)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::config::sandbox::*;
    use secrecy::ExposeSecret;

    // ── SandboxModeConfig defaults ──────────────────────────────────

    #[test]
    fn sandbox_mode_config_default_values() {
        let cfg = SandboxModeConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.policy, "readonly");
        assert_eq!(cfg.timeout_secs, 120);
        assert_eq!(cfg.memory_limit_mb, 2048);
        assert_eq!(cfg.cpu_shares, 1024);
        assert_eq!(cfg.image, "lunarwing-worker:latest");
        assert!(cfg.auto_pull_image);
        assert!(cfg.extra_allowed_domains.is_empty());
    }

    #[test]
    fn sandbox_mode_config_custom_values() {
        let cfg = SandboxModeConfig {
            enabled: false,
            policy: "full_access".to_string(),
            runtime: "podman".to_string(),
            network: "none".to_string(),
            enforce: true,
            timeout_secs: 600,
            memory_limit_mb: 4096,
            cpu_shares: 512,
            image: "custom-worker:v2".to_string(),
            auto_pull_image: false,
            extra_allowed_domains: vec!["example.com".to_string()],
            reaper_interval_secs: 300,
            orphan_threshold_secs: 600,
            allow_full_access: false,
            tunnel: SandboxTunnelModeConfig::default(),
        };
        assert!(!cfg.enabled);
        assert_eq!(cfg.policy, "full_access");
        assert_eq!(cfg.runtime, "podman");
        assert_eq!(cfg.network, "none");
        assert!(cfg.enforce);
        assert_eq!(cfg.timeout_secs, 600);
        assert_eq!(cfg.memory_limit_mb, 4096);
        assert_eq!(cfg.cpu_shares, 512);
        assert_eq!(cfg.image, "custom-worker:v2");
        assert!(!cfg.auto_pull_image);
        assert_eq!(cfg.extra_allowed_domains, vec!["example.com"]);
    }

    #[test]
    fn sandbox_mode_to_sandbox_config_propagates_fields() {
        let mode = SandboxModeConfig {
            enabled: true,
            policy: "workspace_write".to_string(),
            runtime: "docker".to_string(),
            network: "none".to_string(),
            timeout_secs: 300,
            memory_limit_mb: 1024,
            cpu_shares: 2048,
            image: "test:latest".to_string(),
            auto_pull_image: false,
            extra_allowed_domains: vec!["custom.example.com".to_string()],
            reaper_interval_secs: 300,
            orphan_threshold_secs: 600,
            allow_full_access: false,
            enforce: false,
            tunnel: SandboxTunnelModeConfig::default(),
        };
        let sc = mode.to_sandbox_config();
        assert!(sc.enabled);
        assert_eq!(sc.policy, crate::sandbox::SandboxPolicy::WorkspaceWrite);
        assert_eq!(sc.timeout, std::time::Duration::from_secs(300));
        assert_eq!(sc.memory_limit_mb, 1024);
        assert_eq!(sc.cpu_shares, 2048);
        assert_eq!(sc.image, "test:latest");
        assert!(!sc.auto_pull_image);
        assert_eq!(sc.runtime, crate::sandbox::SandboxRuntime::Docker);
        assert_eq!(sc.network, crate::sandbox::SandboxNetworkMode::None);
        // extra domain should be in the allowlist
        assert!(
            sc.network_allowlist
                .contains(&"custom.example.com".to_string()),
            "expected custom domain in allowlist"
        );
    }

    #[test]
    fn sandbox_mode_invalid_runtime_network_fall_back() {
        let mode = SandboxModeConfig {
            runtime: "containerd".to_string(),
            network: "wide-open".to_string(),
            ..SandboxModeConfig::default()
        };
        let sc = mode.to_sandbox_config();
        assert_eq!(sc.runtime, crate::sandbox::SandboxRuntime::Auto);
        assert_eq!(sc.network, crate::sandbox::SandboxNetworkMode::None);
    }

    #[test]
    fn sandbox_network_default_and_enforced_mode_survive_resolution() {
        let default_cfg = SandboxModeConfig::default().to_sandbox_config();
        assert_eq!(
            default_cfg.network,
            crate::sandbox::SandboxNetworkMode::None
        );

        let enforced = SandboxModeConfig {
            network: "proxy-enforced".to_string(),
            ..SandboxModeConfig::default()
        };
        assert_eq!(
            enforced.to_sandbox_config().network,
            crate::sandbox::SandboxNetworkMode::ProxyEnforced
        );

        let legacy = SandboxModeConfig {
            network: "proxy".to_string(),
            ..SandboxModeConfig::default()
        };
        assert_eq!(
            legacy.to_sandbox_config().network,
            crate::sandbox::SandboxNetworkMode::Proxy
        );
    }

    #[test]
    fn sandbox_runtime_network_env_overrides_settings() {
        let _guard = crate::config::helpers::lock_env();
        let mut settings = crate::settings::Settings::default();
        settings.sandbox.runtime = "docker".to_string();
        settings.sandbox.network = "none".to_string();
        settings.sandbox.enforce = true;

        // SAFETY: Under ENV_MUTEX, no concurrent env access.
        unsafe { std::env::set_var("SANDBOX_RUNTIME", "podman") };
        let cfg = SandboxModeConfig::resolve(&settings).expect("resolve");
        unsafe { std::env::remove_var("SANDBOX_RUNTIME") };

        assert_eq!(cfg.runtime, "podman");
        // No env override for network: settings win.
        assert_eq!(cfg.network, "none");
        assert!(cfg.enforce);
    }

    // ── Tunnel mode resolution (WI#248) ──────────────────────────────

    /// A complete netbird tunnel configuration resolves and converts into
    /// module-level settings.
    #[test]
    fn tunnel_netbird_env_resolves_and_converts() {
        let _guard = crate::config::helpers::lock_env();
        let settings = crate::settings::Settings::default();

        // SAFETY: Under ENV_MUTEX, no concurrent env access.
        unsafe {
            std::env::set_var("SANDBOX_NETWORK", "tunnel");
            std::env::set_var(
                "SANDBOX_TUNNEL_NETBIRD_MANAGEMENT_URL",
                "https://nb.example.com",
            );
            std::env::set_var("SANDBOX_TUNNEL_NETBIRD_SETUP_KEY", "key-1");
        }
        let cfg = SandboxModeConfig::resolve(&settings).expect("resolve");
        unsafe {
            std::env::remove_var("SANDBOX_NETWORK");
            std::env::remove_var("SANDBOX_TUNNEL_NETBIRD_MANAGEMENT_URL");
            std::env::remove_var("SANDBOX_TUNNEL_NETBIRD_SETUP_KEY");
        }

        let sandbox = cfg.to_sandbox_config();
        assert_eq!(sandbox.network, crate::sandbox::SandboxNetworkMode::Tunnel);
        let tunnel = sandbox.tunnel.expect("tunnel settings present");
        assert_eq!(tunnel.provider, crate::sandbox::TunnelProvider::Netbird);
        assert_eq!(tunnel.image(), "docker.io/netbirdio/netbird:latest");
    }

    /// Selecting tunnel without the required provider knobs fails resolve
    /// with an error naming the missing setting — fail closed at startup.
    #[test]
    fn tunnel_missing_required_settings_fails_resolve() {
        let _guard = crate::config::helpers::lock_env();
        let settings = crate::settings::Settings::default();

        // SAFETY: Under ENV_MUTEX, no concurrent env access.
        unsafe { std::env::set_var("SANDBOX_NETWORK", "tunnel") };
        let err = SandboxModeConfig::resolve(&settings).unwrap_err();
        unsafe { std::env::remove_var("SANDBOX_NETWORK") };

        let msg = err.to_string();
        assert!(
            msg.contains("SETUP_KEY") || msg.contains("MANAGEMENT_URL"),
            "got: {msg}"
        );
    }

    /// Tunnel env settings are ignored (not an error) when the network mode
    /// is not tunnel.
    #[test]
    fn tunnel_env_ignored_without_tunnel_mode() {
        let _guard = crate::config::helpers::lock_env();
        let settings = crate::settings::Settings::default();

        // SAFETY: Under ENV_MUTEX, no concurrent env access.
        unsafe {
            std::env::set_var(
                "SANDBOX_TUNNEL_NETBIRD_MANAGEMENT_URL",
                "https://nb.example.com",
            )
        };
        let cfg = SandboxModeConfig::resolve(&settings).expect("resolve");
        unsafe { std::env::remove_var("SANDBOX_TUNNEL_NETBIRD_MANAGEMENT_URL") };

        let sandbox = cfg.to_sandbox_config();
        assert_eq!(sandbox.network, crate::sandbox::SandboxNetworkMode::None);
        assert!(
            sandbox.tunnel.is_some(),
            "raw settings still carried for later use"
        );
    }

    /// A bad provider name is rejected at resolve time.
    #[test]
    fn tunnel_invalid_provider_fails_resolve() {
        let _guard = crate::config::helpers::lock_env();
        let settings = crate::settings::Settings::default();

        // SAFETY: Under ENV_MUTEX, no concurrent env access.
        unsafe {
            std::env::set_var("SANDBOX_NETWORK", "tunnel");
            std::env::set_var("SANDBOX_TUNNEL_PROVIDER", "openvpn");
            std::env::set_var(
                "SANDBOX_TUNNEL_NETBIRD_MANAGEMENT_URL",
                "https://nb.example.com",
            );
            std::env::set_var("SANDBOX_TUNNEL_NETBIRD_SETUP_KEY", "key-1");
        }
        let err = SandboxModeConfig::resolve(&settings).unwrap_err();
        unsafe {
            std::env::remove_var("SANDBOX_NETWORK");
            std::env::remove_var("SANDBOX_TUNNEL_PROVIDER");
            std::env::remove_var("SANDBOX_TUNNEL_NETBIRD_MANAGEMENT_URL");
            std::env::remove_var("SANDBOX_TUNNEL_NETBIRD_SETUP_KEY");
        }

        assert!(
            err.to_string().contains("SANDBOX_TUNNEL_PROVIDER"),
            "got: {err}"
        );
    }

    #[test]
    fn sandbox_enforce_env_overrides_default() {
        let _guard = crate::config::helpers::lock_env();
        let settings = crate::settings::Settings::default();
        assert!(!settings.sandbox.enforce);

        // SAFETY: Under ENV_MUTEX, no concurrent env access.
        unsafe { std::env::set_var("SANDBOX_ENFORCE", "true") };
        let cfg = SandboxModeConfig::resolve(&settings).expect("resolve");
        unsafe { std::env::remove_var("SANDBOX_ENFORCE") };

        assert!(cfg.enforce);
    }

    #[test]
    fn sandbox_mode_to_sandbox_config_invalid_policy_falls_back_to_readonly() {
        let mode = SandboxModeConfig {
            policy: "garbage_value".to_string(),
            ..SandboxModeConfig::default()
        };
        let sc = mode.to_sandbox_config();
        assert_eq!(sc.policy, crate::sandbox::SandboxPolicy::ReadOnly);
    }

    #[test]
    fn sandbox_mode_to_sandbox_config_includes_default_allowlist() {
        let mode = SandboxModeConfig::default();
        let sc = mode.to_sandbox_config();
        // The default allowlist from sandbox module should be non-empty
        assert!(
            !sc.network_allowlist.is_empty(),
            "default allowlist should not be empty"
        );
    }

    #[test]
    fn test_full_access_downgraded_without_allow() {
        let config = SandboxModeConfig {
            policy: "full_access".to_string(),
            allow_full_access: false,
            ..Default::default()
        };
        let sandbox = config.to_sandbox_config();
        // Should have been downgraded to WorkspaceWrite
        assert_eq!(
            sandbox.policy,
            crate::sandbox::SandboxPolicy::WorkspaceWrite
        );
        assert!(!sandbox.allow_full_access);
    }

    #[test]
    fn test_full_access_allowed_with_explicit_opt_in() {
        let config = SandboxModeConfig {
            policy: "full_access".to_string(),
            allow_full_access: true,
            ..Default::default()
        };
        let sandbox = config.to_sandbox_config();
        assert_eq!(sandbox.policy, crate::sandbox::SandboxPolicy::FullAccess);
        assert!(sandbox.allow_full_access);
    }

    #[test]
    fn test_non_full_access_policy_unaffected() {
        let config = SandboxModeConfig {
            policy: "workspace_write".to_string(),
            allow_full_access: false,
            ..Default::default()
        };
        let sandbox = config.to_sandbox_config();
        assert_eq!(
            sandbox.policy,
            crate::sandbox::SandboxPolicy::WorkspaceWrite
        );
    }

    // ── Settings fallback tests ──────────────────────────────────────

    #[test]
    fn sandbox_resolve_falls_back_to_settings() {
        let _guard = crate::config::helpers::lock_env();
        let mut settings = crate::settings::Settings::default();
        settings.sandbox.cpu_shares = 99;
        settings.sandbox.auto_pull_image = false;
        settings.sandbox.enabled = false;

        let cfg = SandboxModeConfig::resolve(&settings).expect("resolve");
        assert!(!cfg.enabled);
        assert_eq!(cfg.cpu_shares, 99);
        assert!(!cfg.auto_pull_image);
    }

    #[test]
    fn sandbox_env_overrides_settings() {
        let _guard = crate::config::helpers::lock_env();
        let mut settings = crate::settings::Settings::default();
        settings.sandbox.timeout_secs = 999;

        // SAFETY: Under ENV_MUTEX, no concurrent env access.
        unsafe { std::env::set_var("SANDBOX_TIMEOUT_SECS", "5") };
        let cfg = SandboxModeConfig::resolve(&settings).expect("resolve");
        unsafe { std::env::remove_var("SANDBOX_TIMEOUT_SECS") };

        assert_eq!(cfg.timeout_secs, 5);
    }

    #[test]
    fn test_readonly_policy_unaffected() {
        let config = SandboxModeConfig {
            policy: "readonly".to_string(),
            allow_full_access: false,
            ..Default::default()
        };
        let sandbox = config.to_sandbox_config();
        assert_eq!(sandbox.policy, crate::sandbox::SandboxPolicy::ReadOnly);
    }

    // ── External worker config (mt-admin config.toml contract) ───────────

    /// The exact `config.toml` block `lunarwing-mt-admin.sh` writes for a
    /// tenant's nanocode worker must deserialize into an `ExternalWorkerConfig`
    /// the daemon can route to. Guards the script↔daemon contract: if a field
    /// name (`external_workers`, `auth_token`, `timeout_ms`) or the nesting
    /// (`[[sandbox.external_workers]]`) drifts, the worker would silently fail
    /// to load and `create_job(mode: "nanocode")` would have nothing to route
    /// to — the exact bug in docs/bugs/MISSING-CONFIG-FOR-NANOCODE.md.
    #[test]
    fn external_worker_config_matches_mt_admin_output() {
        // Mirrors ensure_external_worker_config() — including the minimal
        // fresh-file form add-tenant writes (only the external-workers block;
        // every other Settings field falls back to its default).
        let generated = r#"
# LunarWing tenant configuration (auto-generated by lunarwing-mt-admin.sh).

# External worker: nanocode — create_job(mode: "nanocode")
[[sandbox.external_workers]]
name = "nanocode"
url = "ws://127.0.0.1:10007/ws/agent"
auth_token = "deadbeefcafe1234"
timeout_ms = 3600000
"#;

        let settings: crate::settings::Settings =
            toml::from_str(generated).expect("mt-admin config.toml must parse");

        let workers = ExternalWorkerConfig::resolve_from_settings(&settings);
        assert_eq!(workers.len(), 1, "exactly one external worker expected");
        assert_eq!(workers[0].name, "nanocode");
        assert_eq!(workers[0].url, "ws://127.0.0.1:10007/ws/agent");
        assert_eq!(
            workers[0].auth_token.as_ref().map(|s| s.expose_secret()),
            Some("deadbeefcafe1234")
        );
        assert_eq!(workers[0].timeout_ms, 3_600_000);
        assert_eq!(workers[0].prompt_context, PromptContextLimits::default());
    }

    #[test]
    fn external_worker_prompt_context_limits_are_clamped() {
        let generated = r#"
[[sandbox.external_workers]]
name = "opencode"
url = "ws://127.0.0.1:10007/ws/agent"

[sandbox.external_workers.prompt_context]
max_messages = 500
max_chars_per_message = 100000
max_total_chars = 500000
"#;
        let settings: crate::settings::Settings = toml::from_str(generated).expect("config parses");
        let workers = ExternalWorkerConfig::resolve_from_settings(&settings);

        assert_eq!(
            workers[0].prompt_context,
            lunarwing_common::HARD_PROMPT_CONTEXT_LIMITS
        );
    }

    /// Multiple `[[sandbox.external_workers]]` blocks in one file (e.g. nanocode
    /// plus pebble) must all resolve. Mirrors a tenant config that has had a
    /// second worker appended, guarding the array-of-tables append path the
    /// script relies on.
    #[test]
    fn external_worker_config_supports_multiple_workers() {
        let generated = r#"
[[sandbox.external_workers]]
name = "nanocode"
url = "ws://127.0.0.1:10007/ws/agent"
auth_token = "tok-nano"
timeout_ms = 3600000

[[sandbox.external_workers]]
name = "pebble"
url = "ws://127.0.0.1:10008/ws/agent"
auth_token = "tok-pebble"
timeout_ms = 3600000
"#;

        let settings: crate::settings::Settings =
            toml::from_str(generated).expect("multi-worker config.toml must parse");

        let workers = ExternalWorkerConfig::resolve_from_settings(&settings);
        let names: Vec<&str> = workers.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(workers.len(), 2);
        assert!(names.contains(&"nanocode"));
        assert!(names.contains(&"pebble"));
    }

    #[test]
    fn external_worker_config_multi_endpoint() {
        let toml_str = r#"
[[sandbox.external_workers]]
name = "nanocode"
url = "ws://127.0.0.1:10007/ws/agent"
auth_token = "tok-legacy"
timeout_ms = 3600000

[[sandbox.external_workers.endpoints]]
url = "ws://10.0.0.1:9090/ws/agent"
auth_token = "tok-1"

[[sandbox.external_workers.endpoints]]
url = "ws://10.0.0.2:9090/ws/agent"
auth_token = "tok-2"
"#;

        let settings: crate::settings::Settings =
            toml::from_str(toml_str).expect("multi-endpoint config must parse");
        let workers = ExternalWorkerConfig::resolve_from_settings(&settings);
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].endpoints.len(), 2);
        assert_eq!(workers[0].endpoints[0].url, "ws://10.0.0.1:9090/ws/agent");
        assert_eq!(workers[0].endpoints[1].url, "ws://10.0.0.2:9090/ws/agent");
        assert!(matches!(
            workers[0].load_balance,
            LoadBalanceStrategy::RoundRobin
        ));
    }

    #[test]
    fn external_worker_config_legacy_fallback() {
        let toml_str = r#"
[[sandbox.external_workers]]
name = "pebble"
url = "ws://127.0.0.1:8443/ws/agent"
auth_token = "tok-legacy"
timeout_ms = 3600000
"#;

        let settings: crate::settings::Settings =
            toml::from_str(toml_str).expect("legacy config must parse");
        let workers = ExternalWorkerConfig::resolve_from_settings(&settings);
        assert_eq!(workers.len(), 1);
        assert!(workers[0].endpoints.is_empty());

        let endpoints = workers[0].endpoints();
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].url, "ws://127.0.0.1:8443/ws/agent");
        assert_eq!(
            endpoints[0].auth_token.as_ref().map(|s| s.expose_secret()),
            Some("tok-legacy")
        );
    }
}
