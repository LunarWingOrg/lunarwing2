use crate::config::helpers::optional_env;
use crate::error::ConfigError;
use crate::settings::Settings;

/// Tunnel configuration for exposing the agent to the internet.
///
/// Used by channels and tools that need public webhook endpoints.
/// The tunnel URL is shared across all channels (XMPP, etc.).
///
/// Two modes:
/// - **Static URL** (`TUNNEL_URL`): set the public URL directly (manual tunnel)
/// - **Managed provider** (`TUNNEL_PROVIDER`): lifecycle-managed tunnel process
///
/// When a managed provider is configured _and_ no static URL is set,
/// the gateway starts the tunnel on boot and populates `public_url`.
#[derive(Debug, Clone, Default)]
pub struct TunnelConfig {
    /// Public URL from a tunnel provider (e.g., "https://tunnel.example.com").
    /// Set statically via `TUNNEL_URL` or populated at runtime by a managed tunnel.
    pub public_url: Option<String>,
    /// Provider configuration for lifecycle-managed tunnels.
    /// `None` when using a static URL or no tunnel at all.
    pub provider: Option<crate::tunnel::TunnelProviderConfig>,
}

impl TunnelConfig {
    pub(crate) fn resolve(settings: &Settings) -> Result<Self, ConfigError> {
        let public_url = optional_env("TUNNEL_URL")?
            .or_else(|| settings.tunnel.public_url.clone().filter(|s| !s.is_empty()));

        if let Some(ref url) = public_url {
            validate_public_url(url)?;
        }

        // Resolve managed tunnel provider config.
        // Priority: env var > settings > default (none).
        let provider_name = optional_env("TUNNEL_PROVIDER")?
            .or_else(|| settings.tunnel.provider.clone())
            .unwrap_or_default();

        let provider =
            if public_url.is_some() || provider_name.is_empty() || provider_name == "none" {
                None
            } else {
                crate::tunnel::validate_provider_name(&provider_name).map_err(|message| {
                    ConfigError::InvalidValue {
                        key: "TUNNEL_PROVIDER".to_string(),
                        message,
                    }
                })?;

                let netbird_name_prefix = optional_env("TUNNEL_NETBIRD_NAME_PREFIX")?
                    .or_else(|| settings.tunnel.netbird_name_prefix.clone())
                    .unwrap_or_else(|| "lunarwing".to_string());
                if provider_name == "netbird" {
                    crate::tunnel::validate_netbird_name_prefix(&netbird_name_prefix).map_err(
                        |error| ConfigError::InvalidValue {
                            key: "TUNNEL_NETBIRD_NAME_PREFIX".to_string(),
                            message: error.to_string(),
                        },
                    )?;
                }

                let custom = {
                    let health_url = optional_env("TUNNEL_CUSTOM_HEALTH_URL")?
                        .or_else(|| settings.tunnel.custom_health_url.clone());
                    let url_pattern = optional_env("TUNNEL_CUSTOM_URL_PATTERN")?
                        .or_else(|| settings.tunnel.custom_url_pattern.clone());
                    optional_env("TUNNEL_CUSTOM_COMMAND")?
                        .or_else(|| settings.tunnel.custom_command.clone())
                        .map(|start_command| crate::tunnel::CustomTunnelConfig {
                            start_command,
                            health_url,
                            url_pattern,
                        })
                };
                if provider_name == "custom"
                    && custom
                        .as_ref()
                        .is_none_or(|config| config.start_command.trim().is_empty())
                {
                    return Err(ConfigError::InvalidValue {
                        key: "TUNNEL_CUSTOM_COMMAND".to_string(),
                        message: "custom tunnel provider requires a non-empty command".to_string(),
                    });
                }

                Some(crate::tunnel::TunnelProviderConfig {
                    provider: provider_name.clone(),
                    netbird: Some(crate::tunnel::NetBirdTunnelConfig {
                        custom_domain: optional_env("TUNNEL_NETBIRD_CUSTOM_DOMAIN")?
                            .or_else(|| settings.tunnel.netbird_custom_domain.clone()),
                        name_prefix: netbird_name_prefix,
                    }),
                    custom,
                })
            };

        Ok(Self {
            public_url,
            provider,
        })
    }

    /// Check if a tunnel is configured (static URL or managed provider).
    pub fn is_enabled(&self) -> bool {
        self.public_url.is_some() || self.provider.is_some()
    }

