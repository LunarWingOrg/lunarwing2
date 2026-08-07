//! MCP server configuration.
//!
//! Stores configuration for connecting to hosted MCP servers.
//! Configuration is persisted at ~/.lunarwing/mcp-servers.json.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use fs4::FileExt;
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::bootstrap::lunarwing_base_dir;
use crate::tools::tool::ToolError;

/// Operator-provided path to a local lw2studymcp checkout.
pub const STUDY_NOTES_HOME_ENV: &str = "LW2STUDYMCP_HOME";
/// Optional state directory containing the mutable Study Notes database and notes.
pub const STUDY_NOTES_STATE_HOME_ENV: &str = "LW2STUDYMCP_STATE_HOME";
/// Explicit opt-in for Study Notes write tools.
pub const STUDY_NOTES_ALLOW_WRITE_ENV: &str = "LW2STUDYMCP_ALLOW_WRITE";
/// Stable MCP server name used by core agents and OpenCode workers.
pub const STUDY_NOTES_SERVER_NAME: &str = "study_notes";
/// Upstream Study Notes tools that mutate shared state.
pub const STUDY_NOTES_WRITE_TOOLS: [&str; 2] = ["knowledge_submit_note", "knowledge_update_status"];

const STUDY_NOTES_MANAGED_DESCRIPTION_PREFIX: &str = "Managed Study Notes MCP";
const LEGACY_STUDY_NOTES_DESCRIPTION: &str =
    "Shared read-only LunarWing codebase study notes (managed by LW2STUDYMCP_HOME)";

/// Transport configuration for an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum McpTransportConfig {
    /// HTTP/HTTPS transport (uses the `url` field on McpServerConfig).
    Http,
    /// Stdio transport — spawns a child process.
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
        /// Environment variable names mapped to owner-scoped secret names.
        /// Secret values are resolved only when the process starts.
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        env_secrets: HashMap<String, String>,
    },
    /// Unix domain socket transport.
    Unix { socket_path: String },
}

/// Configuration for connecting to a remote MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Unique name for this server (e.g., "notion", "github").
    pub name: String,

    /// Server URL (must be HTTPS for remote servers).
    pub url: String,

    /// Transport configuration. If `None`, defaults to Http using `url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<McpTransportConfig>,

    /// Custom headers to include in every HTTP request.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,

    /// OAuth configuration (if server requires authentication).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OAuthConfig>,

    /// Whether this server is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Automatic recovery stopped after repeated unstable restarts.
    /// Explicit activation clears this marker.
    #[serde(default, skip_serializing_if = "is_false")]
    pub recovery_circuit_open: bool,

    /// Optional description for the server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Raw MCP tool names that always require explicit approval.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub always_require_approval: Vec<String>,

    /// Package provenance record (identity, trust state, executable digest).
    /// Absent means unverified legacy behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<crate::tools::mcp::provenance::McpPackageProvenance>,
}

fn default_true() -> bool {
    true
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl McpServerConfig {
    /// Whether daemon startup may activate this server automatically.
    pub fn should_auto_start(&self) -> bool {
        self.enabled && !self.recovery_circuit_open
    }

    /// Create a new MCP server configuration.
    pub fn new(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
            transport: None,
            headers: HashMap::new(),
            oauth: None,
            enabled: true,
            recovery_circuit_open: false,
            description: None,
            always_require_approval: Vec::new(),
            provenance: None,
        }
    }

    /// Create a new stdio transport MCP server configuration.
    pub fn new_stdio(
        name: impl Into<String>,
        command: impl Into<String>,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> Self {
        Self::new_stdio_with_secrets(name, command, args, env, HashMap::new())
    }

    /// Create a stdio MCP configuration with owner-scoped secret references.
    pub fn new_stdio_with_secrets(
        name: impl Into<String>,
        command: impl Into<String>,
        args: Vec<String>,
        env: HashMap<String, String>,
        env_secrets: HashMap<String, String>,
    ) -> Self {
        Self {
            name: name.into(),
            url: String::new(),
            transport: Some(McpTransportConfig::Stdio {
                command: command.into(),
                args,
                env,
                env_secrets,
            }),
            headers: HashMap::new(),
            oauth: None,
            enabled: true,
            recovery_circuit_open: false,
            description: None,
            always_require_approval: Vec::new(),
            provenance: None,
        }
    }

    /// Create a new Unix socket transport MCP server configuration.
    pub fn new_unix(name: impl Into<String>, socket_path: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            url: String::new(),
            transport: Some(McpTransportConfig::Unix {
                socket_path: socket_path.into(),
            }),
            headers: HashMap::new(),
            oauth: None,
            enabled: true,
            recovery_circuit_open: false,
            description: None,
            always_require_approval: Vec::new(),
            provenance: None,
        }
    }

    /// Set OAuth configuration.
    pub fn with_oauth(mut self, oauth: OAuthConfig) -> Self {
        self.oauth = Some(oauth);
        self
    }

    /// Set description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set custom headers.
    pub fn with_headers(mut self, headers: HashMap<String, String>) -> Self {
        self.headers = headers;
        self
    }

    /// Set raw MCP tool names that must receive per-invocation approval.
    pub fn with_always_require_approval(
        mut self,
        tools: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.always_require_approval = tools.into_iter().map(Into::into).collect();
        self
    }

    /// Set the package provenance record.
    pub fn with_provenance(
        mut self,
        provenance: crate::tools::mcp::provenance::McpPackageProvenance,
    ) -> Self {
        self.provenance = Some(provenance);
        self
    }

    /// Get the effective transport type.
    pub fn effective_transport(&self) -> EffectiveTransport<'_> {
        match &self.transport {
            Some(McpTransportConfig::Http) | None => EffectiveTransport::Http,
            Some(McpTransportConfig::Stdio {
                command,
                args,
                env,
                env_secrets,
            }) => EffectiveTransport::Stdio {
                command,
                args,
                env,
                env_secrets,
            },
            Some(McpTransportConfig::Unix { socket_path }) => {
                EffectiveTransport::Unix { socket_path }
            }
        }
    }

    /// Validate the server configuration.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.name.is_empty() {
            return Err(ConfigError::InvalidConfig {
                reason: "Server name cannot be empty".to_string(),
            });
        }

        match self.effective_transport() {
            EffectiveTransport::Http => {
                if self.url.is_empty() {
                    return Err(ConfigError::InvalidConfig {
                        reason: "Server URL cannot be empty".to_string(),
                    });
                }

                // Remote servers must use HTTPS (localhost is allowed for development)
                let is_localhost = is_localhost_url(&self.url);
                if !is_localhost && !self.url.to_lowercase().starts_with("https://") {
                    return Err(ConfigError::InvalidConfig {
                        reason: "Remote MCP servers must use HTTPS".to_string(),
                    });
                }
            }
            EffectiveTransport::Stdio {
                command,
                args,
                env,
                env_secrets,
            } => {
                let mut plain_env_names = std::collections::HashSet::new();
                let mut secret_env_names = std::collections::HashSet::new();
                if command.is_empty() {
                    return Err(ConfigError::InvalidConfig {
                        reason: "Stdio transport command cannot be empty".to_string(),
                    });
                }
                if command.contains('\0') || args.iter().any(|arg| arg.contains('\0')) {
                    return Err(ConfigError::InvalidConfig {
                        reason: "Stdio command and arguments cannot contain NUL bytes".to_string(),
                    });
                }
                for (name, value) in env {
                    if name.is_empty() || name.contains(['=', '\0']) {
                        return Err(ConfigError::InvalidConfig {
                            reason: format!("Invalid stdio environment variable name '{name}'"),
                        });
                    }
                    if value.contains('\0') {
                        return Err(ConfigError::InvalidConfig {
                            reason: format!(
                                "Stdio environment variable '{name}' contains a NUL byte"
                            ),
                        });
                    }
                    if !plain_env_names.insert(canonical_env_name(name)) {
                        return Err(ConfigError::InvalidConfig {
                            reason: format!("Duplicate stdio environment variable name '{name}'"),
                        });
                    }
                }
                for (name, secret_name) in env_secrets {
                    if name.is_empty() || name.contains(['=', '\0']) {
                        return Err(ConfigError::InvalidConfig {
                            reason: format!("Invalid stdio environment variable name '{name}'"),
                        });
                    }
                    if secret_name.trim().is_empty() || secret_name.contains('\0') {
                        return Err(ConfigError::InvalidConfig {
                            reason: format!(
                                "Invalid secret name for stdio environment variable '{name}'"
                            ),
                        });
                    }
                    let canonical_name = canonical_env_name(name);
                    if plain_env_names.contains(&canonical_name) {
                        return Err(ConfigError::InvalidConfig {
                            reason: format!(
                                "Stdio environment variable '{name}' cannot use both a plain value and a secret reference"
                            ),
                        });
                    }
                    if !secret_env_names.insert(canonical_name) {
                        return Err(ConfigError::InvalidConfig {
                            reason: format!(
                                "Duplicate stdio secret environment variable name '{name}'"
                            ),
                        });
                    }
                }
            }
            EffectiveTransport::Unix { socket_path } => {
                if socket_path.is_empty() {
                    return Err(ConfigError::InvalidConfig {
                        reason: "Unix socket path cannot be empty".to_string(),
                    });
                }
            }
        }

        // Validate custom header names and values using the http crate's RFC 9110
        // token validation (catches CRLF, spaces, colons, null bytes, etc.)
        let mut normalized_headers = std::collections::HashSet::new();
        for (name, value) in &self.headers {
            if name.is_empty() {
                return Err(ConfigError::InvalidConfig {
                    reason: "Header name cannot be empty".to_string(),
                });
            }
            if reqwest::header::HeaderName::from_bytes(name.as_bytes()).is_err() {
                return Err(ConfigError::InvalidConfig {
                    reason: format!(
                        "Header name '{}' is not a valid HTTP header name (RFC 9110)",
                        name
                    ),
                });
            }
            if reqwest::header::HeaderValue::from_str(value).is_err() {
                return Err(ConfigError::InvalidConfig {
                    reason: format!("Header value for '{}' contains invalid characters", name),
                });
            }
            let normalized = name.to_ascii_lowercase();
            if !normalized_headers.insert(normalized.clone()) {
                return Err(ConfigError::InvalidConfig {
                    reason: format!("Duplicate header name '{name}' (case-insensitive)"),
                });
            }
            if matches!(
                normalized.as_str(),
                "mcp-session-id" | "content-type" | "accept"
            ) {
                return Err(ConfigError::InvalidConfig {
                    reason: format!("Header '{name}' is managed by the MCP client"),
                });
            }
        }

        if self
            .always_require_approval
            .iter()
            .any(|name| name.is_empty() || name.contains('\0'))
        {
            return Err(ConfigError::InvalidConfig {
                reason: "Approval-required MCP tool names cannot be empty or contain NUL bytes"
                    .to_string(),
            });
        }

        if let Some(provenance) = &self.provenance {
            provenance.validate()?;
            if provenance.command_sha256.is_some()
                && !matches!(self.effective_transport(), EffectiveTransport::Stdio { .. })
            {
                return Err(ConfigError::InvalidConfig {
                    reason: "provenance command digests only apply to stdio servers".to_string(),
                });
            }
        }

        Ok(())
    }

    /// Check if any custom header sets an Authorization value.
    ///
    /// Used to skip OAuth token injection when the user has explicitly
    /// configured an Authorization header (e.g. for API-key-based servers).
    pub fn has_custom_auth_header(&self) -> bool {
        self.headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("authorization"))
    }

    /// Check if this server requires authentication.
    ///
    /// Returns true if OAuth is pre-configured OR if this is a remote HTTPS server
    /// (which likely supports Dynamic Client Registration even without pre-configured OAuth).
    ///
    /// Non-HTTP transports (stdio, unix) never require auth.
    pub fn requires_auth(&self) -> bool {
        // Non-HTTP transports don't use HTTP auth
        if !matches!(self.effective_transport(), EffectiveTransport::Http) {
            return false;
        }

        if self.oauth.is_some() {
            return true;
        }
        // Remote HTTPS servers need auth handling (DCR, token refresh, 401 detection).
        // Localhost/127.0.0.1 servers are assumed to be dev servers without auth.
        let url_lower = self.url.to_lowercase();
        let is_localhost = is_localhost_url(&url_lower);
        url_lower.starts_with("https://") && !is_localhost
    }

    /// Get the secret name used to store the access token.
    pub fn token_secret_name(&self) -> String {
        format!("mcp_{}_access_token", self.name)
    }

    /// Get the secret name used to store the refresh token.
    pub fn refresh_token_secret_name(&self) -> String {
        format!("mcp_{}_refresh_token", self.name)
    }

    /// Get the secret name used to store the DCR client ID.
    pub fn client_id_secret_name(&self) -> String {
        format!("mcp_{}_client_id", self.name)
    }
}

