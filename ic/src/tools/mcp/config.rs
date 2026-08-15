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

/// Deployment shape for an MCP gateway endpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpGatewayMode {
    /// One gateway endpoint is dedicated to this MCP server.
    #[default]
    Dedicated,
    /// One gateway endpoint routes multiple MCP servers by route name.
    Shared,
}

/// Runtime placement responsible for activating an MCP server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpPlacement {
    /// Run in the LunarWing daemon host runtime.
    #[default]
    Host,
    /// Run inside an isolated job worker.
    JobWorker,
    /// Activated by a named persistent external worker speaking
    /// `lunarwing-agent-v1` (see `external_worker`). Unlike job workers,
    /// external workers are long-lived: the placement persists across daemon
    /// restarts and jobs, and the server stays registered with the worker.
    ExternalWorker,
}

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
    /// Stateful MCP server implemented as a WASM component.
    Wasm {
        component_path: PathBuf,
        #[serde(default = "default_wasm_memory_mb")]
        memory_mb: u64,
        #[serde(default = "default_wasm_fuel")]
        fuel: u64,
        #[serde(default = "default_wasm_timeout_seconds")]
        timeout_seconds: u64,
    },
    /// MCP endpoint hosted behind a dedicated or shared gateway.
    Gateway {
        #[serde(default)]
        mode: McpGatewayMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        route: Option<String>,
    },
    /// Out-of-tree runtime activated through a registered external runtime
    /// descriptor (see `descriptor.rs`). The descriptor's adapter process is
    /// spawned and bridges the foreign runtime to MCP over stdio.
    External {
        /// Runtime kind name registered in the external runtime registry.
        runtime: String,
        /// Opaque options handed to the adapter as JSON.
        #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
        options: serde_json::Value,
    },
}

const fn default_wasm_memory_mb() -> u64 {
    64
}

const fn default_wasm_fuel() -> u64 {
    crate::tools::wasm::DEFAULT_FUEL_LIMIT
}

const fn default_wasm_timeout_seconds() -> u64 {
    60
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

    /// Runtime placement responsible for activating this server.
    #[serde(default)]
    pub placement: McpPlacement,

    /// Name of the persistent external worker (from the orchestrator's
    /// `external_workers` configuration) that activates this server. Required
    /// when `placement` is `external_worker`; forbidden otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_worker: Option<String>,

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

    /// Package provenance record (identity, trust state, runtime artifact digest).
    /// Absent means unverified legacy behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<crate::tools::mcp::provenance::McpPackageProvenance>,

    /// Host OS sandbox policy for a stdio server child process.
    /// Absent means legacy unsandboxed execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<crate::tools::mcp::sandbox::McpSandboxConfig>,
}