    /// Get the webhook URL for a given path.
    pub fn webhook_url(&self, path: &str) -> Option<String> {
        self.public_url.as_ref().map(|base| {
            let base = base.trim_end_matches('/');
            let path = path.trim_start_matches('/');
            format!("{}/{}", base, path)
        })
    }
}

pub(crate) fn validate_public_url(url: &str) -> Result<(), ConfigError> {
    if !url.starts_with("https://") {
        return Err(ConfigError::InvalidValue {
            key: "TUNNEL_URL".to_string(),
            message: "must start with https:// (webhooks require HTTPS)".to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::config::tunnel::TunnelConfig;
    use crate::settings::Settings;
    use crate::tunnel::{CustomTunnelConfig, NetBirdTunnelConfig, TunnelProviderConfig};

    struct TunnelEnvGuard(Vec<(&'static str, Option<String>)>);

    impl TunnelEnvGuard {
        fn clear() -> Self {
            let keys = [
                "TUNNEL_URL",
                "TUNNEL_PROVIDER",
                "TUNNEL_NETBIRD_CUSTOM_DOMAIN",
                "TUNNEL_NETBIRD_NAME_PREFIX",
                "TUNNEL_CUSTOM_COMMAND",
                "TUNNEL_CUSTOM_HEALTH_URL",
                "TUNNEL_CUSTOM_URL_PATTERN",
            ];
            let previous = keys
                .into_iter()
                .map(|key| (key, std::env::var(key).ok()))
                .collect();
            // SAFETY: Callers hold the shared environment lock.
            unsafe {
                for key in keys {
                    std::env::remove_var(key);
                }
            }
            Self(previous)
        }
    }

    impl Drop for TunnelEnvGuard {
        fn drop(&mut self) {
            // SAFETY: Guard is dropped while caller still holds environment lock.
            unsafe {
                for (key, value) in &self.0 {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }

    // ── Default ─────────────────────────────────────────────────────

    #[test]
    fn default_is_disabled() {
        let cfg = TunnelConfig::default();
        assert!(cfg.public_url.is_none());
        assert!(cfg.provider.is_none());
        assert!(!cfg.is_enabled());
    }

    #[test]
    fn removed_provider_fails_without_static_url() {
        let _env_lock = crate::config::helpers::lock_env();
        let _env = TunnelEnvGuard::clear();
        let mut settings = Settings::default();
        settings.tunnel.provider = Some("ngrok".to_string());

        let error = TunnelConfig::resolve(&settings).unwrap_err();
        assert!(error.to_string().contains("was removed"));
    }

    #[test]
    fn static_url_ignores_removed_provider_marker() {
        let _env_lock = crate::config::helpers::lock_env();
        let _env = TunnelEnvGuard::clear();
        let mut settings = Settings::default();
        settings.tunnel.public_url = Some("https://tunnel.example.com".to_string());
        settings.tunnel.provider = Some("ngrok".to_string());

        let config = TunnelConfig::resolve(&settings).unwrap();
        assert_eq!(
            config.public_url.as_deref(),
            Some("https://tunnel.example.com")
        );
        assert!(config.provider.is_none());
    }

    #[test]
    fn custom_provider_requires_command() {
        let _env_lock = crate::config::helpers::lock_env();
        let _env = TunnelEnvGuard::clear();
        let mut settings = Settings::default();
        settings.tunnel.provider = Some("custom".to_string());

        let error = TunnelConfig::resolve(&settings).unwrap_err();
        assert!(error.to_string().contains("TUNNEL_CUSTOM_COMMAND"));
    }

    #[test]
    fn netbird_provider_rejects_invalid_prefix_during_config_resolution() {
        let _env_lock = crate::config::helpers::lock_env();
        let _env = TunnelEnvGuard::clear();
        let mut settings = Settings::default();
        settings.tunnel.provider = Some("netbird".to_string());
        settings.tunnel.netbird_name_prefix = Some("Invalid Prefix".to_string());

        let error = TunnelConfig::resolve(&settings).unwrap_err();
        assert!(error.to_string().contains("TUNNEL_NETBIRD_NAME_PREFIX"));
    }

    // ── is_enabled ──────────────────────────────────────────────────

    #[test]
    fn is_enabled_with_static_url() {
        let cfg = TunnelConfig {
            public_url: Some("https://tunnel.example.com".to_string()),
            provider: None,
        };
        assert!(cfg.is_enabled());
    }

    #[test]
    fn is_enabled_with_provider() {
        let cfg = TunnelConfig {
            public_url: None,
            provider: Some(TunnelProviderConfig {
                provider: "netbird".to_string(),
                netbird: Some(NetBirdTunnelConfig::default()),
                custom: None,
            }),
        };
        assert!(cfg.is_enabled());
    }

    #[test]
    fn is_enabled_with_both() {
        let cfg = TunnelConfig {
            public_url: Some("https://example.com".to_string()),
            provider: Some(TunnelProviderConfig {
                provider: "custom".to_string(),
                netbird: None,
                custom: Some(CustomTunnelConfig {
                    start_command: "example-tunnel {port}".to_string(),
                    health_url: None,
                    url_pattern: None,
                }),
            }),
        };
        assert!(cfg.is_enabled());
    }

    // ── webhook_url ─────────────────────────────────────────────────

    #[test]
    fn webhook_url_none_when_no_public_url() {
        let cfg = TunnelConfig::default();
        assert!(cfg.webhook_url("/hook").is_none());
    }

    #[test]
    fn webhook_url_basic() {
        let cfg = TunnelConfig {
            public_url: Some("https://tunnel.example.com".to_string()),
            provider: None,
        };
        assert_eq!(
            cfg.webhook_url("/webhook/xmpp"),
            Some("https://tunnel.example.com/webhook/xmpp".to_string())
        );
    }

    #[test]
    fn webhook_url_trims_trailing_slash_on_base() {
        let cfg = TunnelConfig {
            public_url: Some("https://tunnel.example.com/".to_string()),
            provider: None,
        };
        assert_eq!(
            cfg.webhook_url("/hook"),
            Some("https://tunnel.example.com/hook".to_string())
        );
    }

    #[test]
    fn webhook_url_trims_leading_slash_on_path() {
        let cfg = TunnelConfig {
            public_url: Some("https://tunnel.example.com".to_string()),
            provider: None,
        };
        // Path without leading slash should also work
        assert_eq!(
            cfg.webhook_url("hook"),
            Some("https://tunnel.example.com/hook".to_string())
        );
    }

    #[test]
    fn webhook_url_double_slash_normalization() {
        let cfg = TunnelConfig {
            public_url: Some("https://tunnel.example.com/".to_string()),
            provider: None,
        };
        // Both base trailing and path leading slashes trimmed
        assert_eq!(
            cfg.webhook_url("/api/webhook"),
            Some("https://tunnel.example.com/api/webhook".to_string())
        );
    }

    #[test]
    fn webhook_url_empty_path() {
        let cfg = TunnelConfig {
            public_url: Some("https://tunnel.example.com".to_string()),
            provider: None,
        };
        assert_eq!(
            cfg.webhook_url(""),
            Some("https://tunnel.example.com/".to_string())
        );
    }

    // ── TunnelProviderConfig field coverage ─────────────────────────

    #[test]
    fn provider_config_netbird() {
        let netbird = NetBirdTunnelConfig {
            custom_domain: Some("lunarwing.example.com".to_string()),
            name_prefix: "lunarwing".to_string(),
        };
        assert_eq!(
            netbird.custom_domain.as_deref(),
            Some("lunarwing.example.com")
        );
        assert_eq!(netbird.name_prefix, "lunarwing");
    }

    #[test]
    fn provider_config_netbird_defaults() {
        let netbird = NetBirdTunnelConfig::default();
        assert!(netbird.custom_domain.is_none());
        assert_eq!(netbird.name_prefix, "lunarwing");
    }

    #[test]
    fn provider_config_custom() {
        let c = CustomTunnelConfig {
            start_command: "bore local {port}".to_string(),
            health_url: Some("http://localhost:8080/health".to_string()),
            url_pattern: Some("https://bore.pub".to_string()),
        };
        assert_eq!(c.start_command, "bore local {port}");
        assert!(c.health_url.is_some());
        assert!(c.url_pattern.is_some());
    }

    #[test]
    fn provider_config_custom_defaults() {
        let c = CustomTunnelConfig::default();
        assert!(c.start_command.is_empty());
        assert!(c.health_url.is_none());
        assert!(c.url_pattern.is_none());
    }
}