#[cfg(windows)]
fn canonical_env_name(name: &str) -> String {
    name.to_ascii_uppercase()
}

#[cfg(not(windows))]
fn canonical_env_name(name: &str) -> String {
    name.to_string()
}

/// Parse the explicit Study Notes write opt-in.
pub fn parse_study_notes_allow_write(value: Option<&OsStr>) -> Result<bool, ConfigError> {
    let Some(value) = value else {
        return Ok(false);
    };
    let value = value.to_str().ok_or_else(|| ConfigError::InvalidConfig {
        reason: format!("{STUDY_NOTES_ALLOW_WRITE_ENV} is not valid UTF-8"),
    })?;
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "false" | "no" | "off" => Ok(false),
        "1" | "true" | "yes" | "on" => Ok(true),
        _ => Err(ConfigError::InvalidConfig {
            reason: format!(
                "{STUDY_NOTES_ALLOW_WRITE_ENV} must be one of 1/true/yes/on or 0/false/no/off"
            ),
        }),
    }
}

fn canonical_study_notes_entry(path: &Path) -> Result<PathBuf, ConfigError> {
    let canonical = path
        .canonicalize()
        .map_err(|error| ConfigError::InvalidConfig {
            reason: format!("Study Notes path '{}': {error}", path.display()),
        })?;
    if canonical != path {
        return Err(ConfigError::InvalidConfig {
            reason: format!("Study Notes path must not be a symlink: {}", path.display()),
        });
    }
    Ok(canonical)
}

/// Build the managed Study Notes MCP configuration.
///
/// Code remains operator-managed and read-only. Write mode requires a separate
/// state directory and forces per-invocation approval for every mutation tool.
pub fn study_notes_server_from_home(
    home: &Path,
    state_home: Option<&Path>,
    allow_write: bool,
) -> Result<McpServerConfig, ConfigError> {
    let home = home
        .canonicalize()
        .map_err(|error| ConfigError::InvalidConfig {
            reason: format!("{STUDY_NOTES_HOME_ENV} path '{}': {error}", home.display()),
        })?;
    if !home.is_dir() {
        return Err(ConfigError::InvalidConfig {
            reason: format!(
                "{STUDY_NOTES_HOME_ENV} path '{}' is not a directory",
                home.display()
            ),
        });
    }

    let server = canonical_study_notes_entry(&home.join("study_notes_mcp.py"))?;
    let memorydb = canonical_study_notes_entry(&home.join("memorydb.py"))?;
    let source_database = canonical_study_notes_entry(&home.join("agent-notes.sqlite3"))?;
    let source_notes = canonical_study_notes_entry(&home.join("agent-notes-by-agent"))?;
    for required in [&server, &memorydb, &source_database] {
        if !required.is_file() {
            return Err(ConfigError::InvalidConfig {
                reason: format!("Study Notes file missing: {}", required.display()),
            });
        }
    }
    if !source_notes.is_dir() {
        return Err(ConfigError::InvalidConfig {
            reason: format!("Study Notes directory missing: {}", source_notes.display()),
        });
    }

    if allow_write && state_home.is_none() {
        return Err(ConfigError::InvalidConfig {
            reason: format!(
                "{STUDY_NOTES_STATE_HOME_ENV} is required when {STUDY_NOTES_ALLOW_WRITE_ENV} is enabled"
            ),
        });
    }
    let data_home = match state_home {
        Some(state_home) => {
            state_home
                .canonicalize()
                .map_err(|error| ConfigError::InvalidConfig {
                    reason: format!(
                        "{STUDY_NOTES_STATE_HOME_ENV} path '{}': {error}",
                        state_home.display()
                    ),
                })?
        }
        None => home.clone(),
    };
    if state_home.is_some() && (data_home.starts_with(&home) || home.starts_with(&data_home)) {
        return Err(ConfigError::InvalidConfig {
            reason: "Study Notes code and state directories must not overlap".to_string(),
        });
    }
    let database = canonical_study_notes_entry(&data_home.join("agent-notes.sqlite3"))?;
    let notes = canonical_study_notes_entry(&data_home.join("agent-notes-by-agent"))?;
    if !database.starts_with(&data_home) || !notes.starts_with(&data_home) {
        return Err(ConfigError::InvalidConfig {
            reason: "Study Notes database and notes must remain inside their data directory"
                .to_string(),
        });
    }
    if !database.is_file() {
        return Err(ConfigError::InvalidConfig {
            reason: format!("Study Notes file missing: {}", database.display()),
        });
    }
    if !notes.is_dir() {
        return Err(ConfigError::InvalidConfig {
            reason: format!("Study Notes directory missing: {}", notes.display()),
        });
    }

    let path_arg = |path: &Path| {
        path.to_str()
            .map(str::to_string)
            .ok_or_else(|| ConfigError::InvalidConfig {
                reason: format!("Study Notes path is not valid UTF-8: {}", path.display()),
            })
    };
    let mut args = vec![
        path_arg(&server)?,
        "--db".to_string(),
        path_arg(&database)?,
        "--notes".to_string(),
        path_arg(&notes)?,
    ];
    if allow_write {
        args.push("--allow-write".to_string());
    }
    let mode = if allow_write {
        "read-write"
    } else {
        "read-only"
    };
    let mut config = McpServerConfig::new_stdio(
        STUDY_NOTES_SERVER_NAME,
        "python3",
        args,
        HashMap::new(),
    )
    .with_description(format!(
        "{STUDY_NOTES_MANAGED_DESCRIPTION_PREFIX} ({mode}; managed by {STUDY_NOTES_HOME_ENV})"
    ));
    if allow_write {
        config = config.with_always_require_approval(STUDY_NOTES_WRITE_TOOLS);
    }
    Ok(config)
}

