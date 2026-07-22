//! MCP server management CLI commands.
//!
//! Commands for adding, removing, authenticating, and testing MCP servers.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use clap::{Args, Subcommand};

use crate::config::Config;
use crate::db::Database;
use crate::secrets::SecretsStore;
use crate::tools::mcp::{
    McpClient, McpProcessManager, McpServerConfig, McpSessionManager, OAuthConfig,
    auth::{authorize_mcp_server, is_authenticated},
    config::{self, EffectiveTransport, McpServersFile},
    factory::create_client_from_config,
};

/// Arguments for the `mcp add` subcommand.
#[derive(Args, Debug, Clone)]
pub struct McpAddArgs {
    /// Server name (e.g., "notion", "github")
    pub name: String,

    /// Server URL (e.g., "https://mcp.notion.com") -- required for http transport
    pub url: Option<String>,

    /// Transport type: http (default), stdio, unix
    #[arg(long, default_value = "http")]
    pub transport: String,

    /// Command to run (stdio transport)
    #[arg(long)]
    pub command: Option<String>,

    /// Command arguments (stdio transport, can be repeated)
    #[arg(long = "arg", num_args = 1..)]
    pub cmd_args: Vec<String>,

    /// Environment variables (stdio transport, KEY=VALUE format, can be repeated)
    #[arg(long = "env", value_parser = parse_env_var)]
    pub env: Vec<(String, String)>,

    /// Unix socket path (unix transport)
    #[arg(long)]
    pub socket: Option<String>,

    /// Custom HTTP headers (KEY:VALUE format, can be repeated)
    #[arg(long = "header", value_parser = parse_header)]
    pub headers: Vec<(String, String)>,

    /// OAuth client ID (if authentication is required)
    #[arg(long)]
    pub client_id: Option<String>,

    /// OAuth authorization URL (optional, can be discovered)
    #[arg(long)]
    pub auth_url: Option<String>,

    /// OAuth token URL (optional, can be discovered)
    #[arg(long)]
    pub token_url: Option<String>,

    /// Scopes to request (comma-separated)
    #[arg(long)]
    pub scopes: Option<String>,

    /// Server description
    #[arg(long)]
    pub description: Option<String>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum McpCommand {
    /// Add an MCP server
    Add(Box<McpAddArgs>),

    /// Remove an MCP server
    Remove {
        /// Server name to remove
        name: String,
    },

    /// List configured MCP servers
    List {
        /// Show detailed information
        #[arg(short, long)]
        verbose: bool,
    },

    /// Authenticate with an MCP server (OAuth flow)
    Auth {
        /// Server name to authenticate
        name: String,

        /// User ID for storing the token (default: "default")
        #[arg(short, long, default_value = "default")]
        user: String,
    },

    /// Test connection to an MCP server
    Test {
        /// Server name to test
        name: String,

        /// User ID for authentication (default: "default")
        #[arg(short, long, default_value = "default")]
        user: String,
    },

    /// Enable or disable an MCP server
    Toggle {
        /// Server name
        name: String,

        /// Enable the server
        #[arg(long, conflicts_with = "disable")]
        enable: bool,

        /// Disable the server
        #[arg(long, conflicts_with = "enable")]
        disable: bool,

        /// Gateway URL for applying the change to a running LunarWing instance
        #[arg(long)]
        url: Option<String>,

        /// Gateway auth token (required with --url; otherwise reads configuration or env)
        #[arg(long)]
        token: Option<String>,

        /// Only update persisted configuration; do not contact the running gateway
        #[arg(long)]
        offline: bool,
    },
}

fn parse_header(s: &str) -> Result<(String, String), String> {
    let pos = s
        .find(':')
        .ok_or_else(|| format!("invalid header format '{}', expected KEY:VALUE", s))?;
    Ok((s[..pos].trim().to_string(), s[pos + 1..].trim().to_string()))
}

fn parse_env_var(s: &str) -> Result<(String, String), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid env var format '{}', expected KEY=VALUE", s))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}

/// Run an MCP command.
pub async fn run_mcp_command(
    cmd: McpCommand,
    config_path: Option<&Path>,
    no_db: bool,
) -> anyhow::Result<()> {
    match cmd {
        McpCommand::Add(args) => add_server(*args, config_path, no_db).await,
        McpCommand::Remove { name } => remove_server(name, config_path, no_db).await,
        McpCommand::List { verbose } => list_servers(verbose, config_path, no_db).await,
        McpCommand::Auth { name, user } => auth_server(name, user, config_path, no_db).await,
        McpCommand::Test { name, user } => test_server(name, user, config_path, no_db).await,
        McpCommand::Toggle {
            name,
            enable,
            disable,
            url,
            token,
            offline,
        } => {
            toggle_server(
                name,
                enable,
                disable,
                url,
                token,
                offline,
                config_path,
                no_db,
            )
            .await
        }
    }
}

/// Add a new MCP server.
async fn add_server(
    args: McpAddArgs,
    config_path: Option<&Path>,
    no_db: bool,
) -> anyhow::Result<()> {
    let McpAddArgs {
        name,
        url,
        transport,
        command,
        cmd_args,
        env,
        socket,
        headers,
        client_id,
        auth_url,
        token_url,
        scopes,
        description,
    } = args;

    let transport_lower = transport.to_lowercase();

    let mut config = match transport_lower.as_str() {
        "stdio" => {
            let cmd = command
                .clone()
                .ok_or_else(|| anyhow::anyhow!("--command is required for stdio transport"))?;
            let env_map: HashMap<String, String> = env.into_iter().collect();
            McpServerConfig::new_stdio(&name, &cmd, cmd_args.clone(), env_map)
        }
        "unix" => {
            let socket_path = socket
                .clone()
                .ok_or_else(|| anyhow::anyhow!("--socket is required for unix transport"))?;
            McpServerConfig::new_unix(&name, &socket_path)
        }
        "http" => {
            let url_val = url
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("URL is required for http transport"))?;
            McpServerConfig::new(&name, url_val)
        }
        other => {
            anyhow::bail!(
                "Unknown transport type '{}'. Supported: http, stdio, unix",
                other
            );
        }
    };

