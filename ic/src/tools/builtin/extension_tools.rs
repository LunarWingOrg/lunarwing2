//! Agent-callable tools for managing extensions (MCP servers and WASM tools).
//!
//! These tools let the LLM manage extensions entirely through conversation.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::context::JobContext;
use crate::extensions::{ExtensionKind, ExtensionManager};
use crate::tools::mcp::McpServerConfig;
use crate::tools::tool::{ApprovalRequirement, Tool, ToolError, ToolOutput, require_str};

fn string_map_param(
    params: &serde_json::Value,
    key: &str,
    description: &str,
) -> Result<Option<HashMap<String, String>>, ToolError> {
    params
        .get(key)
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|error| {
            ToolError::InvalidParameters(format!(
                "{description} must be an object of string values: {error}"
            ))
        })
}

fn string_vec_param(
    params: &serde_json::Value,
    key: &str,
    description: &str,
) -> Result<Option<Vec<String>>, ToolError> {
    params
        .get(key)
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|error| {
            ToolError::InvalidParameters(format!(
                "{description} must be an array of strings: {error}"
            ))
        })
}

fn requests_secret_environment(params: &serde_json::Value) -> bool {
    params.get("env_secrets").is_some_and(|value| {
        value
            .as_object()
            .is_none_or(|mappings| !mappings.is_empty())
    })
}

// ── tool_search ──────────────────────────────────────────────────────────

pub struct ToolSearchTool {
    manager: Arc<ExtensionManager>,
}

impl ToolSearchTool {
    pub fn new(manager: Arc<ExtensionManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &str {
        "tool_search"
    }

    fn description(&self) -> &str {
        "Search for available extensions to add new capabilities. Extensions include \
         channels (XMPP — for messaging), tools, and MCP servers. \
         Use discover:true to search online if the built-in registry has no results."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query (name, keyword, or description fragment)"
                },
                "discover": {
                    "type": "boolean",
                    "description": "If true, also search online (slower, 5-15s). Try without first.",
                    "default": false
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();

        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let discover = params
            .get("discover")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let results = self
            .manager
            .search(query, discover)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let output = serde_json::json!({
            "results": results,
            "count": results.len(),
            "searched_online": discover,
        });

        Ok(ToolOutput::success(output, start.elapsed()))
    }
}

// ── tool_install ─────────────────────────────────────────────────────────

pub struct ToolInstallTool {
    manager: Arc<ExtensionManager>,
}

impl ToolInstallTool {
    pub fn new(manager: Arc<ExtensionManager>) -> Self {
        Self { manager }
    }

    fn mcp_config(
        params: &serde_json::Value,
        name: &str,
    ) -> Result<Option<McpServerConfig>, ToolError> {
        let kind = params.get("kind").and_then(|v| v.as_str());
        let transport = params.get("transport").and_then(|v| v.as_str());
        if kind != Some("mcp_server") && transport.is_none() {
            return Ok(None);
        }
        let config = match transport {
            Some("stdio") => {
                let command = params
                    .get("command")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidParameters(
                            "MCP server command is required for stdio transport".to_string(),
                        )
                    })?;
                let args = string_vec_param(params, "args", "MCP stdio args")?.unwrap_or_default();
                let env = string_map_param(params, "env", "MCP stdio env")?.unwrap_or_default();
                let env_secrets = string_map_param(
                    params,
                    "env_secrets",
                    "MCP stdio secret environment references",
                )?
                .unwrap_or_default();
                McpServerConfig::new_stdio_with_secrets(name, command, args, env, env_secrets)
            }
            Some("http") => {
                if params.get("url").and_then(|v| v.as_str()).is_none() {
                    return Err(ToolError::InvalidParameters(
                        "MCP server URL is required for HTTP transport".to_string(),
                    ));
                }
                return Ok(None);
            }
            Some(other) => {
                return Err(ToolError::InvalidParameters(format!(
                    "Unsupported MCP transport '{other}'"
                )));
            }
            None if params.get("command").is_some()
                || params.get("args").is_some()
                || params.get("env").is_some()
                || params.get("env_secrets").is_some() =>
            {
                return Err(ToolError::InvalidParameters(
                    "MCP transport must be 'stdio' when command, args, env, or env_secrets are provided"
                        .to_string(),
                ));
            }
            None => return Ok(None),
        };