fn default_true() -> bool {
    true
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl McpServerConfig {
    /// Whether daemon startup may activate this host server automatically.
    pub fn should_host_auto_start(&self) -> bool {
        self.enabled && !self.recovery_circuit_open && self.placement == McpPlacement::Host
    }

    /// Whether a job worker may activate this server.
    pub fn should_run_in_job(&self) -> bool {
        self.enabled && !self.recovery_circuit_open && self.placement == McpPlacement::JobWorker
    }

    /// Whether a persistent external worker should activate this server.
    pub fn should_run_in_external_worker(&self) -> bool {
        self.enabled
            && !self.recovery_circuit_open
            && self.placement == McpPlacement::ExternalWorker
    }

    /// Backward-compatible alias for host daemon startup.
    pub fn should_auto_start(&self) -> bool {
        self.should_host_auto_start()
    }

    /// Create a new MCP server configuration.
    pub fn new(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
            transport: None,
            placement: McpPlacement::Host,
            external_worker: None,
            headers: HashMap::new(),
            oauth: None,
            enabled: true,
            recovery_circuit_open: false,
            description: None,
            always_require_approval: Vec::new(),
            provenance: None,
            sandbox: None,
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
            placement: McpPlacement::Host,
            external_worker: None,
            headers: HashMap::new(),
            oauth: None,
            enabled: true,
            recovery_circuit_open: false,
            description: None,
            always_require_approval: Vec::new(),
            provenance: None,
            sandbox: None,
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
            placement: McpPlacement::Host,
            external_worker: None,
            headers: HashMap::new(),
            oauth: None,
            enabled: true,
            recovery_circuit_open: false,
            description: None,
            always_require_approval: Vec::new(),
            provenance: None,
            sandbox: None,
        }
    }

    /// Create a stateful MCP-over-WASM server configuration.
    pub fn new_wasm(name: impl Into<String>, component_path: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            url: String::new(),
            transport: Some(McpTransportConfig::Wasm {
                component_path: component_path.into(),
                memory_mb: default_wasm_memory_mb(),
                fuel: default_wasm_fuel(),
                timeout_seconds: default_wasm_timeout_seconds(),
            }),
            placement: McpPlacement::Host,
            external_worker: None,
            headers: HashMap::new(),
            oauth: None,
            enabled: true,
            recovery_circuit_open: false,
            description: None,
            always_require_approval: Vec::new(),
            provenance: None,
            sandbox: None,
        }
    }

    /// Create an MCP server hosted behind a dedicated or shared gateway.
    pub fn new_gateway(
        name: impl Into<String>,
        endpoint: impl Into<String>,
        mode: McpGatewayMode,
        route: Option<String>,
    ) -> Self {
        let mut config = Self::new(name, endpoint);
        config.transport = Some(McpTransportConfig::Gateway { mode, route });
        config
    }

    /// Create an MCP server activated through a registered external runtime
    /// descriptor.
    pub fn new_external(
        name: impl Into<String>,
        runtime: impl Into<String>,
        options: serde_json::Value,
    ) -> Self {
        let mut config = Self::new(name, "");
        config.transport = Some(McpTransportConfig::External {
            runtime: runtime.into(),
            options,
        });
        config
    }

    /// Override resource limits for an MCP-over-WASM server.
    pub fn with_wasm_limits(mut self, memory_mb: u64, fuel: u64, timeout_seconds: u64) -> Self {
        if let Some(McpTransportConfig::Wasm {
            memory_mb: configured_memory,
            fuel: configured_fuel,
            timeout_seconds: configured_timeout,
            ..
        }) = &mut self.transport
        {
            *configured_memory = memory_mb;
            *configured_fuel = fuel;
            *configured_timeout = timeout_seconds;
        }
        self
    }

    /// Set runtime placement.
    pub fn with_placement(mut self, placement: McpPlacement) -> Self {
        self.placement = placement;
        self
    }

    /// Place this server on a named persistent external worker.
    pub fn with_external_worker(mut self, worker_name: impl Into<String>) -> Self {
        self.placement = McpPlacement::ExternalWorker;
        self.external_worker = Some(worker_name.into());
        self
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

    /// Set the host OS sandbox policy (stdio servers only).
    pub fn with_sandbox(mut self, sandbox: crate::tools::mcp::sandbox::McpSandboxConfig) -> Self {
        self.sandbox = Some(sandbox);
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
            Some(McpTransportConfig::Wasm {
                component_path,
                memory_mb,
                fuel,
                timeout_seconds,
            }) => EffectiveTransport::Wasm {
                component_path,
                memory_mb: *memory_mb,
                fuel: *fuel,
                timeout_seconds: *timeout_seconds,
            },
            Some(McpTransportConfig::Gateway { mode, route }) => EffectiveTransport::Gateway {
                mode: *mode,
                route: route.as_deref(),
            },
            Some(McpTransportConfig::External { runtime, options }) => {
                EffectiveTransport::External { runtime, options }
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

        if self.placement == McpPlacement::JobWorker {
            if !matches!(self.effective_transport(), EffectiveTransport::Stdio { .. }) {
                return Err(ConfigError::InvalidConfig {
                    reason: "Job-worker MCP servers only support stdio transport".to_string(),
                });
            }
            if self.sandbox.is_some() {
                return Err(ConfigError::InvalidConfig {
                    reason: "Job-worker MCP servers cannot use host sandbox policies".to_string(),
                });
            }
            if self.oauth.is_some() {
                return Err(ConfigError::InvalidConfig {
                    reason: "Job-worker MCP servers cannot use OAuth".to_string(),
                });
            }
            if !self.headers.is_empty() {
                return Err(ConfigError::InvalidConfig {
                    reason: "Job-worker MCP servers cannot use custom headers".to_string(),
                });
            }
        }

        match (self.placement, &self.external_worker) {
            (McpPlacement::ExternalWorker, Some(worker)) => {
                if worker.trim().is_empty()
                    || worker.len() > 128
                    || worker.bytes().any(|b| b < 0x21 || b == 0x7f)
                {
                    return Err(ConfigError::InvalidConfig {
                        reason: format!("Invalid external worker name '{worker}'"),
                    });
                }
                // The worker applies its own containment; host sandbox policies
                // and interactive OAuth flows cannot run on an external worker.
                if self.sandbox.is_some() {
                    return Err(ConfigError::InvalidConfig {
                        reason: "External-worker MCP servers cannot use host sandbox policies"
                            .to_string(),
                    });
                }
                if self.oauth.is_some() {
                    return Err(ConfigError::InvalidConfig {
                        reason: "External-worker MCP servers cannot use OAuth".to_string(),
                    });
                }
            }
            (McpPlacement::ExternalWorker, None) => {
                return Err(ConfigError::InvalidConfig {
                    reason: "external_worker placement requires an external_worker name"
                        .to_string(),
                });
            }
            (_, Some(_)) => {
                return Err(ConfigError::InvalidConfig {
                    reason: "external_worker is only valid with external_worker placement"
                        .to_string(),
                });
            }
            _ => {}
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
            EffectiveTransport::Wasm {
                component_path,
                memory_mb,
                fuel,
                timeout_seconds,
            } => {
                if !component_path.is_absolute() || component_path.as_os_str().is_empty() {
                    return Err(ConfigError::InvalidConfig {
                        reason: "MCP-over-WASM component_path must be an absolute path".to_string(),
                    });
                }
                if !(1..=1024).contains(&memory_mb) {
                    return Err(ConfigError::InvalidConfig {
                        reason: "MCP-over-WASM memory_mb must be between 1 and 1024".to_string(),
                    });
                }
                if fuel == 0 {
                    return Err(ConfigError::InvalidConfig {
                        reason: "MCP-over-WASM fuel must be greater than zero".to_string(),
                    });
                }
                if !(1..=300).contains(&timeout_seconds) {
                    return Err(ConfigError::InvalidConfig {
                        reason: "MCP-over-WASM timeout_seconds must be between 1 and 300"
                            .to_string(),
                    });
                }
            }
            EffectiveTransport::Gateway { mode, route } => {
                let parsed =
                    url::Url::parse(&self.url).map_err(|error| ConfigError::InvalidConfig {
                        reason: format!("Invalid MCP gateway endpoint: {error}"),
                    })?;
                let secure = parsed.scheme() == "https";
                let local = parsed.scheme() == "http" && is_localhost_url(&self.url);
                if !secure && !local {
                    return Err(ConfigError::InvalidConfig {
                        reason: "MCP gateway endpoints must use HTTPS (HTTP loopback is allowed)"
                            .to_string(),
                    });
                }
                match (mode, route) {
                    (McpGatewayMode::Dedicated, None) => {}
                    (McpGatewayMode::Dedicated, Some(_)) => {
                        return Err(ConfigError::InvalidConfig {
                            reason: "Dedicated MCP gateways cannot declare a shared route"
                                .to_string(),
                        });
                    }
                    (McpGatewayMode::Shared, Some(route))
                        if !route.is_empty()
                            && route == route.trim()
                            && route.len() <= 128
                            && reqwest::header::HeaderValue::from_str(route).is_ok() => {}
                    (McpGatewayMode::Shared, _) => {
                        return Err(ConfigError::InvalidConfig {
                            reason:
                                "Shared MCP gateways require a valid route of at most 128 bytes"
                                    .to_string(),
                        });
                    }
                }
            }
            EffectiveTransport::External { runtime, options } => {
                crate::tools::mcp::descriptor::validate_runtime_name(runtime).map_err(|error| {
                    ConfigError::InvalidConfig {
                        reason: error.to_string(),
                    }
                })?;
                let options_json =
                    serde_json::to_string(options).map_err(|error| ConfigError::InvalidConfig {
                        reason: format!("external runtime options are not serializable: {error}"),
                    })?;
                if options_json.len() > crate::tools::mcp::descriptor::MAX_EXTERNAL_OPTIONS_BYTES {
                    return Err(ConfigError::InvalidConfig {
                        reason: format!(
                            "external runtime options exceed {} bytes",
                            crate::tools::mcp::descriptor::MAX_EXTERNAL_OPTIONS_BYTES
                        ),
                    });
                }
                if self.oauth.is_some() {
                    return Err(ConfigError::InvalidConfig {
                        reason: "external runtime MCP servers cannot use OAuth".to_string(),
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
                "mcp-session-id"
                    | "content-type"
                    | "accept"
                    | "x-lunarwing-mcp-gateway-mode"
                    | "x-lunarwing-mcp-route"
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
            if provenance.component_sha256.is_some()
                && !matches!(self.effective_transport(), EffectiveTransport::Wasm { .. })
            {
                return Err(ConfigError::InvalidConfig {
                    reason: "provenance component digests only apply to WASM servers".to_string(),
                });
            }
        }

        if let Some(sandbox) = &self.sandbox {
            sandbox.validate()?;
            // External runtimes spawn a local stdio adapter process, so host
            // sandbox policies apply to them exactly like stdio servers.
            if !matches!(
                self.effective_transport(),
                EffectiveTransport::Stdio { .. } | EffectiveTransport::External { .. }
            ) {
                return Err(ConfigError::InvalidConfig {
                    reason: "sandbox policies only apply to stdio servers".to_string(),
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
    /// Non-network transports (stdio, unix, WASM) never require auth.
    pub fn requires_auth(&self) -> bool {
        // Only direct HTTP and gateway transports use HTTP authentication.
        if !matches!(
            self.effective_transport(),
            EffectiveTransport::Http | EffectiveTransport::Gateway { .. }
        ) {
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

    /// When true, every enabled host-placement stdio server must carry a
    /// sandbox policy covering the mandatory baseline
    /// ([`crate::tools::mcp::sandbox::McpSandboxConfig::default_profile`]'s
    /// mandatory minimum). Defaults to false (legacy opt-in sandboxing).
    #[serde(default, skip_serializing_if = "is_false")]
    pub require_sandbox_profiles: bool,

    /// Marketplace trust anchor and revocation distribution configuration.
    /// Absent means legacy behavior: no publisher trust store is consulted
    /// and no revocation list is enforced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marketplace: Option<MarketplaceConfig>,

    /// External runtime descriptors available to `transport = "external"`
    /// server entries. Empty means no external runtimes are registered and
    /// external transports fail closed with an unknown-runtime error.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_runtimes: Vec<crate::tools::mcp::descriptor::ExternalRuntimeDescriptor>,
}

/// Marketplace trust anchor and revocation distribution configuration.
///
/// All revocation fields (`revocation_cache_path`, `revocation_publisher`,
/// `revocation_public_key_hex`) must appear together so a partial deployment
/// cannot silently skip revocation enforcement. `revocation_url` is optional;
/// without it the cached list is still enforced but never refreshed over the
/// network (out-of-band distribution, e.g. configuration management, can
/// still update the cache file).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MarketplaceConfig {
    /// Path to the publisher trust store JSON (`PublisherTrustStoreFile`).
    /// When set, the file must parse and verify at validation time —
    /// a corrupt trust store fails closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_store_path: Option<PathBuf>,

    /// Local cache path for the signed revocation list. The cache is written
    /// atomically by the revocation store and verified on every load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocation_cache_path: Option<PathBuf>,

    /// HTTPS URL distributing signed revocation lists (localhost HTTP is
    /// allowed for test harnesses). Requires `revocation_cache_path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocation_url: Option<String>,

    /// Publisher name expected to sign revocation lists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocation_publisher: Option<String>,

    /// Hex-encoded ed25519 public key verifying revocation list signatures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocation_public_key_hex: Option<String>,
}

/// Maximum accepted size of a publisher trust store file.
const MAX_TRUST_STORE_BYTES: u64 = 256 * 1024;

impl MarketplaceConfig {
    /// Whether signed revocation list enforcement is configured.
    pub fn revocation_configured(&self) -> bool {
        self.revocation_cache_path.is_some()
    }

    /// Structural validation plus fail-closed loading of the trust store and
    /// revocation cache. `servers` are checked against the current revocation
    /// list so a revoked package cannot pass configuration loading.
    pub fn validate(&self, servers: &[McpServerConfig]) -> Result<(), ConfigError> {
        let invalid = |reason: String| ConfigError::InvalidConfig {
            reason: format!("marketplace: {reason}"),
        };
        let revocation_fields = [
            self.revocation_cache_path.is_some(),
            self.revocation_publisher.is_some(),
            self.revocation_public_key_hex.is_some(),
        ];
        if revocation_fields.iter().any(|present| *present)
            && revocation_fields.iter().any(|absent| !absent)
        {
            return Err(invalid(
                "revocation_cache_path, revocation_publisher, and revocation_public_key_hex must be set together"
                    .to_string(),
            ));
        }
        if self.revocation_url.is_some() && self.revocation_cache_path.is_none() {
            return Err(invalid(
                "revocation_url requires revocation_cache_path".to_string(),
            ));
        }
        if let Some(url) = &self.revocation_url
            && !url.to_lowercase().starts_with("https://")
            && !is_localhost_url(url)
        {
            return Err(invalid(format!("revocation_url '{url}' must use HTTPS")));
        }
        if let Some(path) = &self.trust_store_path {
            Self::load_trust_store(path)?;
        }
        if let Some(store) = self.revocation_store()? {
            for server in servers.iter().filter(|server| server.enabled) {
                let Some(provenance) = &server.provenance else {
                    continue;
                };
                store
                    .check_provenance(provenance)
                    .map_err(|error| invalid(format!("server '{}': {error}", server.name)))?;
            }
        }
        Ok(())
    }

    /// Load and verify the publisher trust store (bounded, fail-closed).
    fn load_trust_store(
        path: &Path,
    ) -> Result<crate::tools::mcp::marketplace::PublisherTrustStore, ConfigError> {
        let invalid = |reason: String| ConfigError::InvalidConfig {
            reason: format!("marketplace: trust store '{}': {reason}", path.display()),
        };
        let metadata =
            std::fs::metadata(path).map_err(|error| invalid(format!("cannot stat: {error}")))?;
        if metadata.len() > MAX_TRUST_STORE_BYTES {
            return Err(invalid(format!("exceeds {MAX_TRUST_STORE_BYTES} bytes")));
        }
        let bytes =
            std::fs::read(path).map_err(|error| invalid(format!("cannot read: {error}")))?;
        let file: crate::tools::mcp::marketplace::PublisherTrustStoreFile =
            serde_json::from_slice(&bytes)
                .map_err(|error| invalid(format!("invalid JSON: {error}")))?;
        crate::tools::mcp::marketplace::PublisherTrustStore::from_file(file)
            .map_err(|error| invalid(error.to_string()))
    }

    /// Open the configured revocation store, or `None` when revocation
    /// enforcement is not configured. A corrupt or invalid cache fails closed.
    pub fn revocation_store(
        &self,
    ) -> Result<Option<crate::tools::mcp::marketplace::RevocationStore>, ConfigError> {
        let (Some(cache_path), Some(publisher), Some(public_key_hex)) = (
            &self.revocation_cache_path,
            &self.revocation_publisher,
            &self.revocation_public_key_hex,
        ) else {
            return Ok(None);
        };
        crate::tools::mcp::marketplace::RevocationStore::open(
            cache_path.clone(),
            publisher,
            public_key_hex,
        )
        .map(Some)
        .map_err(|error| ConfigError::InvalidConfig {
            reason: format!("marketplace: revocation store: {error}"),
        })
    }
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
            self.validate_mandatory_sandbox(server)?;
        }
        self.external_runtime_registry()?;
        if let Some(marketplace) = &self.marketplace {
            marketplace.validate(&self.servers)?;
        }
        Ok(())
    }

    /// Build the external runtime registry declared by this file.
    ///
    /// Composition roots pass the registry to
    /// [`crate::tools::mcp::factory::create_client_from_config_with_external_runtimes`]
    /// so `transport = "external"` servers resolve their adapter. Validation
    /// runs the same builder, so invalid or duplicated descriptors fail
    /// configuration loading before any activation attempt.
    pub fn external_runtime_registry(
        &self,
    ) -> Result<crate::tools::mcp::descriptor::ExternalRuntimeRegistry, ConfigError> {
        let mut registry = crate::tools::mcp::descriptor::ExternalRuntimeRegistry::new();
        for descriptor in &self.external_runtimes {
            registry
                .register(descriptor.clone())
                .map_err(|error| ConfigError::InvalidConfig {
                    reason: format!("external runtimes: {error}"),
                })?;
        }
        Ok(registry)
    }

    /// Enforce the deployment-wide mandatory sandbox baseline when enabled.
    fn validate_mandatory_sandbox(&self, server: &McpServerConfig) -> Result<(), ConfigError> {
        if !self.require_sandbox_profiles || !server.enabled {
            return Ok(());
        }
        if server.placement != McpPlacement::Host
            || !matches!(
                server.effective_transport(),
                EffectiveTransport::Stdio { .. }
            )
        {
            return Ok(());
        }
        let invalid = |reason: String| ConfigError::InvalidConfig {
            reason: format!("Server '{name}': {reason}", name = server.name),
        };
        let Some(sandbox) = &server.sandbox else {
            return Err(invalid(
                "require_sandbox_profiles is set: host stdio servers must carry a sandbox policy (see McpSandboxConfig::default_profile)"
                    .to_string(),
            ));
        };
        let missing = sandbox.missing_mandatory_controls();
        if !missing.is_empty() {
            return Err(invalid(format!(
                "sandbox policy misses mandatory controls: {}",
                missing.join(", ")
            )));
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

    /// Get host servers eligible for daemon startup.
    pub fn host_auto_start_servers(&self) -> impl Iterator<Item = &McpServerConfig> {
        self.servers
            .iter()
            .filter(|server| server.should_host_auto_start())
    }

    /// Get job-worker servers eligible for job activation.
    pub fn job_worker_servers(&self) -> impl Iterator<Item = &McpServerConfig> {
        self.servers
            .iter()
            .filter(|server| server.should_run_in_job())
    }

    /// Get servers placed on persistent external workers.
    pub fn external_worker_servers(&self) -> impl Iterator<Item = &McpServerConfig> {
        self.servers
            .iter()
            .filter(|server| server.should_run_in_external_worker())
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

/// Open the configured revocation store and pull the newest signed list.
///
/// This is the revocation distribution entry point for daemon startup: when
/// the marketplace section configures a revocation URL, the store fetches and
/// applies any newer signed list before servers activate. Fetch failures are
/// logged and never clear or roll back the cached list. Returns the open
/// store when revocation enforcement is configured, so callers can re-check
/// servers against the freshest list.
pub async fn refresh_marketplace_revocations(
    servers: &McpServersFile,
) -> Option<std::sync::Arc<crate::tools::mcp::marketplace::RevocationStore>> {
    let marketplace = servers.marketplace.as_ref()?;
    let store = match marketplace.revocation_store() {
        Ok(Some(store)) => std::sync::Arc::new(store),
        Ok(None) => return None,
        Err(error) => {
            // validate() already fails closed on a corrupt cache, so reaching
            // this means the cache broke between load and refresh.
            tracing::warn!(%error, "Marketplace revocation store unavailable");
            return None;
        }
    };
    if let Some(url) = &marketplace.revocation_url {
        let client = reqwest::Client::new();
        match store.fetch_and_apply(url, &client).await {
            Ok(Some(sequence)) => {
                tracing::info!(sequence, "Applied newer marketplace revocation list");
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, url, "Marketplace revocation fetch failed; using cached list");
            }
        }
    }
    Some(store)
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
    Wasm {
        component_path: &'a Path,
        memory_mb: u64,
        fuel: u64,
        timeout_seconds: u64,
    },
    Gateway {
        mode: McpGatewayMode,
        route: Option<&'a str>,
    },
    External {
        runtime: &'a str,
        options: &'a serde_json::Value,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn sandbox_policy_only_applies_to_stdio() {
        let sandbox = crate::tools::mcp::sandbox::McpSandboxConfig {
            max_memory_mb: Some(256),
            isolate_process_group: true,
            ..Default::default()
        };

        let stdio = McpServerConfig::new_stdio("local", "cat", Vec::new(), HashMap::new())
            .with_sandbox(sandbox.clone());
        stdio.validate().expect("stdio server may carry a sandbox");

        let http =
            McpServerConfig::new("remote", "https://mcp.example.com").with_sandbox(sandbox.clone());
        let error = http
            .validate()
            .expect_err("http server must reject a sandbox");
        assert!(error.to_string().contains("stdio"));

        let unix = McpServerConfig::new_unix("unix", "/tmp/mcp.sock").with_sandbox(sandbox);
        assert!(unix.validate().is_err());
    }

    #[test]
    fn sandbox_policy_survives_serde_roundtrip() {
        let sandbox = crate::tools::mcp::sandbox::McpSandboxConfig {
            read_only_root: true,
            writable_paths: vec![PathBuf::from("/var/lib/mcp")],
            network_isolated: true,
            ..Default::default()
        };
        let config = McpServerConfig::new_stdio("local", "cat", Vec::new(), HashMap::new())
            .with_sandbox(sandbox);
        let json = serde_json::to_value(&config).expect("serialize");
        let parsed: McpServerConfig = serde_json::from_value(json).expect("deserialize");
        assert_eq!(parsed.sandbox, config.sandbox);
    }

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
        assert!(!config.should_host_auto_start());

        let mut servers = McpServersFile::default();
        servers.upsert(config);
        let server = servers.get_mut("blocked").unwrap();
        server.enabled = true;
        server.recovery_circuit_open = false;
        assert!(server.should_host_auto_start());
    }

    #[test]
    fn legacy_config_defaults_to_host_placement() {
        let config: McpServerConfig = serde_json::from_str(
            r#"{"name":"legacy","url":"https://mcp.example.com","enabled":true}"#,
        )
        .expect("deserialize legacy config");

        assert_eq!(config.placement, McpPlacement::Host);
        assert!(config.should_host_auto_start());
        assert!(!config.should_run_in_job());

        let invalid = r#"{
            "name":"invalid",
            "url":"https://mcp.example.com",
            "placement":"elsewhere"
        }"#;
        assert!(serde_json::from_str::<McpServerConfig>(invalid).is_err());
    }

    #[test]
    fn job_worker_placement_survives_serde_roundtrip() {
        let config = McpServerConfig::new_stdio("worker", "server", Vec::new(), HashMap::new())
            .with_placement(McpPlacement::JobWorker);
        let json = serde_json::to_string(&config).expect("serialize worker config");
        let parsed: McpServerConfig =
            serde_json::from_str(&json).expect("deserialize worker config");

        assert_eq!(parsed.placement, McpPlacement::JobWorker);
        assert!(parsed.should_run_in_job());
        assert!(!parsed.should_host_auto_start());
    }

    #[test]
    fn job_worker_validation_rejects_host_only_features() {
        let http = McpServerConfig::new("worker", "https://mcp.example.com")
            .with_placement(McpPlacement::JobWorker);
        assert!(http.validate().unwrap_err().to_string().contains("stdio"));

        let sandboxed = McpServerConfig::new_stdio("worker", "server", Vec::new(), HashMap::new())
            .with_sandbox(crate::tools::mcp::sandbox::McpSandboxConfig::default())
            .with_placement(McpPlacement::JobWorker);
        assert!(
            sandboxed
                .validate()
                .unwrap_err()
                .to_string()
                .contains("sandbox")
        );

        let oauth = McpServerConfig::new_stdio("worker", "server", Vec::new(), HashMap::new())
            .with_oauth(OAuthConfig::new("client"))
            .with_placement(McpPlacement::JobWorker);
        assert!(oauth.validate().unwrap_err().to_string().contains("OAuth"));

        let headers = McpServerConfig::new_stdio("worker", "server", Vec::new(), HashMap::new())
            .with_headers(HashMap::from([("X-Test".to_string(), "value".to_string())]))
            .with_placement(McpPlacement::JobWorker);
        assert!(
            headers
                .validate()
                .unwrap_err()
                .to_string()
                .contains("headers")
        );
    }

    #[test]
    fn mandatory_sandbox_profiles_enforced_for_host_stdio_servers() {
        use crate::tools::mcp::sandbox::McpSandboxConfig;

        let unsandboxed = McpServerConfig::new_stdio("local", "server", Vec::new(), HashMap::new());
        let file = McpServersFile {
            servers: vec![unsandboxed],
            require_sandbox_profiles: true,
            ..Default::default()
        };
        let error = file.validate().expect_err("unsandboxed stdio must fail");
        assert!(error.to_string().contains("require_sandbox_profiles"));

        // A sandbox missing mandatory controls also fails.
        let weak = McpServerConfig::new_stdio("local", "server", Vec::new(), HashMap::new())
            .with_sandbox(McpSandboxConfig::default());
        let file = McpServersFile {
            servers: vec![weak],
            require_sandbox_profiles: true,
            ..Default::default()
        };
        let error = file.validate().expect_err("weak sandbox must fail");
        assert!(error.to_string().contains("isolate_process_group"));

        // The documented baseline passes, and remote servers are unaffected.
        let hardened = McpServerConfig::new_stdio("local", "server", Vec::new(), HashMap::new())
            .with_sandbox(McpSandboxConfig::default_profile());
        let remote = McpServerConfig::new("remote", "https://mcp.example.com");
        let file = McpServersFile {
            servers: vec![hardened, remote],
            require_sandbox_profiles: true,
            ..Default::default()
        };
        file.validate().expect("hardened stdio and remote pass");

        // Disabled servers are not activated, so they are exempt.
        let mut disabled = McpServerConfig::new_stdio("off", "server", Vec::new(), HashMap::new());
        disabled.enabled = false;
        let file = McpServersFile {
            servers: vec![disabled],
            require_sandbox_profiles: true,
            ..Default::default()
        };
        file.validate().expect("disabled server exempt");

        // Opt-in default keeps legacy unsandboxed configs valid.
        let legacy = McpServersFile {
            servers: vec![McpServerConfig::new_stdio(
                "local",
                "server",
                Vec::new(),
                HashMap::new(),
            )],
            ..Default::default()
        };
        legacy.validate().expect("opt-in default preserves legacy");
    }

    #[test]
    fn require_sandbox_profiles_survives_serde_roundtrip() {
        let file = McpServersFile {
            require_sandbox_profiles: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&file).expect("serialize");
        let parsed: McpServersFile = serde_json::from_str(&json).expect("deserialize");
        assert!(parsed.require_sandbox_profiles);

        let default_file: McpServersFile = serde_json::from_str("{}").expect("empty file parses");
        assert!(!default_file.require_sandbox_profiles);
    }

    #[test]
    fn external_worker_placement_validates_and_persists() {
        // Valid persistent external-worker placement.
        let server = McpServerConfig::new_stdio("tools", "server", Vec::new(), HashMap::new())
            .with_external_worker("pebble-1");
        server.validate().expect("external-worker stdio validates");
        assert!(server.should_run_in_external_worker());
        assert!(!server.should_host_auto_start());
        assert!(!server.should_run_in_job());
        assert_eq!(server.external_worker.as_deref(), Some("pebble-1"));

        // Serde roundtrip keeps placement and worker name.
        let json = serde_json::to_string(&server).expect("serialize");
        let parsed: McpServerConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.placement, McpPlacement::ExternalWorker);
        assert_eq!(parsed.external_worker.as_deref(), Some("pebble-1"));

        // The file-level iterator surfaces persistent external-worker servers.
        let file = McpServersFile {
            servers: vec![parsed],
            ..Default::default()
        };
        assert_eq!(file.external_worker_servers().count(), 1);

        // Placement without a worker name fails.
        let missing = McpServerConfig::new_stdio("tools", "server", Vec::new(), HashMap::new())
            .with_placement(McpPlacement::ExternalWorker);
        assert!(
            missing
                .validate()
                .expect_err("worker name required")
                .to_string()
                .contains("external_worker")
        );

        // A worker name without the placement fails.
        let mut misplaced =
            McpServerConfig::new_stdio("tools", "server", Vec::new(), HashMap::new());
        misplaced.external_worker = Some("pebble-1".to_string());
        assert!(misplaced.validate().is_err());

        // Host-only features are rejected on external workers.
        let sandboxed = McpServerConfig::new_stdio("tools", "server", Vec::new(), HashMap::new())
            .with_sandbox(crate::tools::mcp::sandbox::McpSandboxConfig::default_profile())
            .with_external_worker("pebble-1");
        assert!(sandboxed.validate().is_err());
        let oauth = McpServerConfig::new("remote", "https://mcp.example.com")
            .with_oauth(OAuthConfig::new("client"))
            .with_external_worker("pebble-1");
        assert!(oauth.validate().is_err());
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

    #[test]
    fn wasm_transport_roundtrip_and_limits() {
        let config = McpServerConfig::new_wasm("portable", "/opt/mcp/server.wasm")
            .with_wasm_limits(128, 42_000_000, 15);
        config.validate().expect("valid WASM MCP config");

        let json = serde_json::to_string(&config).expect("serialize");
        let parsed: McpServerConfig = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(
            parsed.effective_transport(),
            EffectiveTransport::Wasm {
                component_path,
                memory_mb: 128,
                fuel: 42_000_000,
                timeout_seconds: 15,
            } if component_path == Path::new("/opt/mcp/server.wasm")
        ));
    }

    #[test]
    fn gateway_modes_enforce_routing_contract() {
        let dedicated = McpServerConfig::new_gateway(
            "dedicated",
            "https://gateway.example.com/mcp",
            McpGatewayMode::Dedicated,
            None,
        );
        dedicated.validate().expect("valid dedicated gateway");

        let shared = McpServerConfig::new_gateway(
            "shared",
            "https://gateway.example.com/mcp",
            McpGatewayMode::Shared,
            Some("tenant-tools".to_string()),
        );
        shared.validate().expect("valid shared gateway");
        assert!(shared.requires_auth());

        let missing_route = McpServerConfig::new_gateway(
            "bad",
            "https://gateway.example.com/mcp",
            McpGatewayMode::Shared,
            None,
        );
        assert!(missing_route.validate().is_err());

        let insecure = McpServerConfig::new_gateway(
            "bad",
            "http://gateway.example.com/mcp",
            McpGatewayMode::Dedicated,
            None,
        );
        assert!(insecure.validate().is_err());

        let whitespace_route = McpServerConfig::new_gateway(
            "bad",
            "https://gateway.example.com/mcp",
            McpGatewayMode::Shared,
            Some(" project-tools ".to_string()),
        );
        assert!(whitespace_route.validate().is_err());
    }

    fn signed_revocation_cache(
        dir: &Path,
        key: &crate::tools::mcp::marketplace::PublisherSigningKey,
        entries: Vec<crate::tools::mcp::marketplace::RevocationEntry>,
    ) -> PathBuf {
        let list = crate::tools::mcp::marketplace::RevocationList {
            schema_version: 1,
            sequence: 1,
            issued_at: chrono::Utc::now(),
            entries,
        };
        let signed =
            crate::tools::mcp::marketplace::SignedRevocationList::sign(list, "lunarwing", key);
        let cache = dir.join("revocations.json");
        std::fs::write(&cache, serde_json::to_vec(&signed).unwrap()).unwrap();
        cache
    }

    fn provenance_for_package(
        package_id: &str,
    ) -> crate::tools::mcp::provenance::McpPackageProvenance {
        let mut provenance = crate::tools::mcp::provenance::McpPackageProvenance::with_trust(
            crate::tools::mcp::provenance::PackageTrust::Unverified,
        );
        provenance.package_id = Some(package_id.to_string());
        provenance.version = Some("1.0.0".to_string());
        provenance
    }

    fn marketplace_file(
        cache: PathBuf,
        key: &crate::tools::mcp::marketplace::PublisherSigningKey,
        servers: Vec<McpServerConfig>,
    ) -> McpServersFile {
        let mut file = McpServersFile::default();
        file.marketplace = Some(MarketplaceConfig {
            trust_store_path: None,
            revocation_cache_path: Some(cache),
            revocation_url: None,
            revocation_publisher: Some("lunarwing".to_string()),
            revocation_public_key_hex: Some(key.public_key_hex()),
        });
        for server in servers {
            file.upsert(server);
        }
        file
    }

    #[test]
    fn marketplace_requires_complete_revocation_fields() {
        let mut file = McpServersFile::default();
        file.marketplace = Some(MarketplaceConfig {
            revocation_cache_path: Some(PathBuf::from("/tmp/cache.json")),
            ..Default::default()
        });
        let error = file.validate().expect_err("partial revocation config");
        assert!(error.to_string().contains("must be set together"));

        let mut file = McpServersFile::default();
        file.marketplace = Some(MarketplaceConfig {
            revocation_url: Some("https://marketplace.example.com/revocations.json".to_string()),
            ..Default::default()
        });
        let error = file
            .validate()
            .expect_err("revocation url without cache path");
        assert!(error.to_string().contains("revocation_cache_path"));
    }

    #[test]
    fn marketplace_rejects_non_https_revocation_url() {
        let key = crate::tools::mcp::marketplace::PublisherSigningKey::generate();
        let dir = tempdir().unwrap();
        let cache = signed_revocation_cache(dir.path(), &key, Vec::new());
        let mut file = marketplace_file(cache, &key, Vec::new());
        file.marketplace.as_mut().unwrap().revocation_url =
            Some("http://marketplace.example.com/revocations.json".to_string());
        let error = file.validate().expect_err("plain http url");
        assert!(error.to_string().contains("must use HTTPS"));
    }

    #[test]
    fn marketplace_revocation_blocks_revoked_enabled_server() {
        let key = crate::tools::mcp::marketplace::PublisherSigningKey::generate();
        let dir = tempdir().unwrap();
        let cache = signed_revocation_cache(
            dir.path(),
            &key,
            vec![crate::tools::mcp::marketplace::RevocationEntry {
                package_id: "evil-package".to_string(),
                version: None,
                sha256: None,
                reason: Some("compromised".to_string()),
            }],
        );

        let mut revoked = McpServerConfig::new("remote", "https://mcp.example.com");
        revoked.provenance = Some(provenance_for_package("evil-package"));
        let file = marketplace_file(cache.clone(), &key, vec![revoked]);
        let error = file.validate().expect_err("revoked package must fail");
        assert!(error.to_string().contains("remote"));
        assert!(error.to_string().contains("revoked"));

        // A disabled revoked server stays loadable so operators can inspect or
        // remove it; only activation-relevant (enabled) servers are blocked.
        let mut disabled = McpServerConfig::new("remote", "https://mcp.example.com");
        disabled.enabled = false;
        disabled.provenance = Some(provenance_for_package("evil-package"));
        let file = marketplace_file(cache.clone(), &key, vec![disabled]);
        file.validate().expect("disabled revoked server loads");

        let mut clean = McpServerConfig::new("remote", "https://mcp.example.com");
        clean.provenance = Some(provenance_for_package("good-package"));
        let file = marketplace_file(cache, &key, vec![clean]);
        file.validate().expect("unrevoked package loads");
    }

    #[test]
    fn marketplace_fails_closed_on_corrupt_revocation_cache() {
        let key = crate::tools::mcp::marketplace::PublisherSigningKey::generate();
        let dir = tempdir().unwrap();
        let cache = dir.path().join("revocations.json");
        std::fs::write(&cache, b"not json").unwrap();
        let file = marketplace_file(cache, &key, Vec::new());
        assert!(file.validate().is_err());
    }

    #[test]
    fn marketplace_fails_closed_on_unreadable_trust_store() {
        let dir = tempdir().unwrap();
        let mut file = McpServersFile::default();
        file.marketplace = Some(MarketplaceConfig {
            trust_store_path: Some(dir.path().join("missing.json")),
            ..Default::default()
        });
        assert!(file.validate().is_err());

        let trust_path = dir.path().join("trust.json");
        std::fs::write(&trust_path, b"{ not json").unwrap();
        file.marketplace = Some(MarketplaceConfig {
            trust_store_path: Some(trust_path),
            ..Default::default()
        });
        assert!(file.validate().is_err());
    }

    #[test]
    fn external_runtimes_register_and_reject_duplicates() {
        let descriptor = |name: &str| crate::tools::mcp::descriptor::ExternalRuntimeDescriptor {
            descriptor_version: crate::tools::mcp::descriptor::DESCRIPTOR_VERSION,
            name: name.to_string(),
            description: None,
            command: "/usr/local/bin/adapter".to_string(),
            args: Vec::new(),
            env: Default::default(),
            require_sandbox: false,
            capabilities: Vec::new(),
        };

        let mut file = McpServersFile::default();
        file.external_runtimes = vec![descriptor("node-compat")];
        file.validate().expect("valid descriptor");
        let registry = file.external_runtime_registry().expect("registry");
        assert_eq!(registry.names(), vec!["node-compat"]);

        file.external_runtimes.push(descriptor("node-compat"));
        let error = file.validate().expect_err("duplicate descriptor");
        assert!(error.to_string().contains("node-compat"));

        let mut file = McpServersFile::default();
        let mut invalid = descriptor("bad name");
        invalid.name = "bad name".to_string();
        file.external_runtimes = vec![invalid];
        assert!(file.validate().is_err());
    }

    #[tokio::test]
    async fn marketplace_and_external_runtimes_survive_serde_roundtrip() {
        let key = crate::tools::mcp::marketplace::PublisherSigningKey::generate();
        let dir = tempdir().unwrap();
        let cache = signed_revocation_cache(dir.path(), &key, Vec::new());
        let mut file = marketplace_file(cache.clone(), &key, Vec::new());
        file.marketplace.as_mut().unwrap().revocation_url =
            Some("https://marketplace.example.com/revocations.json".to_string());
        file.external_runtimes = vec![crate::tools::mcp::descriptor::ExternalRuntimeDescriptor {
            descriptor_version: crate::tools::mcp::descriptor::DESCRIPTOR_VERSION,
            name: "node-compat".to_string(),
            description: None,
            command: "/usr/local/bin/adapter".to_string(),
            args: Vec::new(),
            env: Default::default(),
            require_sandbox: false,
            capabilities: Vec::new(),
        }];

        let path = dir.path().join("mcp-servers.json");
        save_mcp_servers_to(&file, &path).await.unwrap();
        let loaded = load_mcp_servers_from(&path).await.unwrap();
        let marketplace = loaded.marketplace.as_ref().expect("marketplace section");
        assert_eq!(
            marketplace.revocation_cache_path.as_deref(),
            Some(cache.as_path())
        );
        assert!(loaded.external_runtime_registry().is_ok());
        assert_eq!(loaded.external_runtimes.len(), 1);
    }
}