    // Apply headers if any
    if !headers.is_empty() {
        let headers_map: HashMap<String, String> = headers.into_iter().collect();
        config = config.with_headers(headers_map);
    }

    if let Some(desc) = description {
        config = config.with_description(desc);
    }

    // Track if auth is required
    let requires_auth = client_id.is_some();

    // Set up OAuth if client_id is provided (HTTP transport only)
    if let Some(client_id) = client_id {
        if transport_lower != "http" {
            anyhow::bail!("OAuth authentication is only supported with http transport");
        }

        let mut oauth = OAuthConfig::new(client_id);

        if let (Some(auth), Some(token)) = (auth_url, token_url) {
            oauth = oauth.with_endpoints(auth, token);
        }

        if let Some(scopes_str) = scopes {
            let scope_list: Vec<String> = scopes_str
                .split(',')
                .map(|s| s.trim().to_string())
                .collect();
            oauth = oauth.with_scopes(scope_list);
        }

        config = config.with_oauth(oauth);
    }

    persist_server_with_path(config, true, config_path, no_db).await?;

    println!();
    println!("  ✓ Added MCP server '{}'", name);

    match transport_lower.as_str() {
        "stdio" => {
            println!(
                "    Transport: stdio (command: {})",
                command.as_deref().unwrap_or("")
            );
        }
        "unix" => {
            println!(
                "    Transport: unix (socket: {})",
                socket.as_deref().unwrap_or("")
            );
        }
        _ => {
            println!("    URL: {}", url.as_deref().unwrap_or(""));
        }
    }

    if requires_auth {
        println!();
        println!("  Run 'lunarwing mcp auth {}' to authenticate.", name);
    }

    println!();

    Ok(())
}

/// Remove an MCP server.
async fn remove_server(
    name: String,
    config_path: Option<&Path>,
    no_db: bool,
) -> anyhow::Result<()> {
    let db = connect_db(config_path, no_db).await?;
    if let Some(db) = db.as_ref() {
        config::load_mcp_servers_from_db_or_migrate(db.store.as_ref(), &db.owner_id).await?;
        config::remove_mcp_server_db(db.store.as_ref(), &db.owner_id, &name).await?;
    } else {
        config::remove_mcp_server(&name).await?;
    }

    println!();
    println!("  ✓ Removed MCP server '{}'", name);
    println!();

    Ok(())
}