fn is_managed_study_notes_server(config: &McpServerConfig) -> bool {
    config.name == STUDY_NOTES_SERVER_NAME
        && config.description.as_deref().is_some_and(|description| {
            description.starts_with(STUDY_NOTES_MANAGED_DESCRIPTION_PREFIX)
                || description == LEGACY_STUDY_NOTES_DESCRIPTION
        })
}

fn managed_study_notes_is_writable(config: &McpServerConfig) -> bool {
    is_managed_study_notes_server(config)
        && (!config.always_require_approval.is_empty()
            || matches!(
                config.effective_transport(),
                EffectiveTransport::Stdio { args, .. }
                    if args.iter().any(|arg| arg == "--allow-write")
            ))
}

/// Enforce live environment opt-in whenever a persisted managed writer is activated.
pub fn validate_managed_study_notes_runtime(config: &McpServerConfig) -> Result<(), ConfigError> {
    let allow_write =
        parse_study_notes_allow_write(std::env::var_os(STUDY_NOTES_ALLOW_WRITE_ENV).as_deref())?;
    let home = std::env::var_os(STUDY_NOTES_HOME_ENV);
    let state_home = std::env::var_os(STUDY_NOTES_STATE_HOME_ENV);
    validate_managed_study_notes_runtime_settings(
        config,
        allow_write,
        home.as_deref().map(Path::new),
        state_home.as_deref().map(Path::new),
    )
}

fn validate_managed_study_notes_runtime_settings(
    config: &McpServerConfig,
    allow_write: bool,
    home: Option<&Path>,
    state_home: Option<&Path>,
) -> Result<(), ConfigError> {
    if !managed_study_notes_is_writable(config) {
        return Ok(());
    }
    if !allow_write {
        return Err(ConfigError::InvalidConfig {
            reason: format!(
                "Managed Study Notes write mode requires live {STUDY_NOTES_ALLOW_WRITE_ENV}=1"
            ),
        });
    }
    let home = home.ok_or_else(|| ConfigError::InvalidConfig {
        reason: format!("Managed Study Notes write mode requires {STUDY_NOTES_HOME_ENV}"),
    })?;
    let state_home = state_home.ok_or_else(|| ConfigError::InvalidConfig {
        reason: format!("Managed Study Notes write mode requires {STUDY_NOTES_STATE_HOME_ENV}"),
    })?;
    let expected = study_notes_server_from_home(home, Some(state_home), true)?;
    if serde_json::to_value(&config.transport)? != serde_json::to_value(&expected.transport)?
        || config.always_require_approval != expected.always_require_approval
    {
        return Err(ConfigError::InvalidConfig {
            reason: "Persisted Study Notes writer does not match live code/state configuration"
                .to_string(),
        });
    }
    Ok(())
}

/// Remove a managed Study Notes server from this startup snapshot after config failure.
pub fn suppress_managed_study_notes_server(servers: &mut McpServersFile) -> bool {
    let managed = servers
        .get(STUDY_NOTES_SERVER_NAME)
        .is_some_and(is_managed_study_notes_server);
    managed && servers.remove(STUDY_NOTES_SERVER_NAME)
}

/// Suppress persisted managed write mode unless live environment explicitly reconciles it.
pub fn suppress_unapproved_managed_study_notes_write(servers: &mut McpServersFile) -> bool {
    let writable = servers
        .get(STUDY_NOTES_SERVER_NAME)
        .is_some_and(managed_study_notes_is_writable);
    writable && servers.remove(STUDY_NOTES_SERVER_NAME)
}

fn apply_managed_study_notes_config(
    servers: &mut McpServersFile,
    mut config: McpServerConfig,
) -> Option<McpServerConfig> {
    if let Some(existing) = servers.get(STUDY_NOTES_SERVER_NAME) {
        if !is_managed_study_notes_server(existing) {
            return None;
        }
        config.enabled = existing.enabled;
        config.recovery_circuit_open = existing.recovery_circuit_open;
    }
    servers.upsert(config.clone());
    Some(config)
}

/// Create or refresh the managed Study Notes config without replacing a user-owned server.
pub async fn reconcile_managed_study_notes_server(
    store: Option<&dyn crate::db::Database>,
    owner_id: &str,
    servers: &mut McpServersFile,
    config: McpServerConfig,
) -> Result<bool, ConfigError> {
    let reconciled = if let Some(store) = store {
        mutate_mcp_servers_db(store, owner_id, |latest| {
            Ok(apply_managed_study_notes_config(latest, config.clone()))
        })
        .await?
    } else {
        let _config_guard = acquire_mcp_config_write_guard().await?;
        let mut latest = load_mcp_servers().await?;
        let reconciled = apply_managed_study_notes_config(&mut latest, config);
        if reconciled.is_some() {
            save_mcp_servers(&latest).await?;
        }
        reconciled
    };
    let Some(config) = reconciled else {
        return Ok(false);
    };
    servers.upsert(config);
    Ok(true)
}

/// OAuth 2.1 configuration for an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthConfig {
    /// OAuth client ID.
    pub client_id: String,

    /// Authorization endpoint URL.
    /// If not provided, will be discovered from /.well-known/oauth-protected-resource.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_url: Option<String>,

    /// Token endpoint URL.
    /// If not provided, will be discovered from /.well-known/oauth-authorization-server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_url: Option<String>,

    /// Scopes to request.
    #[serde(default)]
    pub scopes: Vec<String>,

    /// Whether to use PKCE (default: true, as required by OAuth 2.1).
    #[serde(default = "default_true")]
    pub use_pkce: bool,

    /// Extra parameters to include in the authorization request.
    #[serde(default)]
    pub extra_params: HashMap<String, String>,
}

impl OAuthConfig {
    /// Create a new OAuth configuration with just a client ID.
    pub fn new(client_id: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            authorization_url: None,
            token_url: None,
            scopes: Vec::new(),
            use_pkce: true,
            extra_params: HashMap::new(),
        }
    }

    /// Set authorization and token URLs.
    pub fn with_endpoints(
        mut self,
        authorization_url: impl Into<String>,
        token_url: impl Into<String>,
    ) -> Self {
        self.authorization_url = Some(authorization_url.into());
        self.token_url = Some(token_url.into());
        self
    }

    /// Set scopes.
    pub fn with_scopes(mut self, scopes: Vec<String>) -> Self {
        self.scopes = scopes;
        self
    }
}

/// Configuration file containing all MCP servers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpServersFile {
    /// List of configured MCP servers.
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,

    /// Schema version for future compatibility.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
}

fn default_schema_version() -> u32 {
    1
}

impl McpServersFile {
    fn validate(&self) -> Result<(), ConfigError> {
        let mut names = std::collections::HashSet::new();
        for server in &self.servers {
            server
                .validate()
                .map_err(|error| ConfigError::InvalidConfig {
                    reason: format!("Server '{}': {error}", server.name),
                })?;
            if !names.insert(&server.name) {
                return Err(ConfigError::InvalidConfig {
                    reason: format!("Duplicate server name: '{}'", server.name),
                });
            }
        }
        Ok(())
    }

    /// Get a server by name.
    pub fn get(&self, name: &str) -> Option<&McpServerConfig> {
        self.servers.iter().find(|s| s.name == name)
    }

