//! MCP server management CLI commands.
//!
//! Commands for adding, removing, authenticating, and testing MCP servers.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use clap::{Args, Subcommand};

use crate::config::Config;
use crate::db::Database;
use crate::secrets::SecretsStore;
use crate::tools::mcp::{
    McpGatewayMode, McpProcessManager, McpServerConfig, McpSessionManager, OAuthConfig,
    auth::{authorize_mcp_server, is_authenticated},
    config::{self, EffectiveTransport, McpServersFile},
    factory::create_client_from_config,
    provenance::{McpPackageProvenance, PackageEcosystem, PackageTrust, ProvenanceIdentity},
    sandbox::McpSandboxConfig,
};

/// Build the sandbox policy from `mcp add` flags.
///
/// `--sandbox` enables the baseline (process-group isolation plus Linux
/// no-new-privileges and ptrace denial); any `--sandbox-*` option implies a
/// sandbox is present without enabling the whole baseline.
fn build_sandbox_from_add_args(args: &McpAddArgs) -> anyhow::Result<Option<McpSandboxConfig>> {
    let detailed = args.sandbox_max_memory_mb.is_some()
        || args.sandbox_max_cpu_seconds.is_some()
        || args.sandbox_max_fds.is_some()
        || args.sandbox_max_processes.is_some()
        || args.sandbox_max_file_size_mb.is_some()
        || args.sandbox_allow_core_dumps
        || args.sandbox_read_only_root
        || !args.sandbox_writable_paths.is_empty()
        || args.sandbox_network_isolated;
    if !args.sandbox && !detailed {
        return Ok(None);
    }
    let baseline = args.sandbox;
    let sandbox = McpSandboxConfig {
        max_memory_mb: args.sandbox_max_memory_mb,
        max_cpu_seconds: args.sandbox_max_cpu_seconds,
        max_file_descriptors: args.sandbox_max_fds,
        max_processes: args.sandbox_max_processes,
        max_file_size_mb: args.sandbox_max_file_size_mb,
        allow_core_dumps: args.sandbox_allow_core_dumps,
        isolate_process_group: baseline,
        no_new_privileges: baseline,
        deny_ptrace: baseline,
        read_only_root: args.sandbox_read_only_root,
        writable_paths: args.sandbox_writable_paths.clone(),
        network_isolated: args.sandbox_network_isolated,
    };
    sandbox
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid sandbox configuration: {error}"))?;
    Ok(Some(sandbox))
}

/// Arguments for the `mcp add` subcommand.
#[derive(Args, Debug, Clone)]
pub struct McpAddArgs {
    /// Server name (e.g., "notion", "github")
    pub name: String,

    /// Server URL (e.g., "https://mcp.notion.com") -- required for http transport
    pub url: Option<String>,

    /// Transport type: http (default), stdio, unix, wasm, gateway
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

    /// Secret environment references (KEY=SECRET_NAME, can be repeated)
    #[arg(long = "secret-env", value_parser = parse_secret_env_ref)]
    pub secret_env: Vec<(String, String)>,

    /// Unix socket path (unix transport)
    #[arg(long)]
    pub socket: Option<String>,

    /// Absolute component path (wasm transport)
    #[arg(long, value_name = "PATH")]
    pub component: Option<std::path::PathBuf>,

    /// WASM actor memory limit in MiB
    #[arg(long, value_name = "MIB")]
    pub wasm_memory_mb: Option<u64>,

    /// WASM actor fuel replenished for every MCP message
    #[arg(long, value_name = "COUNT")]
    pub wasm_fuel: Option<u64>,

    /// WASM actor wall-clock deadline per MCP message
    #[arg(long, value_name = "SECONDS")]
    pub wasm_timeout_seconds: Option<u64>,

    /// Gateway deployment mode: dedicated or shared
    #[arg(long, value_name = "MODE")]
    pub gateway_mode: Option<String>,

    /// Upstream route name (required for a shared gateway)
    #[arg(long, value_name = "ROUTE")]
    pub gateway_route: Option<String>,

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

    /// Enable the baseline host OS sandbox (stdio transport): the child runs
    /// as an isolated process-group leader with no_new_privileges and a
    /// seccomp filter denying ptrace/process_vm_* syscalls (Linux)
    #[arg(long)]
    pub sandbox: bool,

    /// Sandbox: cap the child's virtual memory in MiB (stdio transport)
    #[arg(long, value_name = "MIB")]
    pub sandbox_max_memory_mb: Option<u64>,

    /// Sandbox: cap the child's CPU time in seconds (stdio transport)
    #[arg(long, value_name = "SECONDS")]
    pub sandbox_max_cpu_seconds: Option<u64>,

    /// Sandbox: cap the child's open file descriptors (stdio transport)
    #[arg(long, value_name = "COUNT")]
    pub sandbox_max_fds: Option<u64>,

    /// Sandbox: cap the child's process/thread count, counted per daemon UID
    /// (stdio transport)
    #[arg(long, value_name = "COUNT")]
    pub sandbox_max_processes: Option<u64>,

    /// Sandbox: cap the size of files the child can write, in MiB (stdio transport)
    #[arg(long, value_name = "MIB")]
    pub sandbox_max_file_size_mb: Option<u64>,

    /// Sandbox: keep core dumps enabled (they are disabled when sandboxed)
    #[arg(long)]
    pub sandbox_allow_core_dumps: bool,

    /// Sandbox: Landlock read/execute-only filesystem root; grant exceptions
    /// with --sandbox-writable-path (stdio transport, Linux)
    #[arg(long)]
    pub sandbox_read_only_root: bool,

    /// Sandbox: absolute path that stays writable under
    /// --sandbox-read-only-root (can be repeated)
    #[arg(long = "sandbox-writable-path", value_name = "PATH")]
    pub sandbox_writable_paths: Vec<std::path::PathBuf>,

    /// Sandbox: remove all network access by moving the child into a fresh
    /// network namespace (stdio transport, Linux)
    #[arg(long)]
    pub sandbox_network_isolated: bool,
}

/// Arguments for the `mcp update` subcommand.
#[derive(Args, Debug, Clone)]
pub struct McpUpdateArgs {
    /// Server name to update
    pub name: String,

