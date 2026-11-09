//! Channel management CLI commands.
//!
//! Lists configured messaging channels and their status.
//! Enable/disable/status subcommands are deferred pending channel config source
//! unification (see module-level note below).
//!
//! ## Why only `list` for now
//!
//! `enable`/`disable` require modifying channel configuration, but the config
//! source is currently split: built-in channels (cli, http, gateway, signal)
//! are resolved from environment variables in `ChannelsConfig::resolve()`,
//! while `settings.channels.*` fields are not consumed by that path.
//! Until `resolve()` falls back to settings (or the CLI writes `.env`),
//! an `enable`/`disable` command would silently fail to take effect.
//!
//! When the configured gateway is reachable and has an auth token, `list`
//! overlays failed runtime starts from its authenticated health endpoint.
//! Without that connection it retains the static configured-state view.

use std::path::Path;

use clap::Subcommand;

#[derive(Subcommand, Debug, Clone)]
pub enum ChannelsCommand {
    /// List all configured channels
    List {
        /// Show detailed information (host, port, config source)
        #[arg(short, long)]
        verbose: bool,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

/// Run the channels CLI subcommand.
pub async fn run_channels_command(
    cmd: ChannelsCommand,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    let config = crate::config::Config::from_env_with_toml(config_path)
        .await
        .map_err(|e| anyhow::anyhow!("{e:#}"))?;

    match cmd {
        ChannelsCommand::List { verbose, json } => cmd_list(&config.channels, verbose, json).await,
    }
}

/// Channel entry for display.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChannelInfo {
    name: String,
    kind: &'static str,
    enabled: bool,
    details: Vec<(&'static str, String)>,
}

#[derive(serde::Deserialize)]
struct GatewayStatusResponse {
    channel_health: Option<std::collections::HashMap<String, ChannelHealthEntry>>,
}

#[derive(serde::Deserialize)]
struct ChannelHealthEntry {
    healthy: bool,
    error: Option<String>,
}

/// Fetch failed runtime channels when the local gateway is configured and reachable.
pub(crate) async fn gateway_channel_errors(
    config: &crate::config::ChannelsConfig,
) -> Option<std::collections::HashMap<String, String>> {
    let gateway = config.gateway.as_ref()?;
    let token = gateway
        .auth_token
        .as_deref()
        .filter(|token| !token.trim().is_empty())
        .map(str::to_string)
        .or_else(|| std::env::var("GATEWAY_AUTH_TOKEN").ok())?;
    let endpoint = format!(
        "http://{}:{}/api/gateway/status",
        gateway.host, gateway.port
    );
    let response = reqwest::Client::new()
        .get(endpoint)
        .bearer_auth(token.trim())
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let status = response.json::<GatewayStatusResponse>().await.ok()?;
    Some(
        status
            .channel_health
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, entry)| !entry.healthy)
            .map(|(name, entry)| {
                let error = entry
                    .error
                    .unwrap_or_else(|| "channel failed to start; see daemon logs".to_string());
                (name, error)
            })
            .collect(),
    )
}

fn builtin_channels(config: &crate::config::ChannelsConfig) -> Vec<ChannelInfo> {
    let mut channels = Vec::new();

    // Built-in: CLI
    channels.push(ChannelInfo {
        name: "cli".to_string(),
        kind: "built-in",
        enabled: config.cli.enabled,
        details: vec![],
    });

    // Built-in: Gateway
    if let Some(ref gw) = config.gateway {
        channels.push(ChannelInfo {
            name: "gateway".to_string(),
            kind: "built-in",
            enabled: true,
            details: vec![("host", gw.host.clone()), ("port", gw.port.to_string())],
        });
    } else {
        channels.push(ChannelInfo {
            name: "gateway".to_string(),
            kind: "built-in",
            enabled: false,
            details: vec![],
        });
    }

    // Built-in: HTTP webhook
    if let Some(ref http) = config.http {
        channels.push(ChannelInfo {
            name: "http".to_string(),
            kind: "built-in",
            enabled: true,
            details: vec![("host", http.host.clone()), ("port", http.port.to_string())],
        });
    } else {
        channels.push(ChannelInfo {
            name: "http".to_string(),
            kind: "built-in",
            enabled: false,
            details: vec![],
        });
    }

    // Built-in: Signal
    if let Some(ref sig) = config.signal {
        channels.push(ChannelInfo {
            name: "signal".to_string(),
            kind: "built-in",
            enabled: true,
            details: vec![
                ("http_url", sig.http_url.clone()),
                ("account", sig.account.clone()),
                ("dm_policy", sig.dm_policy.clone()),
                ("group_policy", sig.group_policy.clone()),
            ],
        });
    } else {
        channels.push(ChannelInfo {
            name: "signal".to_string(),
            kind: "built-in",
            enabled: false,
            details: vec![],
        });
    }

    channels
}