    /// Get a mutable server by name.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut McpServerConfig> {
        self.servers.iter_mut().find(|s| s.name == name)
    }

    /// Add or update a server configuration.
    pub fn upsert(&mut self, config: McpServerConfig) {
        if let Some(existing) = self.get_mut(&config.name) {
            *existing = config;
        } else {
            self.servers.push(config);
        }
    }

    /// Remove a server by name.
    pub fn remove(&mut self, name: &str) -> bool {
        let len_before = self.servers.len();
        self.servers.retain(|s| s.name != name);
        self.servers.len() < len_before
    }

    /// Get all enabled servers.
    pub fn enabled_servers(&self) -> impl Iterator<Item = &McpServerConfig> {
        self.servers.iter().filter(|s| s.enabled)
    }
}

/// Error type for MCP configuration operations.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Invalid configuration: {reason}")]
    InvalidConfig { reason: String },

    #[error("Server not found: {name}")]
    ServerNotFound { name: String },

    #[error("Server already exists: {name}")]
    ServerAlreadyExists { name: String },

    #[error("MCP configuration changed repeatedly; retry the operation")]
    ConcurrentUpdate,
}

impl From<ConfigError> for ToolError {
    fn from(err: ConfigError) -> Self {
        ToolError::ExternalService(err.to_string())
    }
}

/// Get the default MCP servers configuration path.
pub fn default_config_path() -> PathBuf {
    lunarwing_base_dir().join("mcp-servers.json")
}

/// Cross-process guard for disk-backed MCP configuration updates.
pub(crate) struct McpConfigWriteGuard(std::fs::File);

impl Drop for McpConfigWriteGuard {
    fn drop(&mut self) {
        let _ = fs4::FileExt::unlock(&self.0);
    }
}

pub(crate) async fn acquire_mcp_config_write_guard() -> Result<McpConfigWriteGuard, ConfigError> {
    let lock_path = default_config_path().with_extension("lock");
    tokio::task::spawn_blocking(move || {
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            // Lock file: contents are irrelevant, only the flock matters.
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        file.lock_exclusive()?;
        Ok(McpConfigWriteGuard(file))
    })
    .await
    .map_err(|error| ConfigError::Io(std::io::Error::other(error)))?
}

/// Load MCP server configurations from the default location.
pub async fn load_mcp_servers() -> Result<McpServersFile, ConfigError> {
    load_mcp_servers_from(default_config_path()).await
}

/// Load MCP server configurations from a specific path.
pub async fn load_mcp_servers_from(path: impl AsRef<Path>) -> Result<McpServersFile, ConfigError> {
    let path = path.as_ref();

    if !path.exists() {
        return Ok(McpServersFile::default());
    }

    let content = fs::read_to_string(path).await?;
    let config: McpServersFile = serde_json::from_str(&content)?;

    // Validate every server and reject ambiguous runtime ownership.
    config.validate()?;

    Ok(config)
}

/// Save MCP server configurations to the default location.
pub async fn save_mcp_servers(config: &McpServersFile) -> Result<(), ConfigError> {
    save_mcp_servers_to(config, default_config_path()).await
}

/// Save MCP server configurations to a specific path.
pub async fn save_mcp_servers_to(
    config: &McpServersFile,
    path: impl AsRef<Path>,
) -> Result<(), ConfigError> {
    config.validate()?;
    let path = path.as_ref();

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let content = serde_json::to_string_pretty(config)?;

    // Write to a temporary file first, then atomically rename to avoid
    // corrupting the config if the process crashes during the write.
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, content).await?;
    fs::rename(&tmp_path, path).await?;

    Ok(())
}

/// Add a new MCP server configuration.
pub async fn add_mcp_server(config: McpServerConfig) -> Result<(), ConfigError> {
    persist_mcp_server(config, true).await
}

/// Persist an MCP server while preserving concurrent changes from other processes.
pub async fn persist_mcp_server(
    config: McpServerConfig,
    overwrite: bool,
) -> Result<(), ConfigError> {
    config.validate()?;
    let _config_guard = acquire_mcp_config_write_guard().await?;

    let mut servers = load_mcp_servers().await?;
    if !overwrite && servers.get(&config.name).is_some() {
        return Err(ConfigError::ServerAlreadyExists {
            name: config.name.clone(),
        });
    }
    servers.upsert(config);
    save_mcp_servers(&servers).await?;

    Ok(())
}

/// Remove an MCP server by name.
pub async fn remove_mcp_server(name: &str) -> Result<(), ConfigError> {
    let _config_guard = acquire_mcp_config_write_guard().await?;
    let mut servers = load_mcp_servers().await?;

    if !servers.remove(name) {
        return Err(ConfigError::ServerNotFound {
            name: name.to_string(),
        });
    }

    save_mcp_servers(&servers).await?;

    Ok(())
}

/// Set an MCP server's enabled state on disk.
pub async fn set_mcp_server_enabled(
    name: &str,
    enabled: bool,
) -> Result<McpServerConfig, ConfigError> {
    let _config_guard = acquire_mcp_config_write_guard().await?;
    let mut servers = load_mcp_servers().await?;
    let server = servers
        .get_mut(name)
        .ok_or_else(|| ConfigError::ServerNotFound {
            name: name.to_string(),
        })?;
    server.enabled = enabled;
    if enabled {
        server.recovery_circuit_open = false;
    }
    let server = server.clone();
    save_mcp_servers(&servers).await?;
    Ok(server)
}

/// Atomically invert an MCP server's enabled state on disk.
pub async fn toggle_mcp_server_enabled(name: &str) -> Result<McpServerConfig, ConfigError> {
    let _config_guard = acquire_mcp_config_write_guard().await?;
    let mut servers = load_mcp_servers().await?;
    let server = servers
        .get_mut(name)
        .ok_or_else(|| ConfigError::ServerNotFound {
            name: name.to_string(),
        })?;
    server.enabled = !server.enabled;
    if server.enabled {
        server.recovery_circuit_open = false;
    }
    let server = server.clone();
    save_mcp_servers(&servers).await?;
    Ok(server)
}

/// Disable a disk-backed server only if its configuration has not changed.
pub async fn disable_mcp_server_if_unchanged(
    expected: &McpServerConfig,
) -> Result<bool, ConfigError> {
    let _config_guard = acquire_mcp_config_write_guard().await?;
    let mut servers = load_mcp_servers().await?;
    let Some(server) = servers.get_mut(&expected.name) else {
        return Ok(false);
    };
    if serde_json::to_value(&*server)? != serde_json::to_value(expected)? {
        return Ok(false);
    }
    server.enabled = false;
    save_mcp_servers(&servers).await?;
    Ok(true)
}

/// Persist an open recovery circuit without disabling explicit activation.
pub async fn mark_mcp_recovery_circuit_open(name: &str) -> Result<(), ConfigError> {
    let _config_guard = acquire_mcp_config_write_guard().await?;
    let mut servers = load_mcp_servers().await?;
    let server = servers
        .get_mut(name)
        .ok_or_else(|| ConfigError::ServerNotFound {
            name: name.to_string(),
        })?;
    server.recovery_circuit_open = true;
    save_mcp_servers(&servers).await
}

/// Get a specific MCP server configuration.
pub async fn get_mcp_server(name: &str) -> Result<McpServerConfig, ConfigError> {
    let servers = load_mcp_servers().await?;

    servers
        .get(name)
        .cloned()
        .ok_or_else(|| ConfigError::ServerNotFound {
            name: name.to_string(),
        })
}

// ==================== Database-backed MCP server config ====================

/// Load MCP server configurations from the database settings table.
pub async fn load_mcp_servers_from_db(
    store: &dyn crate::db::Database,
    user_id: &str,
) -> Result<McpServersFile, ConfigError> {
    match store.get_setting(user_id, "mcp_servers").await {
        Ok(Some(value)) => parse_mcp_servers_value(value),
        Ok(None) => Ok(McpServersFile::default()),
        Err(error) => Err(ConfigError::Io(std::io::Error::other(error))),
    }
}

/// Load DB configuration, migrating the legacy disk file into this owner once.
pub async fn load_mcp_servers_from_db_or_migrate(
    store: &dyn crate::db::Database,
    user_id: &str,
) -> Result<McpServersFile, ConfigError> {
    let current = store
        .get_setting(user_id, "mcp_servers")
        .await
        .map_err(|error| ConfigError::Io(std::io::Error::other(error)))?;
    if let Some(value) = current {
        return parse_mcp_servers_value(value);
    }

    let disk = load_mcp_servers().await?;
    let value = serde_json::to_value(&disk)?;
    if store
        .compare_and_set_setting(user_id, "mcp_servers", None, &value)
        .await
        .map_err(|error| ConfigError::Io(std::io::Error::other(error)))?
    {
        return Ok(disk);
    }
    load_mcp_servers_from_db(store, user_id).await
}