        config
            .validate()
            .map_err(|e| ToolError::InvalidParameters(e.to_string()))?;
        Ok(Some(config))
    }
}

#[async_trait]
impl Tool for ToolInstallTool {
    fn name(&self) -> &str {
        "tool_install"
    }

    fn description(&self) -> &str {
        "Install an extension (channel, tool, or MCP server). \
         Use the name from tool_search results, provide an explicit URL, or configure \
         a host-local stdio MCP server with a command and structured arguments."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Extension name (from search results or custom)"
                },
                "url": {
                    "type": "string",
                    "description": "Explicit URL (for extensions not in the registry)"
                },
                "transport": {
                    "type": "string",
                    "enum": ["http", "stdio"],
                    "description": "MCP transport. Defaults to http when url is provided."
                },
                "command": {
                    "type": "string",
                    "description": "Executable command for a host-local stdio MCP server"
                },
                "args": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Structured command arguments for a stdio MCP server"
                },
                "env": {
                    "type": "object",
                    "additionalProperties": {"type": "string"},
                    "description": "Non-secret environment variables for a stdio MCP server"
                },
                "env_secrets": {
                    "type": "object",
                    "additionalProperties": {"type": "string"},
                    "description": "Environment variable names mapped to owner-scoped LunarWing secret names. Raw values are exposed to the unsandboxed stdio process."
                },
                "kind": {
                    "type": "string",
                    "enum": ["mcp_server", "wasm_tool", "wasm_channel"],
                    "description": "Extension type (auto-detected if omitted)"
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();

        let name = require_str(&params, "name")?;

        let url = params.get("url").and_then(|v| v.as_str());

        let kind_hint = params
            .get("kind")
            .and_then(|v| v.as_str())
            .and_then(|k| match k {
                "mcp_server" => Some(ExtensionKind::McpServer),
                "wasm_tool" => Some(ExtensionKind::WasmTool),
                "wasm_channel" => Some(ExtensionKind::WasmChannel),
                _ => None,
            })
            .or_else(|| {
                params
                    .get("transport")
                    .and_then(|v| v.as_str())
                    .map(|_| ExtensionKind::McpServer)
            });

        let result = match Self::mcp_config(&params, name)? {
            Some(config) => self.manager.install_mcp_config(config, &ctx.user_id).await,
            None => {
                self.manager
                    .install(name, url, kind_hint, &ctx.user_id)
                    .await
            }
        }
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let output = serde_json::to_value(&result)
            .unwrap_or_else(|_| serde_json::json!({"error": "serialization failed"}));

        Ok(ToolOutput::success(output, start.elapsed()))
    }

    fn requires_approval(&self, params: &serde_json::Value) -> ApprovalRequirement {
        if requests_secret_environment(params) {
            ApprovalRequirement::Always
        } else {
            ApprovalRequirement::UnlessAutoApproved
        }
    }
}

// ── tool_auth ────────────────────────────────────────────────────────────

pub struct ToolAuthTool {
    manager: Arc<ExtensionManager>,
}