    /// New server URL (HTTP or gateway transport)
    pub url: Option<String>,

    /// Transport type. If omitted, keeps the existing transport.
    #[arg(long)]
    pub transport: Option<String>,

    /// New command to run (stdio transport)
    #[arg(long)]
    pub command: Option<String>,

    /// New command arguments (stdio transport, can be repeated)
    #[arg(long = "arg", num_args = 0..)]
    pub cmd_args: Option<Vec<String>>,

    /// New environment variables (stdio transport, KEY=VALUE format, can be repeated)
    #[arg(long = "env", value_parser = parse_env_var, num_args = 0..)]
    pub env: Option<Vec<(String, String)>>,

    /// New secret environment references (KEY=SECRET_NAME, can be repeated)
    #[arg(long = "secret-env", value_parser = parse_secret_env_ref, num_args = 0..)]
    pub secret_env: Option<Vec<(String, String)>>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum McpCommand {
    /// Add an MCP server
    Add(Box<McpAddArgs>),

    /// Remove an MCP server
    Remove {
        /// Server name to remove
        name: String,

        /// Gateway URL for stopping a running MCP server before removal
        #[arg(long)]
        url: Option<String>,

        /// Gateway auth token (required with --url; otherwise reads configuration or env)
        #[arg(long)]
        token: Option<String>,

        /// Remove persisted configuration without contacting the running gateway
        #[arg(long)]
        offline: bool,
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

    /// Update an MCP server's configuration in-place
    Update(Box<McpUpdateArgs>),

    /// Pin package provenance for a stdio MCP server.
    ///
    /// Resolves the configured command, records its SHA-256 digest and any
    /// supplied package identity, and marks the package pinned. Activation
    /// then verifies the digest and fails closed on mismatch.
    Pin {
        /// Server name to pin
        name: String,

        /// Canonical package identity (e.g. npm package name)
        #[arg(long)]
        package: Option<String>,

        /// Package ecosystem: npm, pypi, binary, or unknown
        #[arg(long, default_value = "unknown")]
        ecosystem: String,

        /// Immutable package version
        #[arg(long)]
        version: Option<String>,

        /// Source registry or repository URL
        #[arg(long)]
        source: Option<String>,

        /// Publisher name or key identifier
        #[arg(long)]
        publisher: Option<String>,
    },

    /// Remove package provenance from an MCP server (resume unverified activation)
    Unpin {
        /// Server name to unpin
        name: String,
    },

    /// Revoke an MCP server package: blocks activation and disables the server
    Revoke {
        /// Server name to revoke
        name: String,

        /// Gateway URL for disabling a running server immediately
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

fn parse_secret_env_ref(s: &str) -> Result<(String, String), String> {
    let (name, secret_name) = parse_env_var(s)
        .map_err(|_| format!("invalid secret env format '{s}', expected KEY=SECRET_NAME"))?;
    if secret_name.trim().is_empty() {
        return Err(format!(
            "invalid secret env format '{s}', SECRET_NAME cannot be empty"
        ));
    }
    Ok((name, secret_name))
}

/// Run an MCP command.
pub async fn run_mcp_command(
    cmd: McpCommand,
    config_path: Option<&Path>,
    no_db: bool,
) -> anyhow::Result<()> {
    match cmd {
        McpCommand::Add(args) => add_server(*args, config_path, no_db).await,
        McpCommand::Remove {
            name,
            url,
            token,
            offline,
        } => remove_server(name, url, token, offline, config_path, no_db).await,
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
        McpCommand::Update(args) => update_server(*args, config_path, no_db).await,
        McpCommand::Pin {
            name,
            package,
            ecosystem,
            version,
            source,
            publisher,
        } => {
            pin_server(
                name,
                package,
                ecosystem,
                version,
                source,
                publisher,
                config_path,
                no_db,
            )
            .await
        }
        McpCommand::Unpin { name } => unpin_server(name, config_path, no_db).await,
        McpCommand::Revoke {
            name,
            url,
            token,
            offline,
        } => revoke_server(name, url, token, offline, config_path, no_db).await,
    }
}

/// Add a new MCP server.
async fn add_server(
    args: McpAddArgs,
    config_path: Option<&Path>,
    no_db: bool,
) -> anyhow::Result<()> {
    let sandbox = build_sandbox_from_add_args(&args)?;
    validate_add_runtime_options(&args, sandbox.as_ref())?;
    let config = apply_add_metadata(build_add_transport(&args)?, &args, sandbox)?;
    let sandbox_summary = config
        .sandbox
        .as_ref()
        .map(crate::tools::mcp::sandbox::McpSandboxConfig::describe);
    persist_server_with_path(config, true, config_path, no_db).await?;
    print_add_summary(&args, sandbox_summary.as_deref());
    Ok(())
}

fn validate_add_runtime_options(
    args: &McpAddArgs,
    sandbox: Option<&McpSandboxConfig>,
) -> anyhow::Result<()> {
    let transport = args.transport.to_ascii_lowercase();
    if transport != "stdio" && !args.secret_env.is_empty() {
        anyhow::bail!("--secret-env is only supported with stdio transport");
    }
    if sandbox.is_some() && transport != "stdio" {
        anyhow::bail!("sandbox options are only supported with stdio transport");
    }
    if args.client_id.is_some() && !matches!(transport.as_str(), "http" | "gateway") {
        anyhow::bail!("OAuth authentication is only supported with http or gateway transport");
    }
    Ok(())
}

fn build_add_transport(args: &McpAddArgs) -> anyhow::Result<McpServerConfig> {
    match args.transport.to_ascii_lowercase().as_str() {
        "stdio" => {
            let command = args
                .command
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("--command is required for stdio transport"))?;
            Ok(McpServerConfig::new_stdio_with_secrets(
                &args.name,
                command,
                args.cmd_args.clone(),
                args.env.iter().cloned().collect(),
                args.secret_env.iter().cloned().collect(),
            ))
        }
        "unix" => args
            .socket
            .as_deref()
            .map(|socket| McpServerConfig::new_unix(&args.name, socket))
            .ok_or_else(|| anyhow::anyhow!("--socket is required for unix transport")),
        "http" => args
            .url
            .as_deref()
            .map(|url| McpServerConfig::new(&args.name, url))
            .ok_or_else(|| anyhow::anyhow!("URL is required for http transport")),
        "wasm" => build_wasm_add_transport(args),
        "gateway" => build_gateway_add_transport(args),
        other => anyhow::bail!(
            "Unknown transport type '{other}'. Supported: http, stdio, unix, wasm, gateway"
        ),
    }
}

fn build_wasm_add_transport(args: &McpAddArgs) -> anyhow::Result<McpServerConfig> {
    let path = args
        .component
        .clone()
        .ok_or_else(|| anyhow::anyhow!("--component is required for wasm transport"))?;
    Ok(
        McpServerConfig::new_wasm(&args.name, path).with_wasm_limits(
            args.wasm_memory_mb.unwrap_or(64),
            args.wasm_fuel
                .unwrap_or(crate::tools::wasm::DEFAULT_FUEL_LIMIT),
            args.wasm_timeout_seconds.unwrap_or(60),
        ),
    )
}

fn build_gateway_add_transport(args: &McpAddArgs) -> anyhow::Result<McpServerConfig> {
    let endpoint = args
        .url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("URL is required for gateway transport"))?;
    let mode = match args.gateway_mode.as_deref().unwrap_or("dedicated") {
        "dedicated" => McpGatewayMode::Dedicated,
        "shared" => McpGatewayMode::Shared,
        other => anyhow::bail!("Unknown gateway mode '{other}'. Supported: dedicated, shared"),
    };
    Ok(McpServerConfig::new_gateway(
        &args.name,
        endpoint,
        mode,
        args.gateway_route.clone(),
    ))
}

fn apply_add_metadata(
    mut config: McpServerConfig,
    args: &McpAddArgs,
    sandbox: Option<McpSandboxConfig>,
) -> anyhow::Result<McpServerConfig> {
    if !args.headers.is_empty() {
        config = config.with_headers(args.headers.iter().cloned().collect());
    }
    if let Some(description) = &args.description {
        config = config.with_description(description);
    }
    if let Some(sandbox) = sandbox {
        config = config.with_sandbox(sandbox);
    }
    if let Some(client_id) = &args.client_id {
        let mut oauth = OAuthConfig::new(client_id);
        if let (Some(auth), Some(token)) = (&args.auth_url, &args.token_url) {
            oauth = oauth.with_endpoints(auth, token);
        }
        if let Some(scopes) = &args.scopes {
            oauth = oauth.with_scopes(scopes.split(',').map(|s| s.trim().to_string()).collect());
        }
        config = config.with_oauth(oauth);
    }
    config.validate().map_err(anyhow::Error::from)?;
    Ok(config)
}

fn print_add_summary(args: &McpAddArgs, sandbox_summary: Option<&str>) {
    println!();
    println!("  ✓ Added MCP server '{}'", args.name);
    match args.transport.to_ascii_lowercase().as_str() {
        "stdio" => {
            println!(
                "    Transport: stdio (command: {})",
                args.command.as_deref().unwrap_or("")
            );
            if let Some(summary) = sandbox_summary {
                println!("    Sandbox: {summary}");
            }
        }
        "unix" => println!(
            "    Transport: unix (socket: {})",
            args.socket.as_deref().unwrap_or("")
        ),
        "wasm" => println!(
            "    Transport: wasm (component: {})",
            args.component.as_deref().unwrap_or(Path::new("")).display()
        ),
        "gateway" => println!(
            "    Transport: gateway ({}) at {}",
            args.gateway_mode.as_deref().unwrap_or("dedicated"),
            args.url.as_deref().unwrap_or("")
        ),
        _ => println!("    URL: {}", args.url.as_deref().unwrap_or("")),
    }
    if args.client_id.is_some() {
        println!(
            "\n  Run 'lunarwing mcp auth {}' to authenticate.",
            args.name
        );
    }
    println!();
}

/// Remove an MCP server.
async fn remove_server(
    name: String,
    gateway_url: Option<String>,
    gateway_token: Option<String>,
    offline: bool,
    config_path: Option<&Path>,
    no_db: bool,
) -> anyhow::Result<()> {
    let db = connect_db(config_path, no_db).await?;
    let servers = load_servers(db.as_ref()).await?;
    if servers.get(&name).is_none() {
        anyhow::bail!("Server '{name}' not found");
    }

    if !offline {
        match apply_live_remove(
            &name,
            gateway_url.as_deref(),
            gateway_token.as_deref(),
            config_path,
            db.as_ref(),
        )
        .await?
        {
            LiveMcpResult::Applied(message) => {
                println!();
                println!("  ✓ {message}");
                println!();
                return Ok(());
            }
            LiveMcpResult::Unavailable(reason) => {
                anyhow::bail!(
                    "Live MCP removal unavailable ({reason}). No offline configuration deletion was attempted; if the request reached the gateway, its outcome may be unknown. Check status, or stop LunarWing and rerun with --offline for persisted-only removal."
                );
            }
        }
    }

    eprintln!(
        "  Warning: --offline removes configuration only; it does not stop a running MCP process or unregister its tools."
    );

    if let Some(db) = db.as_ref() {
        config::remove_mcp_server_db(db.store.as_ref(), &db.owner_id, &name).await?;
    } else {
        config::remove_mcp_server(&name).await?;
    }

    println!();
    println!("  ✓ Removed MCP server '{name}' from persisted configuration.");
    println!("    Offline removal did not contact or stop a running LunarWing process.");
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
            EffectiveTransport::Wasm { .. } => "wasm".to_string(),
            EffectiveTransport::Gateway { mode, .. } => format!("gateway ({mode:?})"),
        };

        if verbose {
            println!("  {} {}{}", status, server.name, auth_status);
            println!("      Transport: {}", transport_label);
            match &effective {
                EffectiveTransport::Http => {
                    println!("      URL: {}", server.url);
                }
                EffectiveTransport::Stdio {
                    command,
                    args,
                    env,
                    env_secrets,
                } => {
                    println!("      Command: {}", command);
                    if !args.is_empty() {
                        println!("      Args: {}", args.join(", "));
                    }
                    if !env.is_empty() {
                        // Only print env var names, not values (may contain secrets).
                        let env_keys: Vec<&str> = env.keys().map(|k| k.as_str()).collect();
                        println!("      Env: {}", env_keys.join(", "));
                    }
                    if !env_secrets.is_empty() {
                        let mut mappings = env_secrets
                            .iter()
                            .map(|(name, secret)| format!("{name} <- {secret}"))
                            .collect::<Vec<_>>();
                        mappings.sort();
                        println!("      Secret env: {}", mappings.join(", "));
                    }
                }
                EffectiveTransport::Unix { socket_path } => {
                    println!("      Socket: {}", socket_path);
                }
                EffectiveTransport::Wasm {
                    component_path,
                    memory_mb,
                    fuel,
                    timeout_seconds,
                } => {
                    println!("      Component: {}", component_path.display());
                    println!("      Memory: {memory_mb} MiB");
                    println!("      Fuel/message: {fuel}");
                    println!("      Timeout: {timeout_seconds}s");
                }
                EffectiveTransport::Gateway { mode, route } => {
                    println!("      URL: {}", server.url);
                    println!("      Gateway mode: {mode:?}");
                    if let Some(route) = route {
                        println!("      Route: {route}");
                    }
                }
            }
            if let Some(ref desc) = server.description {
                println!("      Description: {}", desc);
            }
            if let Some(ref provenance) = server.provenance {
                println!("      Provenance: {}", provenance.trust);
                if let Some(ref package_id) = provenance.package_id {
                    println!("      Package: {} [{}]", package_id, provenance.ecosystem);
                }
                if let Some(ref digest) = provenance.command_sha256 {
                    println!("      Command SHA-256: {}", digest);
                }
            }
            if let Some(ref sandbox) = server.sandbox {
                println!("      Sandbox: {}", sandbox.describe());
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
                EffectiveTransport::Http | EffectiveTransport::Gateway { .. } => server.url.clone(),
                EffectiveTransport::Stdio { command, .. } => command.to_string(),
                EffectiveTransport::Unix { socket_path } => socket_path.to_string(),
                EffectiveTransport::Wasm { component_path, .. } => {
                    component_path.display().to_string()
                }
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
    let process_manager = Arc::new(McpProcessManager::new());

    // Always check for stored tokens (from either pre-configured OAuth or DCR)
    let secrets = get_secrets_store().await?;
    let has_tokens = is_authenticated(&server, &secrets, &user_id).await;

    if !has_tokens && server.requires_auth() {
        // OAuth configured but no tokens - need to authenticate
        println!();
        println!(
            "  ✗ Not authenticated. Run 'lunarwing mcp auth {}' first.",
            name
        );
        println!();
        return Ok(());
    }

    let client = create_client_from_config(
        server.clone(),
        &session_manager,
        &process_manager,
        Some(secrets),
        &user_id,
    )
    .await
    .map_err(|e| anyhow::anyhow!("{}", e))?;

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

    if let Err(error) = crate::tools::mcp::factory::shutdown_client_runtime(
        &name,
        Some(&client),
        &session_manager,
        &process_manager,
    )
    .await
    {
        println!("  Cleanup warning: {error}");
    }

    println!();

    Ok(())
}

/// Update an MCP server's configuration in-place.
async fn update_server(
    args: McpUpdateArgs,
    config_path: Option<&Path>,
    no_db: bool,
) -> anyhow::Result<()> {
    let db = connect_db(config_path, no_db).await?;
    let servers = load_servers(db.as_ref()).await?;
    let existing = servers
        .get(&args.name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Server '{}' not found", args.name))?;

    let new_config = build_update_config(&args, &existing)?;
    new_config.validate()?;

    if let Some(db) = db.as_ref() {
        crate::tools::mcp::config::persist_mcp_server_db(
            db.store.as_ref(),
            &db.owner_id,
            new_config,
            true,
        )
        .await?;
    } else {
        crate::tools::mcp::config::persist_mcp_server(new_config, true).await?;
    }

    println!();
    println!("  ✓ Updated MCP server '{}'", args.name);
    println!();
    println!("    The change will take effect when LunarWing next starts.");
    println!(
        "    Use 'lunarwing mcp toggle {} --disable && lunarwing mcp toggle {} --enable' to restart it live.",
        args.name, args.name
    );
    println!();

    Ok(())
}

fn build_update_config(
    args: &McpUpdateArgs,
    existing: &McpServerConfig,
) -> Result<McpServerConfig, anyhow::Error> {
    let transport = args
        .transport
        .as_deref()
        .map(str::to_lowercase)
        .or_else(|| match existing.effective_transport() {
            EffectiveTransport::Http => Some("http".to_string()),
            EffectiveTransport::Stdio { .. } => Some("stdio".to_string()),
            EffectiveTransport::Unix { .. } => Some("unix".to_string()),
            EffectiveTransport::Wasm { .. } => Some("wasm".to_string()),
            EffectiveTransport::Gateway { .. } => Some("gateway".to_string()),
        });

    if transport.as_deref() != Some("stdio") && args.secret_env.is_some() {
        anyhow::bail!("--secret-env is only supported with stdio transport");
    }

    let mut config = match transport.as_deref() {
        Some("stdio") => {
            let cmd = args
                .command
                .as_deref()
                .or_else(|| match existing.effective_transport() {
                    EffectiveTransport::Stdio { command, .. } => Some(command),
                    _ => None,
                })
                .ok_or_else(|| anyhow::anyhow!("--command is required for stdio transport"))?;
            let new_args = args
                .cmd_args
                .clone()
                .or_else(|| match existing.effective_transport() {
                    EffectiveTransport::Stdio { args, .. } => Some(args.to_vec()),
                    _ => None,
                })
                .unwrap_or_default();
            let new_env = args
                .env
                .clone()
                .map(|v| v.into_iter().collect())
                .or_else(|| match existing.effective_transport() {
                    EffectiveTransport::Stdio { env, .. } => Some(env.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let new_secret_env = args
                .secret_env
                .clone()
                .map(|values| values.into_iter().collect())
                .or_else(|| match existing.effective_transport() {
                    EffectiveTransport::Stdio { env_secrets, .. } => Some(env_secrets.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            McpServerConfig::new_stdio_with_secrets(
                &existing.name,
                cmd,
                new_args,
                new_env,
                new_secret_env,
            )
        }
        Some("http") => {
            let url = args
                .url
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(&existing.url);
            McpServerConfig::new(&existing.name, url)
        }
        Some("unix") => {
            let socket_path = match existing.effective_transport() {
                EffectiveTransport::Unix { socket_path } => socket_path.to_string(),
                _ => {
                    return Err(anyhow::anyhow!(
                        "Cannot update to unix transport without an existing socket path"
                    ));
                }
            };
            McpServerConfig::new_unix(&existing.name, &socket_path)
        }
        Some("wasm")
            if matches!(
                existing.effective_transport(),
                EffectiveTransport::Wasm { .. }
            ) =>
        {
            existing.clone()
        }
        Some("gateway")
            if matches!(
                existing.effective_transport(),
                EffectiveTransport::Gateway { .. }
            ) =>
        {
            let mut updated = existing.clone();
            if let Some(url) = args.url.as_deref().filter(|value| !value.is_empty()) {
                updated.url = url.to_string();
            }
            updated
        }
        Some(other) => {
            return Err(anyhow::anyhow!(
                "Unknown or incomplete transport type '{other}'. Supported: http, stdio, unix, wasm, gateway"
            ));
        }
        None => return Err(anyhow::anyhow!("Could not determine transport type")),
    };

    config.enabled = existing.enabled;
    config.oauth = existing.oauth.clone();
    config.headers = existing.headers.clone();
    config.description = existing.description.clone();
    config.always_require_approval = existing.always_require_approval.clone();
    // Retain provenance across updates. When the transport leaves stdio the
    // executable digest is meaningless, so it is dropped and the trust state
    // demoted to unverified; for stdio the pin is kept, so a command change
    // fails closed at activation until the operator re-pins.
    config.provenance = existing.provenance.clone().map(|mut provenance| {
        if !matches!(
            config.effective_transport(),
            EffectiveTransport::Stdio { .. }
        ) {
            provenance.command_sha256 = None;
            provenance.resolved_command = None;
            if provenance.trust == PackageTrust::Pinned {
                provenance.trust = PackageTrust::Unverified;
            }
        }
        provenance
    });
    // Sandbox policy follows the same retention rule as provenance: kept
    // while the server stays on stdio, dropped when the transport changes.
    config.sandbox = if matches!(
        config.effective_transport(),
        EffectiveTransport::Stdio { .. }
    ) {
        existing.sandbox.clone()
    } else {
        None
    };
    Ok(config)
}

/// Persist a mutated server configuration to whichever store is active.
async fn persist_updated_server(
    db: Option<&McpDbContext>,
    server: McpServerConfig,
) -> anyhow::Result<()> {
    if let Some(db) = db {
        config::persist_mcp_server_db(db.store.as_ref(), &db.owner_id, server, true).await?;
    } else {
        config::persist_mcp_server(server, true).await?;
    }
    Ok(())
}

/// Pin package provenance for a stdio MCP server.
#[allow(clippy::too_many_arguments)] // CLI flag plumbing mirrors the clap surface
async fn pin_server(
    name: String,
    package: Option<String>,
    ecosystem: String,
    version: Option<String>,
    source: Option<String>,
    publisher: Option<String>,
    config_path: Option<&Path>,
    no_db: bool,
) -> anyhow::Result<()> {
    let db = connect_db(config_path, no_db).await?;
    let servers = load_servers(db.as_ref()).await?;
    let mut server = servers
        .get(&name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Server '{}' not found", name))?;

    let command = match server.effective_transport() {
        EffectiveTransport::Stdio { command, .. } => command.to_string(),
        _ => anyhow::bail!(
            "Server '{name}' is not a stdio server; executable pinning only applies to stdio transports"
        ),
    };

    let ecosystem = ecosystem
        .parse::<PackageEcosystem>()
        .map_err(|error: String| anyhow::anyhow!(error))?;
    let identity = ProvenanceIdentity {
        package_id: package,
        ecosystem,
        version,
        source,
        publisher,
    };
    let provenance = McpPackageProvenance::pin_for_command(&command, identity)
        .map_err(|error| anyhow::anyhow!("Failed to pin '{name}': {error}"))?;

    print_provenance(&provenance);
    server.provenance = Some(provenance);
    persist_updated_server(db.as_ref(), server).await?;

    println!();
    println!("  ✓ Pinned MCP server '{name}'. Activation now verifies the executable digest");
    println!("    and fails closed on mismatch. Re-run `mcp pin` after package upgrades.");
    println!();
    Ok(())
}

/// Remove the provenance record from an MCP server.
async fn unpin_server(name: String, config_path: Option<&Path>, no_db: bool) -> anyhow::Result<()> {
    let db = connect_db(config_path, no_db).await?;
    let servers = load_servers(db.as_ref()).await?;
    let mut server = servers
        .get(&name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Server '{}' not found", name))?;

    if server.provenance.is_none() {
        println!();
        println!("  Server '{name}' has no provenance record; nothing to unpin.");
        println!();
        return Ok(());
    }

    server.provenance = None;
    persist_updated_server(db.as_ref(), server).await?;

    println!();
    println!("  ✓ Removed provenance from '{name}'; activation is unverified again.");
    println!();
    Ok(())
}

/// Revoke an MCP server package and disable the server.
async fn revoke_server(
    name: String,
    gateway_url: Option<String>,
    gateway_token: Option<String>,
    offline: bool,
    config_path: Option<&Path>,
    no_db: bool,
) -> anyhow::Result<()> {
    let db = connect_db(config_path, no_db).await?;
    let servers = load_servers(db.as_ref()).await?;
    let mut server = servers
        .get(&name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Server '{}' not found", name))?;

    // Persist the revocation first so it is durable even if the live
    // disable cannot reach the gateway.
    let mut provenance = server
        .provenance
        .clone()
        .unwrap_or_else(|| McpPackageProvenance::with_trust(PackageTrust::Revoked));
    provenance.trust = PackageTrust::Revoked;
    provenance.recorded_at = chrono::Utc::now();
    server.provenance = Some(provenance);
    server.enabled = false;
    persist_updated_server(db.as_ref(), server).await?;

    println!();
    println!("  ✓ Revoked the package for MCP server '{name}' and disabled it.");
    println!("    Activation now fails closed until the server is re-pinned or unpinned.");

    if !offline {
        match apply_live_toggle(
            &name,
            Some(false),
            gateway_url.as_deref(),
            gateway_token.as_deref(),
            config_path,
            db.as_ref(),
        )
        .await?
        {
            LiveMcpResult::Applied(message) => println!("  ✓ {message}"),
            LiveMcpResult::Unavailable(reason) => eprintln!(
                "  Gateway unavailable ({reason}); a running server keeps its current runtime until restart."
            ),
        }
    } else {
        println!("    Offline: a running server keeps its current runtime until restart.");
    }
    println!();
    Ok(())
}

/// Print a provenance record for CLI display.
fn print_provenance(provenance: &McpPackageProvenance) {
    println!();
    println!("  Provenance: {}", provenance.trust);
    if let Some(ref package_id) = provenance.package_id {
        let ecosystem = match provenance.ecosystem {
            PackageEcosystem::Unknown => String::new(),
            other => format!(" [{other}]"),
        };
        println!("    Package: {package_id}{ecosystem}");
    }
    if let Some(ref version) = provenance.version {
        println!("    Version: {version}");
    }
    if let Some(ref source) = provenance.source {
        println!("    Source: {source}");
    }
    if let Some(ref publisher) = provenance.publisher {
        println!("    Publisher: {publisher}");
    }
    if let Some(ref resolved) = provenance.resolved_command {
        println!("    Resolved: {resolved}");
    }
    if let Some(ref digest) = provenance.command_sha256 {
        println!("    Command SHA-256: {digest}");
    }
    println!("    Recorded: {}", provenance.recorded_at);
}

/// Toggle server enabled/disabled state.
#[allow(clippy::too_many_arguments)] // CLI flag plumbing mirrors the clap surface
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
            LiveMcpResult::Applied(message) => {
                println!();
                println!("  ✓ {message}");
                println!();
                return Ok(());
            }
            LiveMcpResult::Unavailable(reason) => {
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

enum LiveMcpResult {
    Applied(String),
    Unavailable(String),
}

#[derive(Clone, Copy)]
enum LiveMcpAction {
    Activate,
    Deactivate,
    Toggle,
    Remove,
}

impl LiveMcpAction {
    fn endpoint(self) -> &'static str {
        match self {
            Self::Activate => "activate",
            Self::Deactivate => "deactivate",
            Self::Toggle => "toggle",
            Self::Remove => "remove",
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Self::Activate => "activation",
            Self::Deactivate => "deactivation",
            Self::Toggle => "toggle",
            Self::Remove => "removal",
        }
    }
}

async fn apply_live_toggle(
    name: &str,
    enabled: Option<bool>,
    url_override: Option<&str>,
    token_override: Option<&str>,
    config_path: Option<&Path>,
    db: Option<&McpDbContext>,
) -> anyhow::Result<LiveMcpResult> {
    let action = match enabled {
        Some(true) => LiveMcpAction::Activate,
        Some(false) => LiveMcpAction::Deactivate,
        None => LiveMcpAction::Toggle,
    };
    apply_live_lifecycle(name, action, url_override, token_override, config_path, db).await
}

async fn apply_live_remove(
    name: &str,
    url_override: Option<&str>,
    token_override: Option<&str>,
    config_path: Option<&Path>,
    db: Option<&McpDbContext>,
) -> anyhow::Result<LiveMcpResult> {
    apply_live_lifecycle(
        name,
        LiveMcpAction::Remove,
        url_override,
        token_override,
        config_path,
        db,
    )
    .await
}

async fn apply_live_lifecycle(
    name: &str,
    action: LiveMcpAction,
    url_override: Option<&str>,
    token_override: Option<&str>,
    config_path: Option<&Path>,
    db: Option<&McpDbContext>,
) -> anyhow::Result<LiveMcpResult> {
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
        return Ok(LiveMcpResult::Unavailable(
            "no gateway auth token was provided".to_string(),
        ));
    };

    let action_endpoint = action.endpoint();
    let mut endpoint = url::Url::parse(base_url.trim_end_matches('/'))
        .map_err(|error| anyhow::anyhow!("Invalid gateway URL '{base_url}': {error}"))?;
    endpoint
        .path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Gateway URL cannot be used as a base URL"))?
        .pop_if_empty()
        .extend(["api", "extensions", name, action_endpoint]);
    let response = match reqwest::Client::new()
        .post(endpoint.clone())
        .bearer_auth(token.trim())
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return Ok(LiveMcpResult::Unavailable(format!(
                "could not connect to {endpoint}: {error}"
            )));
        }
    };
    if response.status() == reqwest::StatusCode::NOT_FOUND
        || response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED
    {
        return Ok(LiveMcpResult::Unavailable(format!(
            "gateway does not support live MCP {}",
            action.noun()
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
    Ok(LiveMcpResult::Applied(message))
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
    use std::collections::HashMap;

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
    fn test_mcp_wasm_and_gateway_add_flags_parse() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct TestCli {
            #[command(subcommand)]
            cmd: McpCommand,
        }

        let wasm = TestCli::try_parse_from([
            "test",
            "add",
            "portable",
            "--transport",
            "wasm",
            "--component",
            "/opt/lunarwing/mcp/portable.wasm",
            "--wasm-memory-mb",
            "96",
        ])
        .expect("WASM add flags");
        let McpCommand::Add(wasm) = wasm.cmd else {
            panic!("expected add command");
        };
        assert_eq!(wasm.transport, "wasm");
        assert_eq!(wasm.wasm_memory_mb, Some(96));

        let gateway = TestCli::try_parse_from([
            "test",
            "add",
            "shared-tools",
            "https://gateway.example.com/mcp",
            "--transport",
            "gateway",
            "--gateway-mode",
            "shared",
            "--gateway-route",
            "project-tools",
        ])
        .expect("gateway add flags");
        let McpCommand::Add(gateway) = gateway.cmd else {
            panic!("expected add command");
        };
        assert_eq!(gateway.gateway_mode.as_deref(), Some("shared"));
        assert_eq!(gateway.gateway_route.as_deref(), Some("project-tools"));
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

    #[test]
    fn test_mcp_remove_live_control_flags_parse() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct TestCli {
            #[command(subcommand)]
            cmd: McpCommand,
        }

        let parsed = TestCli::try_parse_from([
            "test",
            "remove",
            "local-files",
            "--url",
            "http://127.0.0.1:3000",
            "--token",
            "test-token",
        ])
        .expect("parse live remove flags");
        match parsed.cmd {
            McpCommand::Remove {
                name,
                url,
                token,
                offline,
            } => {
                assert_eq!(name, "local-files");
                assert_eq!(url.as_deref(), Some("http://127.0.0.1:3000"));
                assert_eq!(token.as_deref(), Some("test-token"));
                assert!(!offline);
            }
            other => panic!("expected remove command, got {other:?}"),
        }
    }

    #[test]
    fn test_mcp_secret_environment_flags_parse() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct TestCli {
            #[command(subcommand)]
            cmd: McpCommand,
        }

        let parsed = TestCli::try_parse_from([
            "test",
            "add",
            "local-files",
            "--transport",
            "stdio",
            "--command",
            "local-files-mcp",
            "--secret-env",
            "API_TOKEN=api_token",
        ])
        .expect("parse secret environment reference");
        match parsed.cmd {
            McpCommand::Add(args) => {
                assert_eq!(
                    args.secret_env,
                    vec![("API_TOKEN".to_string(), "api_token".to_string())]
                );
            }
            other => panic!("expected add command, got {other:?}"),
        }
    }

    #[test]
    fn test_build_update_preserves_secret_refs_and_approval_policy() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct TestCli {
            #[command(subcommand)]
            cmd: McpCommand,
        }

        let existing = McpServerConfig::new_stdio_with_secrets(
            "local-files",
            "old-command",
            vec![],
            HashMap::new(),
            HashMap::from([("API_TOKEN".to_string(), "api_token".to_string())]),
        )
        .with_always_require_approval(["write_file"]);
        let parsed =
            TestCli::try_parse_from(["test", "update", "local-files", "--command", "new-command"])
                .expect("parse update");
        let McpCommand::Update(args) = parsed.cmd else {
            panic!("expected update command");
        };

        let updated = build_update_config(&args, &existing).expect("build update config");
        assert_eq!(
            updated.always_require_approval,
            vec!["write_file".to_string()]
        );
        assert!(matches!(
            updated.effective_transport(),
            EffectiveTransport::Stdio {
                command,
                env_secrets,
                ..
            } if command == "new-command"
                && env_secrets.get("API_TOKEN").map(String::as_str) == Some("api_token")
        ));
    }

    #[test]
    fn test_mcp_sandbox_flags_parse_and_build() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct TestCli {
            #[command(subcommand)]
            cmd: McpCommand,
        }

        let parsed = TestCli::try_parse_from([
            "test",
            "add",
            "local-files",
            "--transport",
            "stdio",
            "--command",
            "local-files-mcp",
            "--sandbox",
            "--sandbox-max-memory-mb",
            "512",
            "--sandbox-read-only-root",
            "--sandbox-writable-path",
            "/var/lib/local-files",
            "--sandbox-network-isolated",
        ])
        .expect("parse sandbox flags");
        let McpCommand::Add(args) = parsed.cmd else {
            panic!("expected add command");
        };
        let sandbox = build_sandbox_from_add_args(&args)
            .expect("sandbox builds")
            .expect("sandbox present");
        assert!(sandbox.isolate_process_group);
        assert!(sandbox.no_new_privileges);
        assert!(sandbox.deny_ptrace);
        assert_eq!(sandbox.max_memory_mb, Some(512));
        assert!(sandbox.read_only_root);
        assert_eq!(
            sandbox.writable_paths,
            vec![std::path::PathBuf::from("/var/lib/local-files")]
        );
        assert!(sandbox.network_isolated);

        // Detail flags without --sandbox build a sandbox without the baseline.
        let parsed = TestCli::try_parse_from([
            "test",
            "add",
            "limited",
            "--transport",
            "stdio",
            "--command",
            "limited-mcp",
            "--sandbox-max-fds",
            "64",
        ])
        .expect("parse limit-only flags");
        let McpCommand::Add(args) = parsed.cmd else {
            panic!("expected add command");
        };
        let sandbox = build_sandbox_from_add_args(&args)
            .expect("sandbox builds")
            .expect("sandbox present");
        assert_eq!(sandbox.max_file_descriptors, Some(64));
        assert!(!sandbox.isolate_process_group);
        assert!(!sandbox.no_new_privileges);

        // No sandbox flags at all means no sandbox.
        let parsed = TestCli::try_parse_from([
            "test",
            "add",
            "plain",
            "--transport",
            "stdio",
            "--command",
            "plain-mcp",
        ])
        .expect("parse plain add");
        let McpCommand::Add(args) = parsed.cmd else {
            panic!("expected add command");
        };
        assert!(
            build_sandbox_from_add_args(&args)
                .expect("no sandbox")
                .is_none()
        );
    }

    #[test]
    fn test_build_update_retains_sandbox_for_stdio() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct TestCli {
            #[command(subcommand)]
            cmd: McpCommand,
        }

        let sandbox = McpSandboxConfig {
            max_memory_mb: Some(256),
            isolate_process_group: true,
            ..McpSandboxConfig::default()
        };
        let existing = McpServerConfig::new_stdio("local-files", "cat", vec![], HashMap::new())
            .with_sandbox(sandbox);

        let parsed =
            TestCli::try_parse_from(["test", "update", "local-files", "--command", "new-command"])
                .expect("parse update");
        let McpCommand::Update(args) = parsed.cmd else {
            panic!("expected update command");
        };
        let updated = build_update_config(&args, &existing).expect("build update config");
        let retained = updated.sandbox.expect("sandbox retained for stdio");
        assert_eq!(retained.max_memory_mb, Some(256));
        assert!(retained.isolate_process_group);

        // Switching transports drops the now-meaningless policy.
        let parsed = TestCli::try_parse_from([
            "test",
            "update",
            "local-files",
            "https://mcp.example.com",
            "--transport",
            "http",
        ])
        .expect("parse transport switch");
        let McpCommand::Update(args) = parsed.cmd else {
            panic!("expected update command");
        };
        let updated = build_update_config(&args, &existing).expect("build update config");
        assert!(updated.sandbox.is_none());
    }

    #[test]
    fn test_mcp_pin_unpin_revoke_flags_parse() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct TestCli {
            #[command(subcommand)]
            cmd: McpCommand,
        }

        let parsed = TestCli::try_parse_from([
            "test",
            "pin",
            "local-files",
            "--package",
            "@example/local-files-mcp",
            "--ecosystem",
            "npm",
            "--version",
            "1.4.0",
            "--publisher",
            "example",
        ])
        .expect("parse pin flags");
        match parsed.cmd {
            McpCommand::Pin {
                name,
                package,
                ecosystem,
                version,
                publisher,
                ..
            } => {
                assert_eq!(name, "local-files");
                assert_eq!(package.as_deref(), Some("@example/local-files-mcp"));
                assert_eq!(ecosystem, "npm");
                assert_eq!(version.as_deref(), Some("1.4.0"));
                assert_eq!(publisher.as_deref(), Some("example"));
            }
            other => panic!("expected pin command, got {other:?}"),
        }

        let parsed =
            TestCli::try_parse_from(["test", "unpin", "local-files"]).expect("parse unpin");
        match parsed.cmd {
            McpCommand::Unpin { name } => assert_eq!(name, "local-files"),
            other => panic!("expected unpin command, got {other:?}"),
        }

        let parsed = TestCli::try_parse_from(["test", "revoke", "local-files", "--offline"])
            .expect("parse revoke");
        match parsed.cmd {
            McpCommand::Revoke { name, offline, .. } => {
                assert_eq!(name, "local-files");
                assert!(offline);
            }
            other => panic!("expected revoke command, got {other:?}"),
        }
    }

    #[test]
    fn test_build_update_retains_provenance_for_stdio() {
        let pinned = McpPackageProvenance::pin_for_command("cat", ProvenanceIdentity::default())
            .expect("pin cat");
        let existing = McpServerConfig::new_stdio("local-files", "cat", vec![], HashMap::new())
            .with_provenance(pinned);

        let args = McpUpdateArgs {
            name: "local-files".to_string(),
            url: None,
            transport: None,
            command: None,
            cmd_args: None,
            env: None,
            secret_env: None,
        };
        let updated = build_update_config(&args, &existing).expect("build update config");
        let provenance = updated.provenance.expect("provenance retained");
        assert_eq!(provenance.trust, PackageTrust::Pinned);
        assert!(provenance.command_sha256.is_some());
    }

    #[test]
    fn test_build_update_drops_digest_when_leaving_stdio() {
        let pinned = McpPackageProvenance::pin_for_command("cat", ProvenanceIdentity::default())
            .expect("pin cat");
        let existing = McpServerConfig::new_stdio("local-files", "cat", vec![], HashMap::new())
            .with_provenance(pinned);

        let args = McpUpdateArgs {
            name: "local-files".to_string(),
            url: Some("https://mcp.example.com".to_string()),
            transport: Some("http".to_string()),
            command: None,
            cmd_args: None,
            env: None,
            secret_env: None,
        };
        let updated = build_update_config(&args, &existing).expect("build update config");
        updated
            .validate()
            .expect("demoted record validates for http");
        let provenance = updated.provenance.as_ref().expect("identity retained");
        assert_eq!(provenance.trust, PackageTrust::Unverified);
        assert!(provenance.command_sha256.is_none());
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
            LiveMcpResult::Applied(message) => assert_eq!(message, "deactivated live"),
            LiveMcpResult::Unavailable(reason) => {
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
            LiveMcpResult::Applied(message) => assert_eq!(message, "toggled live"),
            LiveMcpResult::Unavailable(reason) => {
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
            LiveMcpResult::Unavailable(reason) => {
                assert!(reason.contains("no gateway auth token"));
            }
            LiveMcpResult::Applied(message) => {
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
            LiveMcpResult::Unavailable(reason) => {
                assert!(reason.contains("does not support"));
            }
            LiveMcpResult::Applied(message) => {
                panic!("unexpected live transition: {message}")
            }
        }
        server.abort();
    }

    #[tokio::test]
    async fn test_apply_live_mcp_remove_calls_authenticated_gateway() {
        use axum::extract::Path as AxumPath;
        use axum::http::HeaderMap;
        use axum::routing::post;
        use axum::{Json, Router};

        async fn remove(
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
                "message": "removed live"
            }))
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test gateway");
        let address = listener.local_addr().expect("gateway address");
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/api/extensions/{name}/remove", post(remove)),
            )
            .await
        });

        let result = apply_live_remove(
            "local-files",
            Some(&format!("http://{address}")),
            Some("test-token"),
            None,
            None,
        )
        .await
        .expect("live remove");
        match result {
            LiveMcpResult::Applied(message) => assert_eq!(message, "removed live"),
            LiveMcpResult::Unavailable(reason) => {
                panic!("test gateway unexpectedly unavailable: {reason}")
            }
        }
        server.abort();
    }

    #[tokio::test]
    async fn test_custom_remove_url_requires_explicit_token() {
        let result = apply_live_remove("local-files", Some("http://127.0.0.1:9"), None, None, None)
            .await
            .expect("missing token is reported as unavailable");
        match result {
            LiveMcpResult::Unavailable(reason) => {
                assert!(reason.contains("no gateway auth token"));
            }
            LiveMcpResult::Applied(message) => {
                panic!("unexpected live removal: {message}")
            }
        }
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

    #[test]
    fn test_parse_secret_env_ref() {
        assert_eq!(
            parse_secret_env_ref("API_TOKEN=api_token").unwrap(),
            ("API_TOKEN".to_string(), "api_token".to_string())
        );
        assert!(parse_secret_env_ref("API_TOKEN=").is_err());
        assert!(parse_secret_env_ref("api_token").is_err());
    }
}