/// Save MCP server configurations to the database settings table.
pub async fn save_mcp_servers_to_db(
    store: &dyn crate::db::Database,
    user_id: &str,
    config: &McpServersFile,
) -> Result<(), ConfigError> {
    config.validate()?;
    let value = serde_json::to_value(config)?;
    store
        .set_setting(user_id, "mcp_servers", &value)
        .await
        .map_err(std::io::Error::other)?;
    Ok(())
}

/// Add a new MCP server configuration (DB-backed).
pub async fn add_mcp_server_db(
    store: &dyn crate::db::Database,
    user_id: &str,
    config: McpServerConfig,
) -> Result<(), ConfigError> {
    persist_mcp_server_db(store, user_id, config, true).await
}

/// Persist a DB-backed MCP server using optimistic concurrency control.
pub async fn persist_mcp_server_db(
    store: &dyn crate::db::Database,
    user_id: &str,
    config: McpServerConfig,
    overwrite: bool,
) -> Result<(), ConfigError> {
    config.validate()?;
    mutate_mcp_servers_db(store, user_id, |servers| {
        if !overwrite && servers.get(&config.name).is_some() {
            return Err(ConfigError::ServerAlreadyExists {
                name: config.name.clone(),
            });
        }
        servers.upsert(config.clone());
        Ok(())
    })
    .await
}

/// Remove an MCP server by name (DB-backed).
pub async fn remove_mcp_server_db(
    store: &dyn crate::db::Database,
    user_id: &str,
    name: &str,
) -> Result<(), ConfigError> {
    mutate_mcp_servers_db(store, user_id, |servers| {
        if servers.remove(name) {
            Ok(())
        } else {
            Err(ConfigError::ServerNotFound {
                name: name.to_string(),
            })
        }
    })
    .await
}

/// Set an MCP server's enabled state in the database atomically.
pub async fn set_mcp_server_enabled_db(
    store: &dyn crate::db::Database,
    user_id: &str,
    name: &str,
    enabled: bool,
) -> Result<McpServerConfig, ConfigError> {
    mutate_mcp_servers_db(store, user_id, |servers| {
        let server = servers
            .get_mut(name)
            .ok_or_else(|| ConfigError::ServerNotFound {
                name: name.to_string(),
            })?;
        server.enabled = enabled;
        if enabled {
            server.recovery_circuit_open = false;
        }
        Ok(server.clone())
    })
    .await
}

/// Atomically invert an MCP server's enabled state in the database.
pub async fn toggle_mcp_server_enabled_db(
    store: &dyn crate::db::Database,
    user_id: &str,
    name: &str,
) -> Result<McpServerConfig, ConfigError> {
    mutate_mcp_servers_db(store, user_id, |servers| {
        let server = servers
            .get_mut(name)
            .ok_or_else(|| ConfigError::ServerNotFound {
                name: name.to_string(),
            })?;
        server.enabled = !server.enabled;
        if server.enabled {
            server.recovery_circuit_open = false;
        }
        Ok(server.clone())
    })
    .await
}

/// Disable a DB-backed server only if its configuration has not changed.
pub async fn disable_mcp_server_if_unchanged_db(
    store: &dyn crate::db::Database,
    user_id: &str,
    expected: &McpServerConfig,
) -> Result<bool, ConfigError> {
    mutate_mcp_servers_db(store, user_id, |servers| {
        let Some(server) = servers.get_mut(&expected.name) else {
            return Ok(false);
        };
        if serde_json::to_value(&*server)? != serde_json::to_value(expected)? {
            return Ok(false);
        }
        server.enabled = false;
        Ok(true)
    })
    .await
}

/// Persist an open recovery circuit in owner-scoped configuration.
pub async fn mark_mcp_recovery_circuit_open_db(
    store: &dyn crate::db::Database,
    user_id: &str,
    name: &str,
) -> Result<(), ConfigError> {
    mutate_mcp_servers_db(store, user_id, |servers| {
        let server = servers
            .get_mut(name)
            .ok_or_else(|| ConfigError::ServerNotFound {
                name: name.to_string(),
            })?;
        server.recovery_circuit_open = true;
        Ok(())
    })
    .await
}

fn parse_mcp_servers_value(value: serde_json::Value) -> Result<McpServersFile, ConfigError> {
    let config: McpServersFile = serde_json::from_value(value)?;
    config.validate()?;
    Ok(config)
}

async fn mutate_mcp_servers_db<T>(
    store: &dyn crate::db::Database,
    user_id: &str,
    mut mutate: impl FnMut(&mut McpServersFile) -> Result<T, ConfigError>,
) -> Result<T, ConfigError> {
    const MAX_ATTEMPTS: usize = 16;

    for _ in 0..MAX_ATTEMPTS {
        let expected = store
            .get_setting(user_id, "mcp_servers")
            .await
            .map_err(|error| ConfigError::Io(std::io::Error::other(error)))?;
        let mut servers = match expected.as_ref() {
            Some(value) => parse_mcp_servers_value(value.clone())?,
            None => McpServersFile::default(),
        };
        let result = mutate(&mut servers)?;
        let value = serde_json::to_value(&servers)?;
        if store
            .compare_and_set_setting(user_id, "mcp_servers", expected.as_ref(), &value)
            .await
            .map_err(|error| ConfigError::Io(std::io::Error::other(error)))?
        {
            return Ok(result);
        }
        tokio::task::yield_now().await;
    }

    Err(ConfigError::ConcurrentUpdate)
}