/// List configured MCP servers.
async fn list_servers(
    verbose: bool,
    config_path: Option<&Path>,
    no_db: bool,
) -> anyhow::Result<()> {
    let db = connect_db(config_path, no_db).await?;
    let servers = load_servers(db.as_ref()).await?;

    if servers.servers.is_empty() {
        println!();
        println!("  No MCP servers configured.");
        println!();
        println!("  Add a server with:");
        println!("    lunarwing mcp add <name> <url> [--client-id <id>]");
        println!();
        return Ok(());
    }

    println!();
    println!("  Configured MCP servers:");
    println!();

    for server in &servers.servers {
        let status = if server.enabled { "●" } else { "○" };
        let auth_status = if server.requires_auth() {
            " (auth required)"
        } else {
            ""
        };

        let effective = server.effective_transport();

        let transport_label = match &effective {
            EffectiveTransport::Http => "http".to_string(),
            EffectiveTransport::Stdio { command, .. } => {
                format!("stdio ({})", command)
            }
            EffectiveTransport::Unix { socket_path } => {
                format!("unix ({})", socket_path)
            }
        };

        if verbose {
            println!("  {} {}{}", status, server.name, auth_status);
            println!("      Transport: {}", transport_label);
            match &effective {
                EffectiveTransport::Http => {
                    println!("      URL: {}", server.url);
                }
                EffectiveTransport::Stdio { command, args, env } => {
                    println!("      Command: {}", command);
                    if !args.is_empty() {
                        println!("      Args: {}", args.join(", "));
                    }
                    if !env.is_empty() {
                        // Only print env var names, not values (may contain secrets).
                        let env_keys: Vec<&str> = env.keys().map(|k| k.as_str()).collect();
                        println!("      Env: {}", env_keys.join(", "));
                    }
                }
                EffectiveTransport::Unix { socket_path } => {
                    println!("      Socket: {}", socket_path);
                }
            }
            if let Some(ref desc) = server.description {
                println!("      Description: {}", desc);
            }
            if let Some(ref oauth) = server.oauth {
                println!("      OAuth Client ID: {}", oauth.client_id);
                if !oauth.scopes.is_empty() {
                    println!("      Scopes: {}", oauth.scopes.join(", "));
                }
            }
            if !server.headers.is_empty() {
                let header_keys: Vec<&String> = server.headers.keys().collect();
                println!(
                    "      Headers: {}",
                    header_keys
                        .iter()
                        .map(|k| k.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            println!();
        } else {
            let display = match &effective {
                EffectiveTransport::Http => server.url.clone(),
                EffectiveTransport::Stdio { command, .. } => command.to_string(),
                EffectiveTransport::Unix { socket_path } => socket_path.to_string(),
            };
            println!(
                "  {} {} - {} [{}]{}",
                status, server.name, display, transport_label, auth_status
            );
        }
    }

    if !verbose {
        println!();
        println!("  Use --verbose for more details.");
    }

    println!();

    Ok(())
}

/// Authenticate with an MCP server.
async fn auth_server(
    name: String,
    user_id: String,
    config_path: Option<&Path>,
    no_db: bool,
) -> anyhow::Result<()> {
    // Get server config
    let db = connect_db(config_path, no_db).await?;
    let servers = load_servers(db.as_ref()).await?;
    let server = servers
        .get(&name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Server '{}' not found", name))?;

    // Initialize secrets store
    let secrets = get_secrets_store().await?;

    // Check if already authenticated
    if is_authenticated(&server, &secrets, &user_id).await {
        println!();
        println!("  Server '{}' is already authenticated.", name);
        println!();
        print!("  Re-authenticate? [y/N]: ");
        std::io::stdout().flush()?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            return Ok(());
        }
        println!();
    }

    println!();
    println!("╔════════════════════════════════════════════════════════════════╗");
    println!(
        "║  {:^62}║",
        format!("{} Authentication", name.to_uppercase())
    );
    println!("╚════════════════════════════════════════════════════════════════╝");
    println!();

    // Perform OAuth flow (supports both pre-configured OAuth and DCR)
    match authorize_mcp_server(&server, &secrets, &user_id).await {
        Ok(_token) => {
            println!();
            println!("  ✓ Successfully authenticated with '{}'!", name);
            println!();
            println!("  You can now use tools from this server.");
            println!();
        }
        Err(crate::tools::mcp::auth::AuthError::NotSupported) => {
            println!();
            println!("  ✗ Server does not support OAuth authentication.");
            println!();
            println!("  The server may require a different authentication method,");
            println!("  or you may need to configure OAuth manually:");
            println!();
            println!("    lunarwing mcp remove {}", name);
            println!(
                "    lunarwing mcp add {} {} --client-id YOUR_CLIENT_ID",
                name, server.url
            );
            println!();
        }
        Err(e) => {
            println!();
            println!("  ✗ Authentication failed: {}", e);
            println!();
            return Err(e.into());
        }
    }

    Ok(())
}

/// Test connection to an MCP server.
async fn test_server(
    name: String,
    user_id: String,
    config_path: Option<&Path>,
    no_db: bool,
) -> anyhow::Result<()> {
    // Get server config
    let db = connect_db(config_path, no_db).await?;
    let servers = load_servers(db.as_ref()).await?;
    let server = servers
        .get(&name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Server '{}' not found", name))?;

    println!();
    println!("  Testing connection to '{}'...", name);

    // Create client
    let session_manager = Arc::new(McpSessionManager::new());

    // Always check for stored tokens (from either pre-configured OAuth or DCR)
    let secrets = get_secrets_store().await?;
    let has_tokens = is_authenticated(&server, &secrets, &user_id).await;

    let client = if has_tokens {
        // We have stored tokens, use authenticated client
        McpClient::new_authenticated(server.clone(), session_manager.clone(), secrets, user_id)
    } else if server.requires_auth() {
        // OAuth configured but no tokens - need to authenticate
        println!();
        println!(
            "  ✗ Not authenticated. Run 'lunarwing mcp auth {}' first.",
            name
        );
        println!();
        return Ok(());
    } else {
        // Use the factory to dispatch on transport type (HTTP, stdio, unix)
        let process_manager = Arc::new(McpProcessManager::new());
        create_client_from_config(
            server.clone(),
            &session_manager,
            &process_manager,
            None,
            "default",
        )
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?
    };

    // Test connection
    match client.test_connection().await {
        Ok(()) => {
            println!("  ✓ Connection successful!");
            println!();

            // List tools
            match client.list_tools().await {
                Ok(tools) => {
                    println!("  Available tools ({}):", tools.len());
                    for tool in tools {
                        let approval = if tool.requires_approval() {
                            " [approval required]"
                        } else {
                            ""
                        };
                        println!("    • {}{}", tool.name, approval);
                        if !tool.description.is_empty() {
                            // Truncate long descriptions
                            let desc = if tool.description.len() > 60 {
                                format!("{}...", &tool.description[..57])
                            } else {
                                tool.description.clone()
                            };
                            println!("      {}", desc);
                        }
                    }
                }
                Err(e) => {
                    println!("  ✗ Failed to list tools: {}", e);
                }
            }
        }
        Err(e) => {
            let err_str = e.to_string();
            // Check if server requires auth but we don't have valid tokens
            if err_str.contains("401") || err_str.contains("requires authentication") {
                if has_tokens {
                    // We had tokens but they failed - need to re-authenticate
                    println!(
                        "  ✗ Authentication failed (token may be expired). Try re-authenticating:"
                    );
                    println!("    lunarwing mcp auth {}", name);
                } else {
                    // No tokens - server requires auth
                    println!("  ✗ Server requires authentication.");
                    println!();
                    println!("  Run 'lunarwing mcp auth {}' to authenticate.", name);
                }
            } else {
                println!("  ✗ Connection failed: {}", e);
            }
        }
    }

    println!();

    Ok(())
}

/// Toggle server enabled/disabled state.
async fn toggle_server(
    name: String,
    enable: bool,
    disable: bool,
    gateway_url: Option<String>,
    gateway_token: Option<String>,
    offline: bool,
    config_path: Option<&Path>,
    no_db: bool,
) -> anyhow::Result<()> {
    let db = connect_db(config_path, no_db).await?;
    let servers = load_servers(db.as_ref()).await?;
    if servers.get(&name).is_none() {
        anyhow::bail!("Server '{}' not found", name);
    }
    let requested_state = if enable {
        Some(true)
    } else if disable {
        Some(false)
    } else {
        None
    };

    if !offline {
        match apply_live_toggle(
            &name,
            requested_state,
            gateway_url.as_deref(),
            gateway_token.as_deref(),
            config_path,
            db.as_ref(),
        )
        .await?
        {
            LiveToggleResult::Applied(message) => {
                println!();
                println!("  ✓ {message}");
                println!();
                return Ok(());
            }
            LiveToggleResult::Unavailable(reason) => {
                eprintln!("  Gateway unavailable ({reason}); saving for next startup.");
            }
        }
    }

    let server = if let Some(db) = db.as_ref() {
        match requested_state {
            Some(enabled) => {
                config::set_mcp_server_enabled_db(db.store.as_ref(), &db.owner_id, &name, enabled)
                    .await?
            }
            None => {
                config::toggle_mcp_server_enabled_db(db.store.as_ref(), &db.owner_id, &name).await?
            }
        }
    } else {
        match requested_state {
            Some(enabled) => config::set_mcp_server_enabled(&name, enabled).await?,
            None => config::toggle_mcp_server_enabled(&name).await?,
        }
    };
    let new_state = server.enabled;

    let status = if new_state { "enabled" } else { "disabled" };
    println!();
    println!("  ✓ Server '{name}' is now {status} in persisted configuration.");
    if !offline {
        println!("    The change will take effect when LunarWing next starts.");
    }
    println!();

    Ok(())
}

enum LiveToggleResult {
    Applied(String),
    Unavailable(String),
}

async fn apply_live_toggle(
    name: &str,
    enabled: Option<bool>,
    url_override: Option<&str>,
    token_override: Option<&str>,
    config_path: Option<&Path>,
    db: Option<&McpDbContext>,
) -> anyhow::Result<LiveToggleResult> {
    let config = match Config::from_env_with_toml(config_path).await {
        Ok(config) => Some(config),
        Err(error) if config_path.is_some() => return Err(anyhow::anyhow!("{error:#}")),
        Err(_) => None,
    };
    let gateway = config
        .as_ref()
        .and_then(|config| config.channels.gateway.as_ref());
    let base_url = url_override
        .map(str::to_string)
        .or_else(|| gateway.map(|gateway| format!("http://{}:{}", gateway.host, gateway.port)))
        .unwrap_or_else(|| {
            let host = std::env::var("GATEWAY_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
            let port = std::env::var("GATEWAY_PORT").unwrap_or_else(|_| "3000".to_string());
            format!("http://{host}:{port}")
        });
    let persisted_token = if let Some(db) = db {
        db.store
            .get_setting(&db.owner_id, "channels.gateway_auth_token")
            .await
            .ok()
            .flatten()
            .and_then(|value| value.as_str().map(str::to_string))
    } else {
        None
    };
    let token = if url_override.is_some() {
        token_override.map(str::to_string)
    } else {
        token_override
            .map(str::to_string)
            .or_else(|| gateway.and_then(|gateway| gateway.auth_token.clone()))
            .or_else(|| std::env::var("GATEWAY_AUTH_TOKEN").ok())
            .or(persisted_token)
    };
    let Some(token) = token.filter(|token| !token.trim().is_empty()) else {
        return Ok(LiveToggleResult::Unavailable(
            "no gateway auth token was provided".to_string(),
        ));
    };

    let action = match enabled {
        Some(true) => "activate",
        Some(false) => "deactivate",
        None => "toggle",
    };
    let mut endpoint = url::Url::parse(base_url.trim_end_matches('/'))
        .map_err(|error| anyhow::anyhow!("Invalid gateway URL '{base_url}': {error}"))?;
    endpoint
        .path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Gateway URL cannot be used as a base URL"))?
        .pop_if_empty()
        .extend(["api", "extensions", name, action]);
    let response = match reqwest::Client::new()
        .post(endpoint.clone())
        .bearer_auth(token.trim())
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return Ok(LiveToggleResult::Unavailable(format!(
                "could not connect to {endpoint}: {error}"
            )));
        }
    };
    if response.status() == reqwest::StatusCode::NOT_FOUND
        || response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED
    {
        return Ok(LiveToggleResult::Unavailable(format!(
            "gateway does not support live MCP {action}"
        )));
    }
    if !response.status().is_success() {
        anyhow::bail!(
            "Gateway returned HTTP {}: {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
    }
    let payload: serde_json::Value = response.json().await?;
    let message = payload
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("MCP lifecycle updated")
        .to_string();
    if payload.get("success").and_then(serde_json::Value::as_bool) != Some(true) {
        anyhow::bail!("Gateway rejected MCP lifecycle change: {message}");
    }
    Ok(LiveToggleResult::Applied(message))
}

struct McpDbContext {
    store: Arc<dyn Database>,
    owner_id: String,
}

/// Try to connect to the database (backend-agnostic).
async fn connect_db(
    config_path: Option<&Path>,
    no_db: bool,
) -> anyhow::Result<Option<McpDbContext>> {
    if no_db {
        return Ok(None);
    }
    let config = Config::from_env_with_toml(config_path)
        .await
        .map_err(|error| anyhow::anyhow!("{error:#}"))?;
    let owner_id = config.owner_id.clone();
    let store = crate::db::connect_from_config(&config.database)
        .await
        .map_err(|error| anyhow::anyhow!("{error:#}"))?;
    Ok(Some(McpDbContext { store, owner_id }))
}

/// Load MCP servers (DB if available, else disk).
async fn load_servers(db: Option<&McpDbContext>) -> Result<McpServersFile, config::ConfigError> {
    if let Some(db) = db {
        config::load_mcp_servers_from_db_or_migrate(db.store.as_ref(), &db.owner_id).await
    } else {
        config::load_mcp_servers().await
    }
}

pub(super) async fn persist_server_with_path(
    config: McpServerConfig,
    overwrite: bool,
    config_path: Option<&Path>,
    no_db: bool,
) -> anyhow::Result<()> {
    config.validate()?;

    let db = connect_db(config_path, no_db).await?;
    if let Some(db) = db.as_ref() {
        config::load_mcp_servers_from_db_or_migrate(db.store.as_ref(), &db.owner_id).await?;
        config::persist_mcp_server_db(db.store.as_ref(), &db.owner_id, config, overwrite).await?;
    } else {
        config::persist_mcp_server(config, overwrite).await?;
    }
    Ok(())
}

/// Initialize and return the secrets store.
async fn get_secrets_store() -> anyhow::Result<Arc<dyn SecretsStore + Send + Sync>> {
    crate::cli::init_secrets_store().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_command_parsing() {
        // Just verify the command structure is valid
        use clap::CommandFactory;

        // Create a dummy parent command to test subcommand parsing
        #[derive(clap::Parser)]
        struct TestCli {
            #[command(subcommand)]
            cmd: McpCommand,
        }

        TestCli::command().debug_assert();
    }

    #[test]
    fn test_mcp_toggle_live_control_flags_parse() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct TestCli {
            #[command(subcommand)]
            cmd: McpCommand,
        }

        let parsed = TestCli::try_parse_from([
            "test",
            "toggle",
            "local-files",
            "--disable",
            "--url",
            "http://127.0.0.1:3000",
            "--token",
            "test-token",
        ])
        .expect("parse live toggle flags");
        match parsed.cmd {
            McpCommand::Toggle {
                name,
                disable,
                url,
                token,
                offline,
                ..
            } => {
                assert_eq!(name, "local-files");
                assert!(disable);
                assert_eq!(url.as_deref(), Some("http://127.0.0.1:3000"));
                assert_eq!(token.as_deref(), Some("test-token"));
                assert!(!offline);
            }
            other => panic!("expected toggle command, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_apply_live_mcp_toggle_calls_authenticated_gateway() {
        use axum::extract::Path as AxumPath;
        use axum::http::HeaderMap;
        use axum::routing::post;
        use axum::{Json, Router};

        async fn deactivate(
            AxumPath(name): AxumPath<String>,
            headers: HeaderMap,
        ) -> Json<serde_json::Value> {
            assert_eq!(name, "local-files");
            assert_eq!(
                headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok()),
                Some("Bearer test-token")
            );
            Json(serde_json::json!({
                "success": true,
                "message": "deactivated live"
            }))
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test gateway");
        let address = listener.local_addr().expect("gateway address");
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/api/extensions/{name}/deactivate", post(deactivate)),
            )
            .await
        });

        let result = apply_live_toggle(
            "local-files",
            Some(false),
            Some(&format!("http://{address}")),
            Some("test-token"),
            None,
            None,
        )
        .await
        .expect("live toggle");
        match result {
            LiveToggleResult::Applied(message) => assert_eq!(message, "deactivated live"),
            LiveToggleResult::Unavailable(reason) => {
                panic!("test gateway unexpectedly unavailable: {reason}")
            }
        }
        server.abort();
    }

    #[tokio::test]
    async fn test_apply_live_implicit_toggle_uses_atomic_gateway_route() {
        use axum::{Json, Router, routing::post};

        async fn toggle() -> Json<serde_json::Value> {
            Json(serde_json::json!({
                "success": true,
                "message": "toggled live"
            }))
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test gateway");
        let address = listener.local_addr().expect("gateway address");
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/api/extensions/local-files/toggle", post(toggle)),
            )
            .await
        });

        let result = apply_live_toggle(
            "local-files",
            None,
            Some(&format!("http://{address}")),
            Some("test-token"),
            None,
            None,
        )
        .await
        .expect("live implicit toggle");
        match result {
            LiveToggleResult::Applied(message) => assert_eq!(message, "toggled live"),
            LiveToggleResult::Unavailable(reason) => {
                panic!("test gateway unexpectedly unavailable: {reason}")
            }
        }
        server.abort();
    }

    #[tokio::test]
    async fn test_custom_gateway_url_never_reuses_stored_token() {
        let result = apply_live_toggle(
            "local-files",
            Some(false),
            Some("http://127.0.0.1:9"),
            None,
            None,
            None,
        )
        .await
        .expect("missing explicit token is an offline fallback");
        match result {
            LiveToggleResult::Unavailable(reason) => {
                assert!(reason.contains("no gateway auth token"));
            }
            LiveToggleResult::Applied(message) => {
                panic!("unexpected live transition: {message}")
            }
        }
    }

    #[tokio::test]
    async fn test_old_gateway_route_returns_offline_fallback() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind old gateway fixture");
        let address = listener.local_addr().expect("gateway address");
        let server = tokio::spawn(async move { axum::serve(listener, axum::Router::new()).await });

        let result = apply_live_toggle(
            "local-files",
            Some(false),
            Some(&format!("http://{address}")),
            Some("test-token"),
            None,
            None,
        )
        .await
        .expect("404 is an offline fallback");
        match result {
            LiveToggleResult::Unavailable(reason) => {
                assert!(reason.contains("does not support"));
            }
            LiveToggleResult::Applied(message) => {
                panic!("unexpected live transition: {message}")
            }
        }
        server.abort();
    }

    #[test]
    fn test_parse_header_valid() {
        let result = parse_header("Authorization: Bearer token123").unwrap();
        assert_eq!(result.0, "Authorization");
        assert_eq!(result.1, "Bearer token123");
    }

    #[test]
    fn test_parse_header_no_spaces() {
        let result = parse_header("X-Api-Key:abc123").unwrap();
        assert_eq!(result.0, "X-Api-Key");
        assert_eq!(result.1, "abc123");
    }

    #[test]
    fn test_parse_header_invalid() {
        let result = parse_header("no-colon-here");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid header format"));
    }

    #[test]
    fn test_parse_env_var_valid() {
        let result = parse_env_var("NODE_ENV=production").unwrap();
        assert_eq!(result.0, "NODE_ENV");
        assert_eq!(result.1, "production");
    }

    #[test]
    fn test_parse_env_var_with_equals_in_value() {
        let result = parse_env_var("KEY=value=with=equals").unwrap();
        assert_eq!(result.0, "KEY");
        assert_eq!(result.1, "value=with=equals");
    }

    #[test]
    fn test_parse_env_var_invalid() {
        let result = parse_env_var("no-equals-here");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid env var format"));
    }
}