impl ToolAuthTool {
    pub fn new(manager: Arc<ExtensionManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for ToolAuthTool {
    fn name(&self) -> &str {
        "tool_auth"
    }

    fn description(&self) -> &str {
        "Initiate authentication for an extension. For OAuth, returns a URL. \
         For manual auth, returns instructions. The user provides their token \
         through a secure channel, never through this tool."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Extension name to authenticate"
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();

        let name = require_str(&params, "name")?;

        let result = self
            .manager
            .auth(name, &ctx.user_id)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        // Auto-activate after successful auth so tools are available immediately
        if result.is_authenticated() {
            match self.manager.activate(name, &ctx.user_id).await {
                Ok(activate_result) => {
                    let output = serde_json::json!({
                        "status": "authenticated_and_activated",
                        "name": name,
                        "tools_loaded": activate_result.tools_loaded,
                        "message": activate_result.message,
                    });
                    return Ok(ToolOutput::success(output, start.elapsed()));
                }
                Err(e) => {
                    tracing::warn!(
                        "Extension '{}' authenticated but activation failed: {}",
                        name,
                        e
                    );
                    let output = serde_json::json!({
                        "status": "authenticated",
                        "name": name,
                        "activation_error": e.to_string(),
                        "message": format!(
                            "Authenticated but activation failed: {}. Try tool_activate.",
                            e
                        ),
                    });
                    return Ok(ToolOutput::success(output, start.elapsed()));
                }
            }
        }

        let output = serde_json::to_value(&result)
            .unwrap_or_else(|_| serde_json::json!({"error": "serialization failed"}));

        Ok(ToolOutput::success(output, start.elapsed()))
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        // In gateway mode, tool_auth only returns an auth URL for the frontend
        // to open — no browser is launched server-side, so no approval needed.
        if self.manager.should_use_gateway_mode() {
            ApprovalRequirement::Never
        } else {
            ApprovalRequirement::UnlessAutoApproved
        }
    }
}

// ── tool_activate ────────────────────────────────────────────────────────

pub struct ToolActivateTool {
    manager: Arc<ExtensionManager>,
}

impl ToolActivateTool {
    pub fn new(manager: Arc<ExtensionManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for ToolActivateTool {
    fn name(&self) -> &str {
        "tool_activate"
    }

    fn description(&self) -> &str {
        "Activate an installed extension — starts channels, loads tools, or connects to MCP servers."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Extension name to activate"
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();

        let name = require_str(&params, "name")?;

        match self.manager.activate(name, &ctx.user_id).await {
            Ok(result) => {
                let output = serde_json::to_value(&result)
                    .unwrap_or_else(|_| serde_json::json!({"error": "serialization failed"}));
                Ok(ToolOutput::success(output, start.elapsed()))
            }
            Err(activate_err) => {
                let err_str = activate_err.to_string();
                let err_lower = err_str.to_ascii_lowercase();
                let needs_auth = err_lower.contains("authentication")
                    || err_lower.contains("401")
                    || err_lower.contains("unauthorized")
                    || err_lower.contains("not authenticated");

                if !needs_auth {
                    return Err(ToolError::ExecutionFailed(err_str));
                }

                // Activation failed due to missing auth; initiate auth flow
                // so the agent loop can show the auth card.
                match self.manager.auth(name, &ctx.user_id).await {
                    Ok(auth_result) if auth_result.is_authenticated() => {
                        // Auth succeeded (e.g. env var was set); retry activation.
                        let result = self
                            .manager
                            .activate(name, &ctx.user_id)
                            .await
                            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                        let output = serde_json::to_value(&result).unwrap_or_else(
                            |_| serde_json::json!({"error": "serialization failed"}),
                        );
                        Ok(ToolOutput::success(output, start.elapsed()))
                    }
                    Ok(auth_result) => {
                        // Auth needs user input (awaiting_token). Return the auth
                        // result so detect_auth_awaiting picks it up.
                        let output = serde_json::to_value(&auth_result).unwrap_or_else(
                            |_| serde_json::json!({"error": "serialization failed"}),
                        );
                        Ok(ToolOutput::success(output, start.elapsed()))
                    }
                    Err(auth_err) => Err(ToolError::ExecutionFailed(format!(
                        "Activation failed ({}), and authentication also failed: {}",
                        err_str, auth_err
                    ))),
                }
            }
        }
    }
}

// ── tool_deactivate ──────────────────────────────────────────────────────

pub struct ToolDeactivateTool {
    manager: Arc<ExtensionManager>,
}

impl ToolDeactivateTool {
    pub fn new(manager: Arc<ExtensionManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for ToolDeactivateTool {
    fn name(&self) -> &str {
        "tool_deactivate"
    }

    fn description(&self) -> &str {
        "Deactivate an MCP server without uninstalling it. This unloads its tools and stops its host-local process while preserving configuration and credentials."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Installed MCP server name to deactivate"
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();
        let name = require_str(&params, "name")?;
        let message = self
            .manager
            .deactivate(name, &ctx.user_id)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        Ok(ToolOutput::success(
            serde_json::json!({"name": name, "message": message}),
            start.elapsed(),
        ))
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::UnlessAutoApproved
    }
}

// ── tool_update ──────────────────────────────────────────────────────────

pub struct ToolUpdateTool {
    manager: Arc<ExtensionManager>,
}

impl ToolUpdateTool {
    pub fn new(manager: Arc<ExtensionManager>) -> Self {
        Self { manager }
    }

    fn build_config(
        params: &serde_json::Value,
        existing: &McpServerConfig,
    ) -> Result<McpServerConfig, ToolError> {
        use crate::tools::mcp::config::EffectiveTransport;

        let transport = params.get("transport").and_then(|v| v.as_str());
        match transport {
            Some("stdio") => {
                let command = params
                    .get("command")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidParameters(
                            "MCP server command is required for stdio transport".to_string(),
                        )
                    })?;
                let args = string_vec_param(params, "args", "MCP stdio args")?.unwrap_or_default();
                let env = string_map_param(params, "env", "MCP stdio env")?.unwrap_or_default();
                let env_secrets = string_map_param(
                    params,
                    "env_secrets",
                    "MCP stdio secret environment references",
                )?
                .unwrap_or_default();
                Ok(McpServerConfig::new_stdio_with_secrets(
                    &existing.name,
                    command,
                    args,
                    env,
                    env_secrets,
                ))
            }
            Some("http") => {
                let url = params
                    .get("url")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&existing.url);
                Ok(McpServerConfig::new(&existing.name, url))
            }
            Some("wasm")
                if matches!(
                    existing.effective_transport(),
                    EffectiveTransport::Wasm { .. }
                ) =>
            {
                Ok(existing.clone())
            }
            Some("gateway")
                if matches!(
                    existing.effective_transport(),
                    EffectiveTransport::Gateway { .. }
                ) =>
            {
                let mut updated = existing.clone();
                if let Some(url) = params
                    .get("url")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.is_empty())
                {
                    updated.url = url.to_string();
                }
                Ok(updated)
            }
            Some(other) => Err(ToolError::InvalidParameters(format!(
                "Unsupported or incomplete MCP transport '{other}'"
            ))),
            None => match existing.effective_transport() {
                EffectiveTransport::Stdio {
                    command,
                    args,
                    env,
                    env_secrets,
                } => {
                    let cmd = params
                        .get("command")
                        .and_then(|v| v.as_str())
                        .unwrap_or(command);
                    let a = string_vec_param(params, "args", "MCP stdio args")?
                        .unwrap_or_else(|| args.to_vec());
                    let e = string_map_param(params, "env", "MCP stdio env")?
                        .unwrap_or_else(|| env.clone());
                    let secret_env = string_map_param(
                        params,
                        "env_secrets",
                        "MCP stdio secret environment references",
                    )?
                    .unwrap_or_else(|| env_secrets.clone());
                    Ok(McpServerConfig::new_stdio_with_secrets(
                        &existing.name,
                        cmd,
                        a,
                        e,
                        secret_env,
                    ))
                }
                EffectiveTransport::Http => {
                    let url = params
                        .get("url")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or(&existing.url);
                    Ok(McpServerConfig::new(&existing.name, url))
                }
                EffectiveTransport::Unix { socket_path } => {
                    Ok(McpServerConfig::new_unix(&existing.name, socket_path))
                }
                EffectiveTransport::Wasm { .. } => Ok(existing.clone()),
                EffectiveTransport::Gateway { .. } => {
                    let mut updated = existing.clone();
                    if let Some(url) = params
                        .get("url")
                        .and_then(|value| value.as_str())
                        .filter(|value| !value.is_empty())
                    {
                        updated.url = url.to_string();
                    }
                    Ok(updated)
                }
            },
        }
    }
}

#[async_trait]
impl Tool for ToolUpdateTool {
    fn name(&self) -> &str {
        "tool_update"
    }

    fn description(&self) -> &str {
        "Update an installed MCP server's configuration in-place (command, args, env, env_secrets, or url) \
         without removing and reinstalling it. If the server is active, it is restarted with \
         the new configuration. Enabled state and credentials are preserved."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Installed MCP server name to update"
                },
                "transport": {
                    "type": "string",
                    "enum": ["http", "stdio", "wasm", "gateway"],
                    "description": "New transport type. WASM and gateway values preserve their existing runtime-specific settings."
                },
                "url": {
                    "type": "string",
                    "description": "New URL (HTTP or gateway transport)"
                },
                "command": {
                    "type": "string",
                    "description": "New executable command (stdio transport)"
                },
                "args": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "New command arguments (stdio transport)"
                },
                "env": {
                    "type": "object",
                    "additionalProperties": {"type": "string"},
                    "description": "New non-secret environment variables (stdio transport)"
                },
                "env_secrets": {
                    "type": "object",
                    "additionalProperties": {"type": "string"},
                    "description": "New environment-variable-to-secret-name mappings (stdio transport)"
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();
        let name = require_str(&params, "name")?;

        let existing = self
            .manager
            .get_mcp_server_pub(name, &ctx.user_id)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let new_config = Self::build_config(&params, &existing)?;
        new_config
            .validate()
            .map_err(|e| ToolError::InvalidParameters(e.to_string()))?;

        let message = self
            .manager
            .update_mcp(name, new_config, &ctx.user_id)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        Ok(ToolOutput::success(
            serde_json::json!({"name": name, "message": message}),
            start.elapsed(),
        ))
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::Always
    }
}

// ── tool_list ────────────────────────────────────────────────────────────

pub struct ToolListTool {
    manager: Arc<ExtensionManager>,
}

impl ToolListTool {
    pub fn new(manager: Arc<ExtensionManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for ToolListTool {
    fn name(&self) -> &str {
        "tool_list"
    }

    fn description(&self) -> &str {
        "List extensions with their authentication and activation status. \
         Set include_available:true to also show registry entries not yet installed."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "kind": {
                    "type": "string",
                    "enum": ["mcp_server", "wasm_tool", "wasm_channel"],
                    "description": "Filter by extension type (omit to list all)"
                },
                "include_available": {
                    "type": "boolean",
                    "description": "If true, also include registry entries that are not yet installed",
                    "default": false
                }
            }
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();

        let kind_filter = params
            .get("kind")
            .and_then(|v| v.as_str())
            .and_then(|k| match k {
                "mcp_server" => Some(ExtensionKind::McpServer),
                "wasm_tool" => Some(ExtensionKind::WasmTool),
                "wasm_channel" => Some(ExtensionKind::WasmChannel),
                _ => None,
            });

        let include_available = params
            .get("include_available")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let extensions = self
            .manager
            .list(kind_filter, include_available, &ctx.user_id)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let output = serde_json::json!({
            "extensions": extensions,
            "count": extensions.len(),
        });

        Ok(ToolOutput::success(output, start.elapsed()))
    }
}

// ── tool_remove ──────────────────────────────────────────────────────────

pub struct ToolRemoveTool {
    manager: Arc<ExtensionManager>,
}

impl ToolRemoveTool {
    pub fn new(manager: Arc<ExtensionManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for ToolRemoveTool {
    fn name(&self) -> &str {
        "tool_remove"
    }

    fn description(&self) -> &str {
        "Permanently remove an installed extension (channel, tool, or MCP server) from disk. \
         This action cannot be undone — the WASM binary and configuration files will be deleted."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Extension name to remove"
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();

        let name = require_str(&params, "name")?;

        let message = self
            .manager
            .remove(name, &ctx.user_id)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let output = serde_json::json!({
            "name": name,
            "message": message,
        });

        Ok(ToolOutput::success(output, start.elapsed()))
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::Always
    }
}

// ── tool_upgrade ─────────────────────────────────────────────────────

pub struct ToolUpgradeTool {
    manager: Arc<ExtensionManager>,
}

impl ToolUpgradeTool {
    pub fn new(manager: Arc<ExtensionManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for ToolUpgradeTool {
    fn name(&self) -> &str {
        "tool_upgrade"
    }

    fn description(&self) -> &str {
        "Upgrade installed WASM extensions (channels and tools) to match the current \
         host WIT version. If name is omitted, checks and upgrades all installed WASM \
         extensions. Authentication and secrets are preserved."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Extension name to upgrade (omit to upgrade all)"
                }
            }
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();

        let name = params.get("name").and_then(|v| v.as_str());

        let result = self
            .manager
            .upgrade(name, &ctx.user_id)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let output = serde_json::to_value(&result)
            .unwrap_or_else(|_| serde_json::json!({"error": "serialization failed"}));

        Ok(ToolOutput::success(output, start.elapsed()))
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::UnlessAutoApproved
    }
}

// ── extension_info ────────────────────────────────────────────────────

pub struct ExtensionInfoTool {
    manager: Arc<ExtensionManager>,
}

impl ExtensionInfoTool {
    pub fn new(manager: Arc<ExtensionManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for ExtensionInfoTool {
    fn name(&self) -> &str {
        "extension_info"
    }

    fn description(&self) -> &str {
        "Show detailed information about an installed extension, including version \
         and WIT version compatibility."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Extension name to get info about"
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();

        let name = require_str(&params, "name")?;

        let info = self
            .manager
            .extension_info(name, &ctx.user_id)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        Ok(ToolOutput::success(info, start.elapsed()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_search_schema() {
        let tool = ToolSearchTool {
            manager: test_manager_stub(),
        };
        assert_eq!(tool.name(), "tool_search");
        let schema = tool.parameters_schema();
        assert!(schema.get("properties").is_some());
        assert!(schema["properties"].get("query").is_some());
    }

    #[test]
    fn test_tool_install_schema() {
        use crate::tools::tool::ApprovalRequirement;
        let tool = ToolInstallTool {
            manager: test_manager_stub(),
        };
        assert_eq!(tool.name(), "tool_install");
        assert_eq!(
            tool.requires_approval(&serde_json::json!({})),
            ApprovalRequirement::UnlessAutoApproved
        );
        let schema = tool.parameters_schema();
        assert!(schema["properties"].get("name").is_some());
        assert!(schema["properties"].get("url").is_some());
        assert!(schema["properties"].get("transport").is_some());
        assert!(schema["properties"].get("command").is_some());
        assert!(schema["properties"].get("args").is_some());
        assert!(schema["properties"].get("env").is_some());
        assert!(schema["properties"].get("env_secrets").is_some());
    }

    #[test]
    fn test_tool_install_builds_stdio_mcp_config() {
        use crate::tools::mcp::config::EffectiveTransport;

        let params = serde_json::json!({
            "name": "local-files",
            "kind": "mcp_server",
            "transport": "stdio",
            "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-filesystem", "/srv/data"],
            "env": {"LOG_LEVEL": "warn"},
            "env_secrets": {"API_TOKEN": "api_token"}
        });

        let config = ToolInstallTool::mcp_config(&params, "local-files")
            .expect("valid request")
            .expect("MCP config");
        match config.effective_transport() {
            EffectiveTransport::Stdio {
                command,
                args,
                env,
                env_secrets,
            } => {
                assert_eq!(command, "npx");
                assert_eq!(
                    args,
                    ["-y", "@modelcontextprotocol/server-filesystem", "/srv/data"]
                );
                assert_eq!(env.get("LOG_LEVEL").map(String::as_str), Some("warn"));
                assert_eq!(
                    env_secrets.get("API_TOKEN").map(String::as_str),
                    Some("api_token")
                );
            }
            other => panic!("expected stdio transport, got {other:?}"),
        }
    }

    #[test]
    fn test_tool_install_rejects_non_string_stdio_env_values() {
        let params = serde_json::json!({
            "name": "local-files",
            "kind": "mcp_server",
            "transport": "stdio",
            "command": "local-files-mcp",
            "env": {"PORT": 3000}
        });

        let err = ToolInstallTool::mcp_config(&params, "local-files")
            .expect_err("numeric env value must fail");
        assert!(err.to_string().contains("object of string values"));
    }

    #[test]
    fn tool_install_secret_environment_always_requires_approval() {
        use crate::tools::tool::ApprovalRequirement;

        let tool = ToolInstallTool {
            manager: test_manager_stub(),
        };
        let params = serde_json::json!({
            "name": "local-files",
            "kind": "mcp_server",
            "transport": "stdio",
            "command": "local-files-mcp",
            "env_secrets": {"API_TOKEN": "api_token"}
        });

        assert_eq!(tool.requires_approval(&params), ApprovalRequirement::Always);
    }

    #[test]
    fn test_tool_install_defers_registry_mcp_install() {
        let params = serde_json::json!({
            "name": "local-files",
            "kind": "mcp_server"
        });

        assert!(
            ToolInstallTool::mcp_config(&params, "local-files")
                .expect("valid registry request")
                .is_none()
        );
    }

    #[test]
    fn test_tool_install_defers_http_mcp_install() {
        let params = serde_json::json!({
            "name": "hosted-server",
            "kind": "mcp_server",
            "transport": "http",
            "url": "https://example.com/mcp"
        });

        assert!(
            ToolInstallTool::mcp_config(&params, "hosted-server")
                .expect("valid HTTP request")
                .is_none()
        );
    }

    #[test]
    fn test_tool_auth_schema() {
        use crate::tools::tool::ApprovalRequirement;
        let tool = ToolAuthTool {
            manager: test_manager_stub(),
        };
        assert_eq!(tool.name(), "tool_auth");
        assert_eq!(
            tool.requires_approval(&serde_json::json!({})),
            ApprovalRequirement::UnlessAutoApproved
        );
        let schema = tool.parameters_schema();
        assert!(schema["properties"].get("name").is_some());
        // token param must NOT be in schema (security: tokens never go through LLM)
        assert!(
            schema["properties"].get("token").is_none(),
            "tool_auth must not have a token parameter"
        );
    }

    #[test]
    fn test_tool_activate_schema() {
        use crate::tools::tool::ApprovalRequirement;
        let tool = ToolActivateTool {
            manager: test_manager_stub(),
        };
        assert_eq!(tool.name(), "tool_activate");
        assert_eq!(
            tool.requires_approval(&serde_json::json!({})),
            ApprovalRequirement::Never
        );
    }

    #[test]
    fn test_tool_deactivate_schema() {
        use crate::tools::tool::ApprovalRequirement;
        let tool = ToolDeactivateTool {
            manager: test_manager_stub(),
        };
        assert_eq!(tool.name(), "tool_deactivate");
        assert_eq!(
            tool.requires_approval(&serde_json::json!({})),
            ApprovalRequirement::UnlessAutoApproved
        );
        let schema = tool.parameters_schema();
        assert!(schema["properties"].get("name").is_some());
        assert_eq!(schema["required"], serde_json::json!(["name"]));
    }

    #[test]
    fn test_tool_update_schema() {
        use crate::tools::tool::ApprovalRequirement;
        let tool = ToolUpdateTool {
            manager: test_manager_stub(),
        };
        assert_eq!(tool.name(), "tool_update");
        assert_eq!(
            tool.requires_approval(&serde_json::json!({})),
            ApprovalRequirement::Always
        );
        let schema = tool.parameters_schema();
        assert!(schema["properties"].get("name").is_some());
        assert!(schema["properties"].get("transport").is_some());
        assert!(schema["properties"].get("command").is_some());
        assert!(schema["properties"].get("args").is_some());
        assert!(schema["properties"].get("env_secrets").is_some());
        assert_eq!(schema["required"], serde_json::json!(["name"]));
    }

    #[test]
    fn tool_update_preserves_unmentioned_secret_environment() {
        use crate::tools::mcp::config::EffectiveTransport;

        let existing = McpServerConfig::new_stdio_with_secrets(
            "local-files",
            "old-command",
            vec![],
            HashMap::new(),
            HashMap::from([("API_TOKEN".to_string(), "api_token".to_string())]),
        );
        let updated = ToolUpdateTool::build_config(
            &serde_json::json!({"name": "local-files", "command": "new-command"}),
            &existing,
        )
        .expect("build update");

        assert!(matches!(
            updated.effective_transport(),
            EffectiveTransport::Stdio {
                command,
                env_secrets,
                ..
            } if command == "new-command"
                && env_secrets.get("API_TOKEN").map(String::as_str) == Some("api_token")
        ));

        let tool = ToolUpdateTool {
            manager: test_manager_stub(),
        };
        assert_eq!(
            tool.requires_approval(&serde_json::json!({
                "name": "local-files",
                "env_secrets": {"API_TOKEN": "api_token"}
            })),
            ApprovalRequirement::Always
        );
    }

    #[test]
    fn test_tool_list_schema() {
        use crate::tools::tool::ApprovalRequirement;
        let tool = ToolListTool {
            manager: test_manager_stub(),
        };
        assert_eq!(tool.name(), "tool_list");
        assert_eq!(
            tool.requires_approval(&serde_json::json!({})),
            ApprovalRequirement::Never
        );
        let schema = tool.parameters_schema();
        assert!(schema["properties"].get("kind").is_some());
    }

    #[test]
    fn test_tool_remove_schema() {
        use crate::tools::tool::ApprovalRequirement;
        let tool = ToolRemoveTool {
            manager: test_manager_stub(),
        };
        assert_eq!(tool.name(), "tool_remove");
        assert_eq!(
            tool.requires_approval(&serde_json::json!({})),
            ApprovalRequirement::Always
        );
    }

    #[test]
    fn tool_remove_always_requires_approval_regardless_of_params() {
        use crate::tools::tool::ApprovalRequirement;
        let tool = ToolRemoveTool {
            manager: test_manager_stub(),
        };

        let test_cases = vec![
            ("no params", serde_json::json!({})),
            ("empty name", serde_json::json!({"name": ""})),
            ("gotify", serde_json::json!({"name": "gotify"})),
            ("github-cli", serde_json::json!({"name": "github-cli"})),
            (
                "with extra fields",
                serde_json::json!({"name": "tool", "extra": "field"}),
            ),
        ];

        for (case_name, params) in test_cases {
            assert_eq!(
                tool.requires_approval(&params),
                ApprovalRequirement::Always,
                "tool_remove must always require approval for case: {}",
                case_name
            );
        }
    }

    #[tokio::test]
    async fn tool_auth_no_approval_in_gateway_mode() {
        let manager = test_manager_stub();
        manager
            .enable_gateway_mode("http://localhost:3000".to_string())
            .await;
        let tool = ToolAuthTool {
            manager: manager.clone(),
        };
        assert_eq!(
            tool.requires_approval(&serde_json::json!({})),
            ApprovalRequirement::Never,
            "tool_auth should not require approval in gateway mode"
        );
    }

    #[test]
    fn test_tool_upgrade_schema() {
        use crate::tools::tool::ApprovalRequirement;
        let tool = ToolUpgradeTool {
            manager: test_manager_stub(),
        };
        assert_eq!(tool.name(), "tool_upgrade");
        assert_eq!(
            tool.requires_approval(&serde_json::json!({})),
            ApprovalRequirement::UnlessAutoApproved
        );
        let schema = tool.parameters_schema();
        // name is optional (omit to upgrade all)
        assert!(schema["properties"].get("name").is_some());
        assert!(
            schema.get("required").is_none(),
            "tool_upgrade should have no required params"
        );
    }

    #[test]
    fn test_extension_info_schema() {
        let tool = ExtensionInfoTool {
            manager: test_manager_stub(),
        };
        assert_eq!(tool.name(), "extension_info");
        let schema = tool.parameters_schema();
        assert!(schema["properties"].get("name").is_some());
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("name")));
    }

    /// Create a stub manager for schema tests (these don't call execute).
    fn test_manager_stub() -> Arc<ExtensionManager> {
        use crate::secrets::{InMemorySecretsStore, SecretsCrypto};
        use crate::testing::credentials::TEST_CRYPTO_KEY;
        use crate::tools::ToolRegistry;
        use crate::tools::mcp::session::McpSessionManager;

        let master_key = secrecy::SecretString::from(TEST_CRYPTO_KEY.to_string());
        let crypto = Arc::new(SecretsCrypto::new(master_key).unwrap());

        Arc::new(ExtensionManager::new(
            Arc::new(McpSessionManager::new()),
            Arc::new(crate::tools::mcp::process::McpProcessManager::new()),
            Arc::new(InMemorySecretsStore::new(crypto)),
            Arc::new(ToolRegistry::new()),
            None,
            None,
            std::env::temp_dir().join("lunarwing-test-tools"),
            std::env::temp_dir().join("lunarwing-test-channels"),
            None,
            "test".to_string(),
            None,
            Vec::new(),
        ))
    }
}