/// Check if a URL points to a loopback address (localhost, 127.0.0.1, [::1]).
///
/// Uses `url::Url` for proper parsing so edge cases (IPv6, userinfo, ports)
/// are handled correctly without manual string splitting.
pub(crate) fn is_localhost_url(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    match parsed.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Resolved transport type (borrows from config).
#[derive(Debug)]
pub enum EffectiveTransport<'a> {
    Http,
    Stdio {
        command: &'a str,
        args: &'a [String],
        env: &'a HashMap<String, String>,
        env_secrets: &'a HashMap<String, String>,
    },
    Unix {
        socket_path: &'a str,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_is_localhost_url() {
        assert!(is_localhost_url("http://localhost:3000/path"));
        assert!(is_localhost_url("https://localhost/path"));
        assert!(is_localhost_url("http://127.0.0.1:8080"));
        assert!(is_localhost_url("http://127.0.0.1"));
        assert!(!is_localhost_url("https://notlocalhost.com/path"));
        assert!(!is_localhost_url("https://example-localhost.io"));
        assert!(!is_localhost_url("https://mcp.notion.com"));
        assert!(is_localhost_url("http://user:pass@localhost:3000/path"));
        // IPv6 loopback
        assert!(is_localhost_url("http://[::1]:8080/path"));
        assert!(is_localhost_url("http://[::1]/path"));
        assert!(!is_localhost_url("http://[::2]:8080/path"));
    }

    #[test]
    fn test_server_config_validation() {
        // Valid HTTPS server
        let config = McpServerConfig::new("notion", "https://mcp.notion.com");
        assert!(config.validate().is_ok());

        // Valid localhost (allowed for dev)
        let config = McpServerConfig::new("local", "http://localhost:8080");
        assert!(config.validate().is_ok());

        // Invalid: empty name
        let config = McpServerConfig::new("", "https://example.com");
        assert!(config.validate().is_err());

        // Invalid: HTTP for remote server
        let config = McpServerConfig::new("remote", "http://mcp.example.com");
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_oauth_config_builder() {
        let oauth = OAuthConfig::new("client-123")
            .with_endpoints(
                "https://auth.example.com/authorize",
                "https://auth.example.com/token",
            )
            .with_scopes(vec!["read".to_string(), "write".to_string()]);

        assert_eq!(oauth.client_id, "client-123");
        assert!(oauth.authorization_url.is_some());
        assert!(oauth.token_url.is_some());
        assert_eq!(oauth.scopes.len(), 2);
        assert!(oauth.use_pkce);
    }

    #[test]
    fn test_servers_file_operations() {
        let mut file = McpServersFile::default();

        // Add a server
        file.upsert(McpServerConfig::new("notion", "https://mcp.notion.com"));
        assert_eq!(file.servers.len(), 1);

        // Update the server
        let mut updated = McpServerConfig::new("notion", "https://mcp.notion.com/v2");
        updated.enabled = false;
        file.upsert(updated);
        assert_eq!(file.servers.len(), 1);
        assert!(!file.get("notion").unwrap().enabled);

        // Add another server
        file.upsert(McpServerConfig::new("github", "https://mcp.github.com"));
        assert_eq!(file.servers.len(), 2);

        // Remove a server
        assert!(file.remove("notion"));
        assert_eq!(file.servers.len(), 1);
        assert!(file.get("notion").is_none());

        // Remove non-existent server
        assert!(!file.remove("nonexistent"));
    }

    #[test]
    fn study_notes_config_is_read_only_stdio() {
        let dir = tempdir().unwrap();
        create_study_notes_fixture(dir.path());

        let config = study_notes_server_from_home(dir.path(), None, false).unwrap();
        assert_eq!(config.name, STUDY_NOTES_SERVER_NAME);
        assert!(config.enabled);
        assert!(config.always_require_approval.is_empty());
        match config.effective_transport() {
            EffectiveTransport::Stdio {
                command, args, env, ..
            } => {
                assert_eq!(command, "python3");
                assert!(args.iter().any(|arg| arg.ends_with("study_notes_mcp.py")));
                assert!(args.iter().any(|arg| arg.ends_with("agent-notes.sqlite3")));
                assert!(!args.iter().any(|arg| arg == "--allow-write"));
                assert!(env.is_empty());
            }
            other => panic!("expected stdio config, got {other:?}"),
        }
    }

    #[test]
    fn study_notes_config_rejects_incomplete_checkout() {
        let dir = tempdir().unwrap();
        let error = study_notes_server_from_home(dir.path(), None, false).unwrap_err();
        assert!(error.to_string().contains("study_notes_mcp.py"));
    }

    #[test]
    fn study_notes_write_config_uses_separate_state_and_forces_approval() {
        let code = tempdir().unwrap();
        let state = tempdir().unwrap();
        create_study_notes_fixture(code.path());
        std::fs::write(state.path().join("agent-notes.sqlite3"), "").unwrap();
        std::fs::create_dir(state.path().join("agent-notes-by-agent")).unwrap();

        let config = study_notes_server_from_home(code.path(), Some(state.path()), true).unwrap();
        assert_eq!(
            config.always_require_approval,
            STUDY_NOTES_WRITE_TOOLS.map(str::to_string)
        );
        assert!(
            validate_managed_study_notes_runtime_settings(
                &config,
                false,
                Some(code.path()),
                Some(state.path())
            )
            .is_err()
        );
        assert!(
            validate_managed_study_notes_runtime_settings(&config, true, Some(code.path()), None)
                .is_err()
        );
        validate_managed_study_notes_runtime_settings(
            &config,
            true,
            Some(code.path()),
            Some(state.path()),
        )
        .unwrap();
        match config.effective_transport() {
            EffectiveTransport::Stdio { args, .. } => {
                assert!(args.iter().any(|arg| arg == "--allow-write"));
                assert!(
                    args.iter()
                        .any(|arg| arg.starts_with(state.path().to_str().unwrap()))
                );
                assert!(!args.iter().any(|arg| {
                    arg.starts_with(code.path().to_str().unwrap())
                        && arg.ends_with("agent-notes.sqlite3")
                }));
            }
            other => panic!("expected stdio config, got {other:?}"),
        }
    }

    #[test]
    fn study_notes_write_config_requires_separate_state() {
        let dir = tempdir().unwrap();
        create_study_notes_fixture(dir.path());

        let missing = study_notes_server_from_home(dir.path(), None, true).unwrap_err();
        assert!(missing.to_string().contains(STUDY_NOTES_STATE_HOME_ENV));
        let overlapping =
            study_notes_server_from_home(dir.path(), Some(dir.path()), true).unwrap_err();
        assert!(overlapping.to_string().contains("must not overlap"));
    }

    #[cfg(unix)]
    #[test]
    fn study_notes_config_rejects_symlinked_executable() {
        use std::os::unix::fs::symlink;

        let code = tempdir().unwrap();
        let outside = tempdir().unwrap();
        create_study_notes_fixture(code.path());
        std::fs::write(outside.path().join("server.py"), "").unwrap();
        std::fs::remove_file(code.path().join("study_notes_mcp.py")).unwrap();
        symlink(
            outside.path().join("server.py"),
            code.path().join("study_notes_mcp.py"),
        )
        .unwrap();

        let error = study_notes_server_from_home(code.path(), None, false).unwrap_err();
        assert!(error.to_string().contains("must not be a symlink"));
    }

    #[test]
    fn study_notes_write_opt_in_is_strict() {
        assert!(!parse_study_notes_allow_write(None).unwrap());
        assert!(parse_study_notes_allow_write(Some(OsStr::new("true"))).unwrap());
        assert!(!parse_study_notes_allow_write(Some(OsStr::new("off"))).unwrap());
        assert!(parse_study_notes_allow_write(Some(OsStr::new("sometimes"))).is_err());
    }

    #[test]
    fn managed_study_notes_refresh_preserves_lifecycle_state() {
        let code = tempdir().unwrap();
        let state = tempdir().unwrap();
        create_study_notes_fixture(code.path());
        std::fs::write(state.path().join("agent-notes.sqlite3"), "").unwrap();
        std::fs::create_dir(state.path().join("agent-notes-by-agent")).unwrap();

        let mut existing = study_notes_server_from_home(code.path(), None, false).unwrap();
        existing.description = Some(LEGACY_STUDY_NOTES_DESCRIPTION.to_string());
        existing.enabled = false;
        existing.recovery_circuit_open = true;
        let mut servers = McpServersFile::default();
        servers.upsert(existing);
        let generated =
            study_notes_server_from_home(code.path(), Some(state.path()), true).unwrap();

        let replacement = apply_managed_study_notes_config(&mut servers, generated)
            .expect("managed config should refresh");
        assert!(!replacement.enabled);
        assert!(replacement.recovery_circuit_open);
        assert!(
            replacement
                .always_require_approval
                .contains(&"knowledge_submit_note".to_string())
        );
        let mut persisted_write = McpServersFile::default();
        persisted_write.upsert(replacement);
        assert!(suppress_unapproved_managed_study_notes_write(
            &mut persisted_write
        ));
        assert!(suppress_managed_study_notes_server(&mut servers));
        assert!(servers.get(STUDY_NOTES_SERVER_NAME).is_none());
    }

    #[test]
    fn managed_study_notes_refresh_does_not_replace_custom_server() {
        let mut servers = McpServersFile::default();
        servers.upsert(
            McpServerConfig::new_stdio(
                STUDY_NOTES_SERVER_NAME,
                "custom-server",
                Vec::new(),
                HashMap::new(),
            )
            .with_description("Operator-owned Study Notes server"),
        );
        let generated = servers.get(STUDY_NOTES_SERVER_NAME).unwrap().clone();

        assert!(apply_managed_study_notes_config(&mut servers, generated).is_none());
        assert!(!suppress_managed_study_notes_server(&mut servers));
        assert!(!suppress_unapproved_managed_study_notes_write(&mut servers));
        assert!(servers.get(STUDY_NOTES_SERVER_NAME).is_some());
    }

    fn create_study_notes_fixture(path: &Path) {
        std::fs::write(path.join("study_notes_mcp.py"), "").unwrap();
        std::fs::write(path.join("memorydb.py"), "").unwrap();
        std::fs::write(path.join("agent-notes.sqlite3"), "").unwrap();
        std::fs::create_dir(path.join("agent-notes-by-agent")).unwrap();
    }

    #[tokio::test]
    async fn test_load_save_config() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("mcp-servers.json");

        // Save a configuration
        let mut config = McpServersFile::default();
        config.upsert(
            McpServerConfig::new("notion", "https://mcp.notion.com").with_oauth(
                OAuthConfig::new("client-123")
                    .with_scopes(vec!["read".to_string(), "write".to_string()]),
            ),
        );

        save_mcp_servers_to(&config, &path).await.unwrap();

        // Load it back
        let loaded = load_mcp_servers_from(&path).await.unwrap();
        assert_eq!(loaded.servers.len(), 1);

        let server = loaded.get("notion").unwrap();
        assert_eq!(server.url, "https://mcp.notion.com");
        assert!(server.oauth.is_some());
        assert_eq!(server.oauth.as_ref().unwrap().client_id, "client-123");
    }

    #[tokio::test]
    async fn test_load_nonexistent_returns_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");

        let config = load_mcp_servers_from(&path).await.unwrap();
        assert!(config.servers.is_empty());
    }

    #[tokio::test]
    async fn test_load_rejects_corrupted_headers() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("mcp-servers.json");

        // Write a config with an invalid header name directly to disk,
        // bypassing the add_mcp_server() validation path.
        let corrupted = serde_json::json!({
            "servers": [{
                "name": "bad-server",
                "url": "https://mcp.example.com",
                "enabled": true,
                "headers": { "X Bad": "value" }
            }]
        });
        tokio::fs::write(&path, corrupted.to_string())
            .await
            .unwrap();

        let result = load_mcp_servers_from(&path).await;
        assert!(result.is_err(), "Load should reject corrupted headers");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("bad-server"),
            "Error should name the offending server, got: {err}"
        );
    }

    #[tokio::test]
    async fn load_rejects_duplicate_server_names() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("mcp-servers.json");
        let duplicate = serde_json::json!({
            "servers": [
                {"name": "duplicate", "url": "https://one.example.com"},
                {"name": "duplicate", "url": "https://two.example.com"}
            ]
        });
        tokio::fs::write(&path, duplicate.to_string())
            .await
            .unwrap();

        let error = load_mcp_servers_from(&path).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Duplicate server name: 'duplicate'")
        );
    }

    #[test]
    fn recovery_circuit_blocks_auto_start_until_explicit_enable() {
        let mut config =
            McpServerConfig::new_stdio("blocked", "server", Vec::new(), HashMap::new());
        config.recovery_circuit_open = true;
        assert!(!config.should_auto_start());

        let mut servers = McpServersFile::default();
        servers.upsert(config);
        let server = servers.get_mut("blocked").unwrap();
        server.enabled = true;
        server.recovery_circuit_open = false;
        assert!(server.should_auto_start());
    }

    #[test]
    fn test_token_secret_names() {
        let config = McpServerConfig::new("notion", "https://mcp.notion.com");
        assert_eq!(config.token_secret_name(), "mcp_notion_access_token");
        assert_eq!(
            config.refresh_token_secret_name(),
            "mcp_notion_refresh_token"
        );
    }

    #[test]
    fn test_requires_auth_with_oauth() {
        let config = McpServerConfig::new("notion", "https://mcp.notion.com")
            .with_oauth(OAuthConfig::new("client-123"));
        assert!(config.requires_auth());
    }

    #[test]
    fn test_requires_auth_remote_https_without_oauth() {
        // Remote HTTPS servers need auth even without pre-configured OAuth (DCR)
        let config = McpServerConfig::new("github-copilot", "https://api.githubcopilot.com/mcp/");
        assert!(config.requires_auth());

        let config = McpServerConfig::new("notion", "https://mcp.notion.com");
        assert!(config.requires_auth());
    }

    #[test]
    fn test_requires_auth_localhost_no_auth() {
        // Localhost servers are dev servers, no auth needed
        let config = McpServerConfig::new("local", "http://localhost:8080");
        assert!(!config.requires_auth());

        let config = McpServerConfig::new("local", "http://127.0.0.1:3000/mcp");
        assert!(!config.requires_auth());

        // Even HTTPS localhost doesn't require auth
        let config = McpServerConfig::new("local", "https://localhost:8443");
        assert!(!config.requires_auth());
    }

    #[test]
    fn test_requires_auth_http_remote_no_auth() {
        // HTTP remote servers won't pass validation, but if they existed
        // they wouldn't trigger HTTPS auth detection
        let config = McpServerConfig::new("bad", "http://mcp.example.com");
        assert!(!config.requires_auth());
    }

    #[test]
    fn test_stdio_config_creation() {
        let env = HashMap::from([("PATH".to_string(), "/usr/bin".to_string())]);
        let config = McpServerConfig::new_stdio(
            "my-server",
            "npx",
            vec!["-y".to_string(), "@modelcontextprotocol/server".to_string()],
            env.clone(),
        );

        assert_eq!(config.name, "my-server");
        assert!(config.url.is_empty());
        assert!(config.enabled);
        assert!(config.oauth.is_none());
        assert!(config.headers.is_empty());

        match &config.transport {
            Some(McpTransportConfig::Stdio {
                command,
                args,
                env: e,
                env_secrets,
            }) => {
                assert_eq!(command, "npx");
                assert_eq!(
                    args,
                    &["-y".to_string(), "@modelcontextprotocol/server".to_string()]
                );
                assert_eq!(e, &env);
                assert!(env_secrets.is_empty());
            }
            other => panic!("Expected Stdio transport, got {:?}", other),
        }
    }

    #[test]
    fn test_unix_config_creation() {
        let config = McpServerConfig::new_unix("local-server", "/tmp/mcp.sock");

        assert_eq!(config.name, "local-server");
        assert!(config.url.is_empty());
        assert!(config.enabled);

        match &config.transport {
            Some(McpTransportConfig::Unix { socket_path }) => {
                assert_eq!(socket_path, "/tmp/mcp.sock");
            }
            other => panic!("Expected Unix transport, got {:?}", other),
        }
    }

    #[test]
    fn test_stdio_validation() {
        // Valid stdio config
        let config = McpServerConfig::new_stdio("server", "npx", vec![], HashMap::new());
        assert!(config.validate().is_ok());

        // Invalid: empty command
        let config = McpServerConfig::new_stdio("server", "", vec![], HashMap::new());
        assert!(config.validate().is_err());
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("command"),
            "Error should mention command: {}",
            err
        );

        // Invalid: empty name
        let config = McpServerConfig::new_stdio("", "npx", vec![], HashMap::new());
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_stdio_validation_rejects_invalid_environment() {
        let invalid_name = McpServerConfig::new_stdio(
            "server",
            "npx",
            vec![],
            HashMap::from([("BAD=NAME".to_string(), "value".to_string())]),
        );
        assert!(invalid_name.validate().is_err());

        let invalid_value = McpServerConfig::new_stdio(
            "server",
            "npx",
            vec![],
            HashMap::from([("GOOD_NAME".to_string(), "bad\0value".to_string())]),
        );
        assert!(invalid_value.validate().is_err());
    }

    #[test]
    fn test_stdio_secret_environment_validation() {
        let valid = McpServerConfig::new_stdio_with_secrets(
            "server",
            "npx",
            vec![],
            HashMap::new(),
            HashMap::from([("API_TOKEN".to_string(), "api_token".to_string())]),
        );
        assert!(valid.validate().is_ok());

        let invalid_name = McpServerConfig::new_stdio_with_secrets(
            "server",
            "npx",
            vec![],
            HashMap::new(),
            HashMap::from([("BAD=NAME".to_string(), "api_token".to_string())]),
        );
        assert!(invalid_name.validate().is_err());

        let empty_secret = McpServerConfig::new_stdio_with_secrets(
            "server",
            "npx",
            vec![],
            HashMap::new(),
            HashMap::from([("API_TOKEN".to_string(), "  ".to_string())]),
        );
        assert!(empty_secret.validate().is_err());

        let overlap = McpServerConfig::new_stdio_with_secrets(
            "server",
            "npx",
            vec![],
            HashMap::from([("API_TOKEN".to_string(), "plain".to_string())]),
            HashMap::from([("API_TOKEN".to_string(), "api_token".to_string())]),
        );
        let error = overlap.validate().expect_err("overlap must fail");
        assert!(
            error
                .to_string()
                .contains("both a plain value and a secret")
        );
    }

    #[test]
    fn test_unix_validation() {
        // Valid unix config
        let config = McpServerConfig::new_unix("server", "/tmp/mcp.sock");
        assert!(config.validate().is_ok());

        // Invalid: empty socket path
        let config = McpServerConfig::new_unix("server", "");
        assert!(config.validate().is_err());
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("socket"),
            "Error should mention socket: {}",
            err
        );

        // Invalid: empty name
        let config = McpServerConfig::new_unix("", "/tmp/mcp.sock");
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_requires_auth_stdio_never() {
        // Stdio transport should never require auth, even with OAuth configured
        let mut config = McpServerConfig::new_stdio("server", "npx", vec![], HashMap::new());
        assert!(!config.requires_auth());

        // Even if OAuth is set, stdio doesn't use HTTP auth
        config.oauth = Some(OAuthConfig::new("client-123"));
        assert!(!config.requires_auth());
    }

    #[test]
    fn test_requires_auth_unix_never() {
        // Unix transport should never require auth
        let mut config = McpServerConfig::new_unix("server", "/tmp/mcp.sock");
        assert!(!config.requires_auth());

        config.oauth = Some(OAuthConfig::new("client-123"));
        assert!(!config.requires_auth());
    }

    #[test]
    fn test_header_crlf_injection_rejected() {
        let mut headers = HashMap::new();
        headers.insert("X-Good".to_string(), "safe".to_string());
        headers.insert("X-Bad\r\nInjected: true".to_string(), "value".to_string());

        let config =
            McpServerConfig::new("server", "https://mcp.example.com").with_headers(headers);
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("not a valid HTTP header name"),
            "Expected RFC 9110 error, got: {err}"
        );
    }

    #[test]
    fn test_header_value_crlf_injection_rejected() {
        let mut headers = HashMap::new();
        headers.insert(
            "X-Header".to_string(),
            "value\r\nInjected: true".to_string(),
        );

        let config =
            McpServerConfig::new("server", "https://mcp.example.com").with_headers(headers);
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("invalid characters"),
            "Expected invalid characters error, got: {err}"
        );
    }

    #[test]
    fn test_header_name_with_space_rejected() {
        let headers = HashMap::from([("X Bad".to_string(), "value".to_string())]);
        let config =
            McpServerConfig::new("server", "https://mcp.example.com").with_headers(headers);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_header_name_with_colon_rejected() {
        let headers = HashMap::from([("X:Bad".to_string(), "value".to_string())]);
        let config =
            McpServerConfig::new("server", "https://mcp.example.com").with_headers(headers);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_header_name_with_null_byte_rejected() {
        let headers = HashMap::from([("X-Bad\0".to_string(), "value".to_string())]);
        let config =
            McpServerConfig::new("server", "https://mcp.example.com").with_headers(headers);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_header_empty_name_rejected() {
        let mut headers = HashMap::new();
        headers.insert(String::new(), "value".to_string());

        let config =
            McpServerConfig::new("server", "https://mcp.example.com").with_headers(headers);
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("empty"),
            "Expected empty name error, got: {err}"
        );
    }

    #[test]
    fn test_case_variant_duplicate_headers_rejected() {
        let headers = HashMap::from([
            ("Authorization".to_string(), "Bearer first".to_string()),
            ("authorization".to_string(), "Bearer second".to_string()),
        ]);
        let config =
            McpServerConfig::new("server", "https://mcp.example.com").with_headers(headers);

        assert!(config.validate().is_err());
    }

    #[test]
    fn test_custom_session_header_rejected() {
        let headers = HashMap::from([("Mcp-Session-Id".to_string(), "stale".to_string())]);
        let config =
            McpServerConfig::new("server", "https://mcp.example.com").with_headers(headers);

        assert!(config.validate().is_err());
    }

    #[test]
    fn test_has_custom_auth_header_case_insensitive() {
        let headers = HashMap::from([("authorization".to_string(), "Bearer token".to_string())]);
        let config =
            McpServerConfig::new("server", "https://mcp.example.com").with_headers(headers);
        assert!(config.has_custom_auth_header());

        let headers = HashMap::from([("AUTHORIZATION".to_string(), "Bearer token".to_string())]);
        let config =
            McpServerConfig::new("server", "https://mcp.example.com").with_headers(headers);
        assert!(config.has_custom_auth_header());

        let headers = HashMap::from([("X-Api-Key".to_string(), "key".to_string())]);
        let config =
            McpServerConfig::new("server", "https://mcp.example.com").with_headers(headers);
        assert!(!config.has_custom_auth_header());
    }

    #[test]
    fn test_custom_headers() {
        let headers = HashMap::from([
            ("X-Api-Key".to_string(), "secret".to_string()),
            ("Authorization".to_string(), "Bearer token".to_string()),
        ]);
        let config =
            McpServerConfig::new("server", "https://mcp.example.com").with_headers(headers.clone());

        assert_eq!(config.headers, headers);
        assert_eq!(config.headers.get("X-Api-Key").unwrap(), "secret");
    }

    #[test]
    fn test_transport_config_serde_http() {
        let transport = McpTransportConfig::Http;
        let json = serde_json::to_string(&transport).unwrap();
        assert!(json.contains("\"transport\":\"http\""));

        let parsed: McpTransportConfig = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, McpTransportConfig::Http));
    }

    #[test]
    fn test_transport_config_serde_stdio() {
        let transport = McpTransportConfig::Stdio {
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "server".to_string()],
            env: HashMap::from([("KEY".to_string(), "val".to_string())]),
            env_secrets: HashMap::from([("API_TOKEN".to_string(), "api_token".to_string())]),
        };
        let json = serde_json::to_string(&transport).unwrap();
        assert!(json.contains("\"transport\":\"stdio\""));
        assert!(json.contains("\"command\":\"npx\""));

        let parsed: McpTransportConfig = serde_json::from_str(&json).unwrap();
        match parsed {
            McpTransportConfig::Stdio {
                command,
                args,
                env,
                env_secrets,
            } => {
                assert_eq!(command, "npx");
                assert_eq!(args, vec!["-y".to_string(), "server".to_string()]);
                assert_eq!(env.get("KEY").unwrap(), "val");
                assert_eq!(env_secrets.get("API_TOKEN").unwrap(), "api_token");
            }
            other => panic!("Expected Stdio, got {:?}", other),
        }
    }

    #[test]
    fn test_transport_config_serde_unix() {
        let transport = McpTransportConfig::Unix {
            socket_path: "/tmp/mcp.sock".to_string(),
        };
        let json = serde_json::to_string(&transport).unwrap();
        assert!(json.contains("\"transport\":\"unix\""));
        assert!(json.contains("\"socket_path\":\"/tmp/mcp.sock\""));

        let parsed: McpTransportConfig = serde_json::from_str(&json).unwrap();
        match parsed {
            McpTransportConfig::Unix { socket_path } => {
                assert_eq!(socket_path, "/tmp/mcp.sock");
            }
            other => panic!("Expected Unix, got {:?}", other),
        }
    }

    #[test]
    fn test_backward_compat_no_transport_field() {
        // Existing configs without transport field should still deserialize
        let json = r#"{
            "name": "notion",
            "url": "https://mcp.notion.com",
            "enabled": true
        }"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.name, "notion");
        assert_eq!(config.url, "https://mcp.notion.com");
        assert!(config.transport.is_none());
        assert!(config.headers.is_empty());
        assert!(config.always_require_approval.is_empty());
        assert!(matches!(
            config.effective_transport(),
            EffectiveTransport::Http
        ));
    }

    #[test]
    fn test_config_roundtrip_with_transport() {
        // Test full roundtrip with stdio transport
        let config = McpServerConfig::new_stdio(
            "test-server",
            "node",
            vec!["server.js".to_string()],
            HashMap::from([("NODE_ENV".to_string(), "production".to_string())]),
        )
        .with_description("A test server");

        let json = serde_json::to_string_pretty(&config).unwrap();
        let parsed: McpServerConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.name, "test-server");
        assert!(parsed.url.is_empty());
        assert_eq!(parsed.description.as_deref(), Some("A test server"));

        match &parsed.transport {
            Some(McpTransportConfig::Stdio {
                command,
                args,
                env,
                env_secrets,
            }) => {
                assert_eq!(command, "node");
                assert_eq!(args, &["server.js".to_string()]);
                assert_eq!(env.get("NODE_ENV").unwrap(), "production");
                assert!(env_secrets.is_empty());
            }
            other => panic!("Expected Stdio transport, got {:?}", other),
        }

        // Test full roundtrip with unix transport
        let config = McpServerConfig::new_unix("unix-server", "/var/run/mcp.sock");
        let json = serde_json::to_string_pretty(&config).unwrap();
        let parsed: McpServerConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.name, "unix-server");
        match &parsed.transport {
            Some(McpTransportConfig::Unix { socket_path }) => {
                assert_eq!(socket_path, "/var/run/mcp.sock");
            }
            other => panic!("Expected Unix transport, got {:?}", other),
        }

        // Test roundtrip with HTTP + headers
        let headers = HashMap::from([("X-Custom".to_string(), "value".to_string())]);
        let config =
            McpServerConfig::new("http-server", "https://mcp.example.com").with_headers(headers);
        let json = serde_json::to_string_pretty(&config).unwrap();
        let parsed: McpServerConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.name, "http-server");
        assert!(parsed.transport.is_none());
        assert_eq!(parsed.headers.get("X-Custom").unwrap(), "value");
    }

    // --- Issue 3 regression: is_localhost_url rejects attacker subdomains ---

    #[test]
    fn test_is_localhost_url_rejects_attacker_subdomain() {
        // Before the fix, url.contains("localhost") matched this.
        assert!(
            !is_localhost_url("http://evil.localhost.attacker.com:8080/mcp"),
            "attacker subdomain containing 'localhost' must not be treated as local"
        );
    }

    #[test]
    fn test_is_localhost_url_accepts_real_localhost() {
        assert!(is_localhost_url("http://localhost:8080/mcp"));
        assert!(is_localhost_url("https://localhost/path"));
    }

    #[test]
    fn test_is_localhost_url_accepts_loopback_ip() {
        assert!(is_localhost_url("http://127.0.0.1:3000"));
        assert!(is_localhost_url("http://[::1]:3000"));
    }

    #[test]
    fn test_is_localhost_url_rejects_remote() {
        assert!(!is_localhost_url("https://mcp.example.com"));
        assert!(!is_localhost_url("http://192.168.1.1:8080"));
    }
}