/// List all configured channels.
async fn cmd_list(
    config: &crate::config::ChannelsConfig,
    verbose: bool,
    json: bool,
) -> anyhow::Result<()> {
    let mut channels = builtin_channels(config);

    // WASM channels: scan directory
    if config.wasm_channels_enabled {
        let wasm_channels = discover_wasm_channels(&config.wasm_channels_dir).await;
        for name in wasm_channels {
            let owner = config.wasm_channel_owner_ids.get(&name);
            let mut details = vec![];
            if let Some(id) = owner {
                details.push(("owner_id", id.to_string()));
            }
            channels.push(ChannelInfo {
                name,
                kind: "wasm",
                enabled: true,
                details,
            });
        }
    }

    let runtime_errors = gateway_channel_errors(config).await;

    if json {
        let entries: Vec<serde_json::Value> = channels
            .iter()
            .map(|channel| {
                let error = runtime_errors
                    .as_ref()
                    .and_then(|errors| errors.get(&channel.name));
                channel_to_json(channel, verbose, error)
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".to_string())
        );
    } else {
        print_channels_text(&channels, verbose, runtime_errors.as_ref());
    }

    Ok(())
}

fn channel_to_json(
    channel: &ChannelInfo,
    verbose: bool,
    runtime_error: Option<&String>,
) -> serde_json::Value {
    let mut value = serde_json::json!({
        "name": channel.name,
        "kind": channel.kind,
        "enabled": channel.enabled,
    });
    if let Some(error) = runtime_error {
        value["active"] = serde_json::Value::Bool(false);
        value["activation_error"] = serde_json::Value::String(error.clone());
    }
    if verbose {
        let details: serde_json::Map<String, serde_json::Value> = channel
            .details
            .iter()
            .map(|(key, value)| (key.to_string(), serde_json::Value::String(value.clone())))
            .collect();
        value["details"] = serde_json::Value::Object(details);
    }
    value
}

fn print_channels_text(
    channels: &[ChannelInfo],
    verbose: bool,
    runtime_errors: Option<&std::collections::HashMap<String, String>>,
) {
    let enabled_count = channels.iter().filter(|channel| channel.enabled).count();
    println!(
        "Configured channels ({} enabled, {} total):\n",
        enabled_count,
        channels.len()
    );

    for channel in channels {
        let runtime_error = runtime_errors.and_then(|errors| errors.get(&channel.name));
        let status = if runtime_error.is_some() {
            "failed"
        } else if channel.enabled {
            "enabled"
        } else {
            "disabled"
        };
        print_channel_text(channel, status, verbose, runtime_error);
    }

    if !verbose {
        println!();
        println!("Use --verbose for details.");
        println!();
        println!("Note: enable/disable not yet available.");
        println!("Built-in channels are managed via environment variables and installable");
        println!("channels are configured through the extensions lifecycle.");
    }
}

fn print_channel_text(
    channel: &ChannelInfo,
    status: &str,
    verbose: bool,
    runtime_error: Option<&String>,
) {
    if verbose {
        println!("  {} [{}] ({})", channel.name, status, channel.kind);
        for (key, value) in &channel.details {
            println!("    {}: {}", key, value);
        }
        if let Some(error) = runtime_error {
            println!("    {error}");
        } else if channel.details.is_empty() && channel.enabled {
            println!("    (default config)");
        }
        println!();
        return;
    }

    let details = if let Some(error) = runtime_error {
        format!("  ({error})")
    } else if channel.enabled && !channel.details.is_empty() {
        let parts: Vec<String> = channel
            .details
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        format!("  ({})", parts.join(", "))
    } else {
        String::new()
    };
    println!(
        "  {:<16} {:<10} {:<10}{}",
        channel.name, status, channel.kind, details
    );
}

/// Discover WASM channel names by scanning the channels directory for `*.wasm` files.
///
/// Matches the real loader's discovery logic (`WasmChannelLoader::load_from_dir`):
/// scans only top-level `*.wasm` files in the directory.
async fn discover_wasm_channels(dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(_) => return names,
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("wasm")
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            names.push(stem.to_string());
        }
    }

    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ChannelsConfig, CliConfig, GatewayConfig, HttpConfig};
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[tokio::test]
    async fn discover_wasm_channels_empty_on_missing_dir() {
        let result = discover_wasm_channels(Path::new("/nonexistent/path")).await;
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn discover_wasm_channels_finds_flat_wasm_files() {
        let tmp = tempfile::tempdir().unwrap();
        // Flat .wasm files — matches real loader (load_from_dir)
        std::fs::File::create(tmp.path().join("weechat.wasm")).unwrap();
        std::fs::File::create(tmp.path().join("xmpp.wasm")).unwrap();
        // Non-.wasm files should be skipped
        std::fs::File::create(tmp.path().join("readme.txt")).unwrap();
        // Directories should be skipped
        std::fs::create_dir(tmp.path().join("somedir")).unwrap();

        let result = discover_wasm_channels(tmp.path()).await;
        assert_eq!(result, vec!["weechat", "xmpp"]);
    }

    #[test]
    fn channel_info_struct() {
        let info = ChannelInfo {
            name: "test".to_string(),
            kind: "built-in",
            enabled: true,
            details: vec![("port", "3000".to_string())],
        };
        assert!(info.enabled);
        assert_eq!(info.kind, "built-in");
        assert_eq!(info.details.len(), 1);
    }

    #[test]
    fn channel_json_surfaces_failed_runtime_start() {
        let channel = ChannelInfo {
            name: "multica".to_string(),
            kind: "wasm",
            enabled: true,
            details: vec![],
        };
        let reason =
            "Channel multica failed to start: required field 'multica_url' is not configured"
                .to_string();

        let value = channel_to_json(&channel, false, Some(&reason));
        assert_eq!(value["active"], false);
        assert_eq!(value["activation_error"], reason);
        assert_eq!(value["enabled"], true);
    }

    #[test]
    fn builtin_channels_do_not_include_xmpp() {
        let config = ChannelsConfig {
            cli: CliConfig { enabled: true },
            http: Some(HttpConfig {
                host: "127.0.0.1".to_string(),
                port: 8080,
                webhook_secret: None,
                user_id: "owner".to_string(),
            }),
            gateway: Some(GatewayConfig {
                host: "127.0.0.1".to_string(),
                port: 3000,
                auth_token: None,
                user_id: "owner".to_string(),
                workspace_read_scopes: vec![],
                memory_layers: vec![],
                user_tokens: None,
                mascot_enabled: false,
                ws_ping_interval_secs: 30,
                ws_idle_timeout_secs: 120,
            }),
            signal: None,
            xmpp: None,
            wasm_channels_dir: PathBuf::from("/tmp/channels"),
            wasm_channels_enabled: true,
            wasm_channel_owner_ids: HashMap::new(),
            wasm_channel_owner_actor_ids: HashMap::new(),
            tool_activity: false,
        };

        let channels = builtin_channels(&config);
        assert!(channels.iter().all(|channel| channel.name != "xmpp"));
    }
}
