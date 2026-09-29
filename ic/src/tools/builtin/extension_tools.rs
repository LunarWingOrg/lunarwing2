//! Agent-callable tools for managing extensions (MCP servers and WASM tools).
//!
//! These tools let the LLM manage extensions entirely through conversation.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::context::JobContext;
use crate::extensions::{ExtensionError, ExtensionKind, ExtensionManager};
use crate::tools::mcp::{McpGatewayMode, McpPlacement, McpServerConfig};
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

fn u64_param(
    params: &serde_json::Value,
    key: &str,
    description: &str,
) -> Result<Option<u64>, ToolError> {
    params
        .get(key)
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|error| {
            ToolError::InvalidParameters(format!(
                "{description} must be a non-negative integer: {error}"
            ))
        })
}

fn wasm_mcp_config(params: &serde_json::Value, name: &str) -> Result<McpServerConfig, ToolError> {
    let component = params
        .get("component_path")
        .and_then(|value| value.as_str())
        .ok_or_else(|| {
            ToolError::InvalidParameters(
                "MCP component_path is required for WASM transport".to_string(),
            )
        })?;
    Ok(McpServerConfig::new_wasm(name, component).with_wasm_limits(
        u64_param(params, "memory_mb", "MCP WASM memory_mb")?.unwrap_or(64),
        u64_param(params, "fuel", "MCP WASM fuel")?
            .unwrap_or(crate::tools::wasm::DEFAULT_FUEL_LIMIT),
        u64_param(params, "timeout_seconds", "MCP WASM timeout_seconds")?.unwrap_or(60),
    ))
}

fn gateway_mcp_config(
    params: &serde_json::Value,
    name: &str,
) -> Result<McpServerConfig, ToolError> {
    let endpoint = params
        .get("url")
        .and_then(|value| value.as_str())
        .ok_or_else(|| {
            ToolError::InvalidParameters(
                "MCP server URL is required for gateway transport".to_string(),
            )
        })?;
    let normalized_mode = params
        .get("gateway_mode")
        .and_then(|value| value.as_str())
        .unwrap_or("dedicated")
        .trim()
        .to_ascii_lowercase();
    let mode = match normalized_mode.as_str() {
        "dedicated" => McpGatewayMode::Dedicated,
        "shared" => McpGatewayMode::Shared,
        other => {
            return Err(ToolError::InvalidParameters(format!(
                "Unsupported MCP gateway mode '{other}'"
            )));
        }
    };
    let route = params
        .get("gateway_route")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|route| !route.is_empty())
        .map(str::to_string);
    Ok(McpServerConfig::new_gateway(name, endpoint, mode, route))
}

fn requests_secret_environment(params: &serde_json::Value) -> bool {
    params.get("env_secrets").is_some_and(|value| {
        value
            .as_object()
            .is_none_or(|mappings| !mappings.is_empty())
    })
}

fn requests_secret_headers(params: &serde_json::Value) -> bool {
    params.get("header_secrets").is_some_and(|value| {
        value
            .as_object()
            .is_none_or(|mappings| !mappings.is_empty())
    })
}

fn http_mcp_config(
    params: &serde_json::Value,
    name: &str,
) -> Result<Option<McpServerConfig>, ToolError> {
    let headers = string_map_param(params, "headers", "MCP HTTP headers")?;
    let header_secrets = string_map_param(
        params,
        "header_secrets",
        "MCP HTTP header secret references",
    )?;
    if headers.is_none() && header_secrets.is_none() {
        return Ok(None);
    }
    let url = params.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
        ToolError::InvalidParameters(
            "MCP server URL is required for HTTP transport with headers".to_string(),
        )
    })?;
    let mut config = McpServerConfig::new(name, url);
    if let Some(headers) = headers {
        config = config.with_headers(headers);
    }
    if let Some(header_secrets) = header_secrets {
        config = config.with_header_secrets(header_secrets);
    }
    Ok(Some(config))
}

fn placement_param(params: &serde_json::Value) -> Result<Option<McpPlacement>, ToolError> {
    let Some(value) = params.get("placement") else {
        return Ok(None);
    };
    let placement = value.as_str().ok_or_else(|| {
        ToolError::InvalidParameters("MCP placement must be a string".to_string())
    })?;
    match placement {
        "host" => Ok(Some(McpPlacement::Host)),
        "job_worker" | "job-worker" => Ok(Some(McpPlacement::JobWorker)),
        other => Err(ToolError::InvalidParameters(format!(
            "Unsupported MCP placement '{other}'"
        ))),
    }
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
        let placement = placement_param(params)?.unwrap_or_default();
        if kind != Some("mcp_server") && transport.is_none() {
            return if placement == McpPlacement::JobWorker {
                Err(ToolError::InvalidParameters(
                    "Job-worker placement requires an explicit stdio MCP server".to_string(),
                ))
            } else {
                Ok(None)
            };
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
                if placement == McpPlacement::JobWorker {
                    return Err(ToolError::InvalidParameters(
                        "Job-worker MCP servers only support stdio transport".to_string(),
                    ));
                }
                return http_mcp_config(params, name);
            }
            Some("wasm") => wasm_mcp_config(params, name)?,
            Some("gateway") => gateway_mcp_config(params, name)?,
            Some(other) => {
                return Err(ToolError::InvalidParameters(format!(
                    "Unsupported MCP transport '{other}'"
                )));
            }
            None if params.get("url").is_some()
                && (params.get("headers").is_some() || params.get("header_secrets").is_some()) =>
            {
                // Headers default the transport to http when a url is present.
                if placement == McpPlacement::JobWorker {
                    return Err(ToolError::InvalidParameters(
                        "Job-worker MCP servers only support stdio transport".to_string(),
                    ));
                }
                match http_mcp_config(params, name)? {
                    Some(config) => return Ok(Some(config)),
                    None => {
                        return Err(ToolError::InvalidParameters(
                            "MCP transport is required when headers are provided".to_string(),
                        ));
                    }
                }
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
            None => {
                if placement == McpPlacement::JobWorker {
                    return Err(ToolError::InvalidParameters(
                        "Job-worker MCP installation requires explicit stdio transport".to_string(),
                    ));
                }
                return Ok(None);
            }
        };

        let config = config.with_placement(placement);
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
         a host-local stdio/WASM MCP server, dedicated/shared MCP gateway, or job-worker \
         stdio server whose command already exists in worker image."
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
                    "enum": ["http", "stdio", "wasm", "gateway"],
                    "description": "MCP transport. Defaults to http when url is provided."
                },
                "placement": {
                    "type": "string",
                    "enum": ["host", "job_worker"],
                    "description": "MCP runtime placement. host starts a daemon-host process; job_worker makes a stdio server eligible inside future jobs. Defaults to host."
                },
                "command": {
                    "type": "string",
                    "description": "Executable command for a stdio MCP server. For job_worker placement, command must exist in worker image."
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
                    "description": "Environment variable names mapped to owner-scoped LunarWing secret names. Raw values are exposed only to selected host or job-local stdio MCP process."
                },
                "headers": {
                    "type": "object",
                    "additionalProperties": {"type": "string"},
                    "description": "Custom HTTP headers for an http-transport MCP server. Values are stored as provided; prefer header_secrets for credentials."
                },
                "header_secrets": {
                    "type": "object",
                    "additionalProperties": {"type": "string"},
                    "description": "HTTP header names mapped to owner-scoped LunarWing secret names (http transport). The secret value replaces the header value at activation; for bearer auth store the full value including the scheme prefix."
                },
                "component_path": {
                    "type": "string",
                    "description": "Absolute component path for MCP-over-WASM"
                },
                "memory_mb": {"type": "integer", "minimum": 1, "maximum": 1024},
                "fuel": {"type": "integer", "minimum": 1},
                "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 300},
                "gateway_mode": {
                    "type": "string",
                    "enum": ["dedicated", "shared"]
                },
                "gateway_route": {
                    "type": "string",
                    "description": "Required routing key for shared gateways"
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
        if requests_secret_environment(params) || requests_secret_headers(params) {
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
        "Activate an installed extension. Host MCP placement starts a host process; job_worker placement enables eligibility for future jobs without changing running job snapshots."
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
                let needs_auth = matches!(activate_err, ExtensionError::AuthRequired);

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
        "Deactivate an MCP server without uninstalling it. Host placement unloads tools and stops its process; job_worker placement removes future-job eligibility. Configuration and credentials remain."
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
        let mut config = match transport {
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
                EffectiveTransport::External { .. } => Ok(existing.clone()),
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
        }?;
        config.placement = placement_param(params)?.unwrap_or(existing.placement);
        config
            .validate()
            .map_err(|error| ToolError::InvalidParameters(error.to_string()))?;
        Ok(config)
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
         the new configuration. Placement defaults to existing placement; explicit host/job_worker \
         switches runtime location. Worker updates affect future jobs, not running snapshots. Enabled state and credentials are preserved."
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
                "placement": {
                    "type": "string",
                    "enum": ["host", "job_worker"],
                    "description": "New MCP runtime placement. Omit to preserve existing placement. job_worker requires stdio and command availability in worker image."
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

/// Default and maximum number of extension entries per `tool_list` page.
const TOOL_LIST_DEFAULT_PAGE: usize = 50;
const TOOL_LIST_MAX_PAGE: usize = 100;

/// Default and maximum number of per-extension tool names per `tool_list`
/// entry. A server exposing hundreds of tools would otherwise consume the
/// whole page budget on one entry; `tools_offset`/`tools_limit` page through
/// the rest.
const TOOL_LIST_TOOLS_PAGE_DEFAULT: usize = 25;
const TOOL_LIST_TOOLS_PAGE_MAX: usize = 100;

/// Serialized-byte budget for a single `tool_list` page, covering the
/// pretty-printed JSON the safety layer would otherwise clip mid-structure.
/// The default safety cap is 100_000 bytes; this leaves ample headroom for
/// wrapper text and truncation notices.
const TOOL_LIST_PAGE_BUDGET_BYTES: usize = 60_000;

/// Serialized-byte budget for a single entry. Tool-name windows are sized so
/// one entry can never exceed this even with worst-case JSON escaping
/// (control characters serialize to six bytes each), so a one-entry page
/// always fits the page budget without clipping identifiers.
const TOOL_LIST_ENTRY_BUDGET_BYTES: usize = 32_000;

/// Worst-case JSON serialization expansion of a string: every char may become
/// a six-byte `\u00XX` escape (control characters), plus the surrounding
/// quotes.
const TOOL_LIST_JSON_ESCAPE_FACTOR: usize = 6;

/// Maximum characters embedded for any single descriptive string field
/// (description, display name, activation error) in an entry. Longer values
/// are elided with an explicit marker; the full text stays available through
/// the owning subsystem. Identifiers (`name`, `tools[]`) are never elided —
/// see `TOOL_LIST_ENTRY_BUDGET_BYTES`.
const TOOL_LIST_FIELD_MAX_CHARS: usize = 200;

/// Elision marker appended when a string field is clipped.
const TOOL_LIST_ELLIPSIS: &str = "…";

/// Bound a descriptive string field, eliding past `TOOL_LIST_FIELD_MAX_CHARS`
/// with an explicit marker so consumers can tell a clipped value from a full
/// one. Identifiers must not pass through here — eliding them would make two
/// long names indistinguishable with no way back (MR !1221 finding 2).
fn bound_field(value: &str) -> String {
    if value.chars().count() <= TOOL_LIST_FIELD_MAX_CHARS {
        return value.to_string();
    }
    let keep = TOOL_LIST_FIELD_MAX_CHARS.saturating_sub(TOOL_LIST_ELLIPSIS.len());
    let clipped: String = value.chars().take(keep).collect();
    format!("{clipped}{TOOL_LIST_ELLIPSIS}")
}

/// Serialized byte length of a string as it appears in pretty JSON, assuming
/// worst-case per-char escaping. Cheap upper bound used to size tool-name
/// windows without serializing per candidate.
fn json_string_upper_bound(value: &str) -> usize {
    2 + value.chars().count() * TOOL_LIST_JSON_ESCAPE_FACTOR
}

/// Upper bound on the serialized byte length of a tool-name window entry
/// (pretty-printed array element = quoted string + comma/indent).
fn tool_name_upper_bound(name: &str) -> usize {
    json_string_upper_bound(name) + 8
}

/// One page of per-extension tool names plus completeness metadata.
///
/// Names are embedded verbatim (never elided) so long identifiers stay
/// distinguishable; if they do not all fit the entry byte budget, the window
/// shrinks and `truncated`/`next_offset` point at the remainder (MR !1221
/// finding 2).
struct ToolNameWindow {
    names: Vec<String>,
    total: usize,
    start: usize,
    truncated: bool,
    next_offset: Option<usize>,
}

/// Fixed serialized cost of an entry's kind/status/config fields (including
/// the restored `enabled`/`placement`/`transport`/`url`/`command` from MR
/// !1225 finding 2) plus the per-window metadata keys, with generous slack.
/// Reserved before tool names so the window sizing cannot be starved by
/// descriptive fields; the extension's own name is added on top since it is
/// embedded verbatim.
const TOOL_LIST_ENTRY_FIXED_BUDGET_BYTES: usize = TOOL_LIST_ENTRY_BUDGET_BYTES - 20_000;

/// Apply a `tools_offset`/`tools_limit` window over one extension's tool
/// names so entries with more tools than the window cap remain fully
/// enumerable (review finding 1 on WI#245), keeping every name lossless and
/// bounded by the per-entry serialized budget (MR !1221 findings 1-2).
fn tool_name_window(
    tools: &[String],
    extension_name: &str,
    offset: usize,
    limit: usize,
) -> ToolNameWindow {
    let total = tools.len();
    let start = offset.min(total);
    let end = (start + limit).min(total);

    let mut names_end = end;
    let mut used = TOOL_LIST_ENTRY_FIXED_BUDGET_BYTES + json_string_upper_bound(extension_name);
    for (idx, name) in tools[start..end].iter().enumerate() {
        let cost = tool_name_upper_bound(name);
        if idx > 0 && used + cost > TOOL_LIST_ENTRY_BUDGET_BYTES {
            names_end = start + idx;
            break;
        }
        used += cost;
    }

    ToolNameWindow {
        names: tools[start..names_end].to_vec(),
        total,
        start,
        truncated: names_end < total,
        next_offset: (names_end < total).then_some(names_end),
    }
}

/// Parse an optional non-negative integer parameter with an inclusive upper
/// clamp. `None`/`null` yields `default`.
fn parse_bounded_param(
    params: &serde_json::Value,
    key: &str,
    default: usize,
    max: usize,
) -> Result<usize, ToolError> {
    let Some(raw) = params.get(key) else {
        return Ok(default);
    };
    if raw.is_null() {
        return Ok(default);
    }
    raw.as_u64()
        .map(|v| (v as usize).clamp(0, max))
        .ok_or_else(|| {
            ToolError::InvalidParameters(format!("{key} must be a non-negative integer"))
        })
}

/// Parse a required positive integer parameter with an inclusive upper clamp.
fn parse_positive_param(
    params: &serde_json::Value,
    key: &str,
    default: usize,
    max: usize,
) -> Result<usize, ToolError> {
    let Some(raw) = params.get(key) else {
        return Ok(default);
    };
    if raw.is_null() {
        return Ok(default);
    }
    let value = raw
        .as_u64()
        .ok_or_else(|| ToolError::InvalidParameters(format!("{key} must be a positive integer")))?;
    if value == 0 {
        return Err(ToolError::InvalidParameters(format!(
            "{key} must be a positive integer"
        )));
    }
    Ok((value as usize).min(max))
}

/// Serialize an entry with verbatim identifiers, elided descriptive fields,
/// and a windowed tool-name list that keeps every name lossless.
fn extension_to_json(
    extension: &crate::extensions::InstalledExtension,
    tools_offset: usize,
    tools_limit: usize,
) -> serde_json::Value {
    let window = tool_name_window(&extension.tools, &extension.name, tools_offset, tools_limit);

    let mut obj = serde_json::Map::new();
    // Identifiers stay verbatim: eliding `name` would make long names
    // unrecoverable through this surface (MR !1221 finding 2). The per-entry
    // byte budget is enforced by the tool-name window, which shrinks instead
    // of clipping.
    obj.insert("name".to_string(), serde_json::json!(extension.name));
    obj.insert("kind".to_string(), serde_json::json!(extension.kind));
    obj.insert(
        "authenticated".to_string(),
        serde_json::json!(extension.authenticated),
    );
    obj.insert("active".to_string(), serde_json::json!(extension.active));
    obj.insert(
        "installed".to_string(),
        serde_json::json!(extension.installed),
    );
    obj.insert("tools".to_string(), serde_json::json!(window.names));
    if window.total > 0 {
        obj.insert("tools_total".to_string(), serde_json::json!(window.total));
        if window.start > 0 {
            obj.insert("tools_offset".to_string(), serde_json::json!(window.start));
        }
    }
    if window.truncated {
        obj.insert(
            "tools_truncated".to_string(),
            serde_json::json!(window.truncated),
        );
        obj.insert(
            "tools_next_offset".to_string(),
            serde_json::json!(window.next_offset),
        );
    }

    // Status/config fields restored from the pre-pagination serialization
    // (MR !1225 finding 2): without `enabled`, a deliberately disabled MCP
    // server is indistinguishable from an inactive/failing one; without
    // `placement`, host vs job-worker placement cannot be diagnosed from
    // this surface. `transport`, `url`, and `command` carry the same
    // configuration the old full-struct output exposed. Free-text values
    // are bounded; enums and booleans are short by construction.
    if let Some(enabled) = extension.enabled {
        obj.insert("enabled".to_string(), serde_json::json!(enabled));
    }
    if let Some(placement) = extension.placement {
        obj.insert("placement".to_string(), serde_json::json!(placement));
    }
    if let Some(transport) = &extension.transport {
        obj.insert(
            "transport".to_string(),
            serde_json::json!(bound_field(transport)),
        );
    }
    if let Some(url) = &extension.url {
        obj.insert("url".to_string(), serde_json::json!(bound_field(url)));
    }
    if let Some(command) = &extension.command {
        obj.insert(
            "command".to_string(),
            serde_json::json!(bound_field(command)),
        );
    }

    // Optional fields ride along only when present, bounded in length.
    if let Some(display_name) = &extension.display_name {
        obj.insert(
            "display_name".to_string(),
            serde_json::json!(bound_field(display_name)),
        );
    }
    if let Some(description) = &extension.description {
        obj.insert(
            "description".to_string(),
            serde_json::json!(bound_field(description)),
        );
    }
    if let Some(version) = &extension.version {
        obj.insert(
            "version".to_string(),
            serde_json::json!(bound_field(version)),
        );
    }
    if let Some(activation_error) = &extension.activation_error {
        obj.insert(
            "activation_error".to_string(),
            serde_json::json!(bound_field(activation_error)),
        );
    }
    if extension.needs_setup || extension.has_auth {
        obj.insert(
            "needs_setup".to_string(),
            serde_json::json!(extension.needs_setup),
        );
        obj.insert(
            "has_auth".to_string(),
            serde_json::json!(extension.has_auth),
        );
    }
    if extension.recovering {
        obj.insert("recovering".to_string(), serde_json::json!(true));
    }

    serde_json::Value::Object(obj)
}

/// Byte length a page would occupy once pretty-printed — the exact string
/// the safety layer clips at `max_output_length` — including pagination
/// metadata for `offset`/`total`. Used to enforce the serialized budget
/// before returning (review finding 2 on WI#245).
fn page_json_len(page: &[serde_json::Value], offset: usize, total: usize) -> Option<usize> {
    let next_offset = (offset + page.len() < total).then_some(offset + page.len());
    let combined = serde_json::json!({
        "extensions": page,
        "count": page.len(),
        "total": total,
        "offset": offset,
        "next_offset": next_offset,
        "truncated": next_offset.is_some(),
    });
    serde_json::to_string_pretty(&combined)
        .ok()
        .map(|s| s.len())
}

/// Drop trailing entries until the serialized page fits the byte budget.
/// Metadata is derived after shrinking, so `count`/`next_offset`/`truncated`
/// always describe the entries actually returned. The check also applies to
/// the final single entry: a lone oversized entry is dropped rather than
/// returned clipped-while-claiming-completeness (MR !1221 finding 1).
///
/// Returns `FittedPage::Empty` when no entry survives the budget **and the
/// request asked for entries** — only that case is a hard error for the
/// caller. A request that legitimately matched zero entries (empty
/// inventory, or a `kind` filter with no matches) is reported via
/// `FittedPage::NoEntries` so it can return the same
/// `{"extensions":[],"count":0}` shape callers got before pagination
/// (MR !1225 finding 1).
enum FittedPage {
    Entries(Vec<serde_json::Value>),
    NoEntries,
    Empty,
}

impl std::fmt::Debug for FittedPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Entries(page) => f.debug_tuple("Entries").field(&page.len()).finish(),
            Self::NoEntries => f.write_str("NoEntries"),
            Self::Empty => f.write_str("Empty"),
        }
    }
}

fn fit_page_to_budget(mut page: Vec<serde_json::Value>, offset: usize, total: usize) -> FittedPage {
    if page.is_empty() {
        // No entries were requested or matched: a complete empty page, not a
        // budget failure.
        return FittedPage::NoEntries;
    }
    while page_json_len(&page, offset, total).is_none_or(|len| len > TOOL_LIST_PAGE_BUDGET_BYTES) {
        page.pop();
        if page.is_empty() {
            // At least one entry existed and even a single one could not fit:
            // an upstream value defeated the per-entry bound. Signal the
            // caller to fail loudly rather than claim completeness.
            return FittedPage::Empty;
        }
    }
    FittedPage::Entries(page)
}

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
         Set include_available:true to also show registry entries not yet installed. \
         Output is paginated: when truncated is true, call again with the returned \
         next_offset as offset (same kind/include_available) to continue. \
         Per-extension tool names are windowed by tools_offset/tools_limit; \
         tools_truncated/tools_next_offset on an entry mark servers whose remaining \
         tool names need a follow-up call with that entry's name."
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
                },
                "offset": {
                    "type": "integer",
                    "description": "Index of the first entry to return. Use next_offset from a previous truncated response to continue.",
                    "default": 0,
                    "minimum": 0
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum entries per page (1-100, default 50).",
                    "default": 50,
                    "minimum": 1,
                    "maximum": 100
                },
                "tools_offset": {
                    "type": "integer",
                    "description": "Index of the first per-entry tool name to return within each entry's tools array. Use tools_next_offset from a truncated entry to continue.",
                    "default": 0,
                    "minimum": 0
                },
                "tools_limit": {
                    "type": "integer",
                    "description": "Maximum per-entry tool names per page (1-100, default 25).",
                    "default": 25,
                    "minimum": 1,
                    "maximum": 100
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

        let offset = parse_bounded_param(&params, "offset", 0, usize::MAX)?;
        let limit =
            parse_positive_param(&params, "limit", TOOL_LIST_DEFAULT_PAGE, TOOL_LIST_MAX_PAGE)?;
        let tools_offset = parse_bounded_param(&params, "tools_offset", 0, usize::MAX)?;
        let tools_limit = parse_positive_param(
            &params,
            "tools_limit",
            TOOL_LIST_TOOLS_PAGE_DEFAULT,
            TOOL_LIST_TOOLS_PAGE_MAX,
        )?;

        let extensions = self
            .manager
            .list(kind_filter, include_available, &ctx.user_id)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let total = extensions.len();
        if offset > total {
            return Err(ToolError::InvalidParameters(format!(
                "offset {offset} exceeds total entry count {total}"
            )));
        }

        let page: Vec<serde_json::Value> = extensions
            .iter()
            .skip(offset)
            .take(limit)
            .map(|e| extension_to_json(e, tools_offset, tools_limit))
            .collect();

        // Enforce the serialized-byte budget: if the page would exceed it,
        // drop entries from the tail until it fits. A single oversized entry
        // (long upstream description + 100 tool names) must never produce a
        // page the safety layer would clip mid-JSON while its own
        // truncated/next_offset fields claim completeness.
        let page = match fit_page_to_budget(page, offset, total) {
            FittedPage::Entries(page) => page,
            FittedPage::NoEntries => {
                // Empty inventory or a kind filter with no matches: a
                // complete empty page, matching the pre-pagination shape
                // (MR !1225 finding 1).
                let output = serde_json::json!({
                    "extensions": [],
                    "count": 0,
                    "total": total,
                    "offset": offset,
                    "next_offset": null,
                    "truncated": false,
                });
                return Ok(ToolOutput::success(output, start.elapsed()));
            }
            FittedPage::Empty => {
                // The per-entry tool-name window bounds every entry below the
                // page budget, so this is unreachable for well-formed data; if
                // an upstream value ever defeats it, fail loudly rather than
                // return an empty page whose metadata would claim completeness.
                return Err(ToolError::ExecutionFailed(format!(
                    "entry at offset {offset} exceeds the serialized page budget; \
                     retry with a smaller tools_limit or a higher tools_offset"
                )));
            }
        };

        let returned = page.len();
        let next_offset = (offset + returned < total).then_some(offset + returned);

        let output = serde_json::json!({
            "extensions": page,
            "count": returned,
            "total": total,
            "offset": offset,
            "next_offset": next_offset,
            "truncated": next_offset.is_some(),
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
        assert_eq!(
            schema["properties"]["placement"]["enum"],
            serde_json::json!(["host", "job_worker"])
        );
        assert!(schema["properties"].get("command").is_some());
        assert!(schema["properties"].get("args").is_some());
        assert!(schema["properties"].get("env").is_some());
        assert!(schema["properties"].get("env_secrets").is_some());
        assert!(schema["properties"].get("component_path").is_some());
        assert!(schema["properties"].get("gateway_mode").is_some());
        assert!(schema["properties"].get("gateway_route").is_some());
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
        assert_eq!(config.placement, McpPlacement::Host);
    }

    #[test]
    fn test_tool_install_builds_job_worker_stdio_config() {
        let params = serde_json::json!({
            "name": "worker-files",
            "kind": "mcp_server",
            "transport": "stdio",
            "command": "worker-files-mcp",
            "placement": "job_worker"
        });
        let config = ToolInstallTool::mcp_config(&params, "worker-files")
            .expect("valid worker config")
            .expect("explicit MCP config");

        assert_eq!(config.placement, McpPlacement::JobWorker);
    }

    #[test]
    fn test_tool_install_rejects_job_worker_non_stdio_paths() {
        for params in [
            serde_json::json!({
                "name": "remote",
                "kind": "mcp_server",
                "transport": "http",
                "url": "https://example.com/mcp",
                "placement": "job_worker"
            }),
            serde_json::json!({
                "name": "catalog",
                "kind": "mcp_server",
                "placement": "job_worker"
            }),
        ] {
            let error = ToolInstallTool::mcp_config(&params, "worker")
                .expect_err("worker non-stdio must fail");
            assert!(error.to_string().contains("Job-worker MCP"));
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
    fn test_tool_install_builds_wasm_mcp_config() {
        use crate::tools::mcp::config::EffectiveTransport;

        let config = ToolInstallTool::mcp_config(
            &serde_json::json!({
                "kind": "mcp_server",
                "transport": "wasm",
                "component_path": "/opt/lunarwing/tools.wasm",
                "memory_mb": 128,
                "fuel": 12345,
                "timeout_seconds": 15
            }),
            "portable",
        )
        .expect("valid request")
        .expect("MCP config");
        assert!(matches!(
            config.effective_transport(),
            EffectiveTransport::Wasm {
                memory_mb: 128,
                fuel: 12345,
                timeout_seconds: 15,
                ..
            }
        ));
    }

    #[test]
    fn test_tool_install_rejects_non_numeric_wasm_limits() {
        let error = ToolInstallTool::mcp_config(
            &serde_json::json!({
                "kind": "mcp_server",
                "transport": "wasm",
                "component_path": "/opt/lunarwing/tools.wasm",
                "memory_mb": "128"
            }),
            "portable",
        )
        .expect_err("string memory limit must fail");
        assert!(error.to_string().contains("non-negative integer"));
    }

    #[test]
    fn test_tool_install_builds_shared_gateway_config() {
        use crate::tools::mcp::config::EffectiveTransport;

        let config = ToolInstallTool::mcp_config(
            &serde_json::json!({
                "kind": "mcp_server",
                "transport": "gateway",
                "url": "https://gateway.example.com/mcp",
                "gateway_mode": " SHARED ",
                "gateway_route": " project-tools "
            }),
            "shared-tools",
        )
        .expect("valid request")
        .expect("MCP config");
        assert!(matches!(
            config.effective_transport(),
            EffectiveTransport::Gateway {
                mode: McpGatewayMode::Shared,
                route: Some("project-tools")
            }
        ));
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
        let worker_params = serde_json::json!({
            "name": "worker-files",
            "kind": "mcp_server",
            "transport": "stdio",
            "command": "worker-files-mcp",
            "placement": "job_worker",
            "env_secrets": {"API_TOKEN": "api_token"}
        });
        assert_eq!(
            tool.requires_approval(&worker_params),
            ApprovalRequirement::Always
        );
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
        assert_eq!(
            schema["properties"]["placement"]["enum"],
            serde_json::json!(["host", "job_worker"])
        );
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
    fn tool_update_preserves_or_explicitly_switches_placement() {
        let existing = McpServerConfig::new_stdio("worker", "old", vec![], HashMap::new())
            .with_placement(McpPlacement::JobWorker);
        let preserved = ToolUpdateTool::build_config(
            &serde_json::json!({"name": "worker", "command": "new"}),
            &existing,
        )
        .expect("preserved config");
        assert_eq!(preserved.placement, McpPlacement::JobWorker);

        let switched = ToolUpdateTool::build_config(
            &serde_json::json!({
                "name": "worker",
                "transport": "stdio",
                "command": "new",
                "placement": "host"
            }),
            &existing,
        )
        .expect("switched config");
        assert_eq!(switched.placement, McpPlacement::Host);

        let error = ToolUpdateTool::build_config(
            &serde_json::json!({"transport": "http", "placement": "job_worker"}),
            &existing,
        )
        .expect_err("HTTP worker update must fail");
        assert!(error.to_string().contains("only support stdio"));
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
        for param in ["kind", "offset", "limit", "tools_offset", "tools_limit"] {
            assert!(
                schema["properties"].get(param).is_some(),
                "tool_list schema must expose {param}"
            );
        }
    }

    fn installed_extension(
        name: &str,
        kind: ExtensionKind,
        tools: Vec<String>,
    ) -> crate::extensions::InstalledExtension {
        crate::extensions::InstalledExtension {
            name: name.to_string(),
            kind,
            display_name: None,
            description: None,
            url: None,
            transport: None,
            command: None,
            placement: None,
            authenticated: true,
            active: true,
            recovering: false,
            enabled: None,
            tools,
            needs_setup: false,
            has_auth: false,
            installed: true,
            activation_error: None,
            version: None,
        }
    }

    #[test]
    fn tool_list_entry_windows_tool_names_with_continuation() {
        let tools: Vec<String> = (0..40).map(|i| format!("tool_{i}")).collect();
        let entry = extension_to_json(
            &installed_extension("big", ExtensionKind::McpServer, tools),
            0,
            25,
        );

        let listed = entry["tools"].as_array().expect("tools array");
        assert_eq!(listed.len(), 25);
        assert_eq!(entry["tools_total"], serde_json::json!(40));
        assert_eq!(entry["tools_truncated"], serde_json::json!(true));
        assert_eq!(entry["tools_next_offset"], serde_json::json!(25));

        // Finding 1: the dropped names must be retrievable — window past 25.
        let tools: Vec<String> = (0..40).map(|i| format!("tool_{i}")).collect();
        let tail = extension_to_json(
            &installed_extension("big", ExtensionKind::McpServer, tools),
            25,
            25,
        );
        let tail_names = tail["tools"].as_array().expect("tools array");
        assert_eq!(tail_names.len(), 15);
        assert_eq!(tail_names[0], serde_json::json!("tool_25"));
        assert_eq!(tail_names[14], serde_json::json!("tool_39"));
        // final window: no truncation marker, next_offset absent (Null)
        assert_eq!(tail.get("tools_truncated"), None);
        assert_eq!(tail["tools_next_offset"], serde_json::json!(null));
    }

    #[test]
    fn tool_list_entry_omits_absent_optional_fields() {
        let entry = extension_to_json(
            &installed_extension("plain", ExtensionKind::WasmChannel, Vec::new()),
            0,
            TOOL_LIST_TOOLS_PAGE_DEFAULT,
        );
        for field in [
            "display_name",
            "description",
            "version",
            "activation_error",
            "recovering",
            "tools_total",
            "tools_truncated",
            "enabled",
            "placement",
            "transport",
            "url",
            "command",
        ] {
            assert!(entry.get(field).is_none(), "field {field} should be absent");
        }
        assert_eq!(entry["name"], serde_json::json!("plain"));
        assert_eq!(entry["installed"], serde_json::json!(true));
    }

    #[test]
    fn tool_list_entry_preserves_mcp_status_and_config_fields() {
        // MR !1225 finding 2: the pre-pagination output serialized the full
        // InstalledExtension. Pagination must not drop status/config fields:
        // a deliberately disabled MCP server must stay distinguishable from
        // an inactive/failing one, and host vs job-worker placement must be
        // diagnosable from this surface.
        let mut extension = installed_extension("configured", ExtensionKind::McpServer, Vec::new());
        extension.enabled = Some(false);
        extension.placement = Some(crate::tools::mcp::McpPlacement::JobWorker);
        extension.transport = Some("stdio".to_string());
        extension.url = Some("https://example.com/mcp".to_string());
        extension.command = Some("server-binary".to_string());

        let entry = extension_to_json(&extension, 0, TOOL_LIST_TOOLS_PAGE_DEFAULT);
        assert_eq!(entry["enabled"], serde_json::json!(false));
        assert_eq!(entry["placement"], serde_json::json!("job_worker"));
        assert_eq!(entry["transport"], serde_json::json!("stdio"));
        assert_eq!(entry["url"], serde_json::json!("https://example.com/mcp"));
        assert_eq!(entry["command"], serde_json::json!("server-binary"));

        // WASM entries keep placement/enabled absent (None in the source).
        let wasm = extension_to_json(
            &installed_extension("wasm-tool", ExtensionKind::WasmTool, Vec::new()),
            0,
            TOOL_LIST_TOOLS_PAGE_DEFAULT,
        );
        assert!(wasm.get("enabled").is_none());
        assert!(wasm.get("placement").is_none());
    }

    #[test]
    fn tool_list_entry_bounds_restored_config_fields() {
        // The restored fields ride along bounded: worst-case escaped urls and
        // commands must not blow the per-entry budget reserved for tool names.
        let mut extension = installed_extension("long_url", ExtensionKind::McpServer, Vec::new());
        extension.transport = Some("t".repeat(1_000));
        extension.url = Some("u".repeat(1_000));
        extension.command = Some("c".repeat(1_000));
        extension.description = Some("d".repeat(1_000));
        extension.display_name = Some("n".repeat(1_000));

        let entry = extension_to_json(&extension, 0, TOOL_LIST_TOOLS_PAGE_DEFAULT);
        for field in ["transport", "url", "command", "description", "display_name"] {
            let value = entry[field].as_str().expect(field);
            assert!(
                value.chars().count() <= TOOL_LIST_FIELD_MAX_CHARS,
                "{field} must be bounded, got {}",
                value.chars().count()
            );
        }
        let serialized = serde_json::to_string(&entry).expect("serialize entry");
        assert!(
            serialized.len() <= TOOL_LIST_ENTRY_BUDGET_BYTES,
            "entry with bounded fields must stay within the per-entry budget (got {})",
            serialized.len()
        );
    }

    #[test]
    fn tool_list_bounds_unbounded_upstream_fields() {
        let long_desc = "x".repeat(5_000);
        let mut extension =
            installed_extension("desc_server", ExtensionKind::McpServer, Vec::new());
        extension.description = Some(long_desc);
        extension.display_name = Some("y".repeat(1_000));

        let entry = extension_to_json(&extension, 0, 25);
        let description = entry["description"].as_str().expect("description");
        assert!(
            description.chars().count() <= TOOL_LIST_FIELD_MAX_CHARS,
            "description must be bounded, got {}",
            description.chars().count()
        );
        assert!(description.ends_with(TOOL_LIST_ELLIPSIS));
        let display_name = entry["display_name"].as_str().expect("display name");
        assert!(display_name.chars().count() <= TOOL_LIST_FIELD_MAX_CHARS);
    }

    #[test]
    fn tool_list_param_parsing_defaults_and_errors() {
        let none = serde_json::json!({});
        assert_eq!(
            parse_bounded_param(&none, "offset", 0, usize::MAX).expect("default offset"),
            0
        );
        assert_eq!(
            parse_positive_param(&none, "limit", TOOL_LIST_DEFAULT_PAGE, TOOL_LIST_MAX_PAGE)
                .expect("default limit"),
            50
        );
        assert_eq!(
            parse_positive_param(
                &none,
                "tools_limit",
                TOOL_LIST_TOOLS_PAGE_DEFAULT,
                TOOL_LIST_TOOLS_PAGE_MAX
            )
            .expect("default tools_limit"),
            25
        );

        let explicit = serde_json::json!({"offset": 12, "limit": 7, "tools_limit": 40});
        assert_eq!(
            parse_bounded_param(&explicit, "offset", 0, usize::MAX).expect("offset"),
            12
        );
        assert_eq!(
            parse_positive_param(&explicit, "limit", 50, 100).expect("limit"),
            7
        );
        assert_eq!(
            parse_positive_param(&explicit, "tools_limit", 25, 100).expect("tools_limit"),
            40
        );

        // null means "not provided"
        assert_eq!(
            parse_bounded_param(
                &serde_json::json!({"offset": null}),
                "offset",
                0,
                usize::MAX
            )
            .expect("null offset"),
            0
        );

        // clamping, not errors, for over-max values
        assert_eq!(
            parse_positive_param(&serde_json::json!({"limit": 500}), "limit", 50, 100)
                .expect("clamped limit"),
            100
        );

        for bad in [
            serde_json::json!({"offset": -1}),
            serde_json::json!({"offset": "5"}),
            serde_json::json!({"limit": 0}),
            serde_json::json!({"limit": "many"}),
            serde_json::json!({"tools_limit": 0}),
        ] {
            let res1 = parse_bounded_param(&bad, "offset", 0, usize::MAX);
            let res2 = parse_positive_param(&bad, "limit", 50, 100);
            let res3 = parse_positive_param(&bad, "tools_limit", 25, 100);
            assert!(
                res1.is_err() || res2.is_err() || res3.is_err(),
                "one parser must reject {bad}"
            );
        }
    }

    #[test]
    fn tool_list_page_respects_serialized_byte_budget() {
        // Finding 2: 100 max-size entries (bounded 200-char descriptions,
        // 100 tool names each) must shrink to fit the byte budget, with
        // metadata describing the shrunken page.
        let make_entry = |i: usize| {
            let tools: Vec<String> = (0..100).map(|t| format!("server{i}_tool_{t}")).collect();
            let mut extension =
                installed_extension(&format!("server_{i:03}"), ExtensionKind::McpServer, tools);
            extension.description = Some("d".repeat(TOOL_LIST_FIELD_MAX_CHARS));
            extension_to_json(&extension, 0, TOOL_LIST_TOOLS_PAGE_MAX)
        };
        let page: Vec<serde_json::Value> = (0..100).map(make_entry).collect();

        let total = 100;
        let full_len = page_json_len(&page, 0, total).expect("serialize page");
        assert!(
            full_len > TOOL_LIST_PAGE_BUDGET_BYTES,
            "fixture must be oversized to exercise the budget (got {full_len})"
        );

        let fitted = match fit_page_to_budget(page, 0, total) {
            FittedPage::Entries(fitted) => fitted,
            other => panic!("oversized page must keep entries, got {other:?}"),
        };
        assert!(
            !fitted.is_empty(),
            "budget fit must keep at least one entry"
        );
        let fitted_len = page_json_len(&fitted, 0, total).expect("serialize fitted");
        assert!(
            fitted_len <= TOOL_LIST_PAGE_BUDGET_BYTES,
            "fitted page ({fitted_len}) must stay within the {} byte budget",
            TOOL_LIST_PAGE_BUDGET_BYTES
        );
    }

    #[test]
    fn tool_list_single_entry_fits_escaped_worst_case_names() {
        // MR !1221 finding 1: 100 tool names of 200 JSON-escaped control
        // characters serialize to >120 KiB pretty-printed. The tool-name
        // window must bound the entry below the page budget (and per-entry
        // budget) even in that worst case, and every name it does return
        // stays verbatim.
        let name: String = "\u{1}".repeat(200);
        let tools: Vec<String> = (0..100).map(|_| name.clone()).collect();
        let entry = extension_to_json(
            &installed_extension("escape_server", ExtensionKind::McpServer, tools.clone()),
            0,
            TOOL_LIST_TOOLS_PAGE_MAX,
        );

        let listed = entry["tools"].as_array().expect("tools array");
        assert!(
            listed.len() < 100,
            "worst-case escaped names must be windowed, got {}",
            listed.len()
        );
        assert_eq!(entry["tools_total"], serde_json::json!(100));
        assert_eq!(entry["tools_truncated"], serde_json::json!(true));
        assert_eq!(entry["tools_next_offset"], serde_json::json!(listed.len()));
        assert_eq!(
            listed[0].as_str().expect("verbatim name"),
            name,
            "tool names must stay lossless"
        );

        // The single-entry page fits the page budget without dropping the
        // entry (previously the fitter stopped at one entry unconditionally).
        let serialized = serde_json::to_string(&entry).expect("serialize entry");
        assert!(
            serialized.len() <= TOOL_LIST_ENTRY_BUDGET_BYTES,
            "entry ({}) must stay within the {} byte per-entry budget",
            serialized.len(),
            TOOL_LIST_ENTRY_BUDGET_BYTES
        );
        let single = match fit_page_to_budget(vec![entry.clone()], 0, 1) {
            FittedPage::Entries(single) => single,
            other => panic!("window-bounded entry must survive, got {other:?}"),
        };
        assert_eq!(
            single.len(),
            1,
            "a window-bounded entry must survive the page fitter"
        );

        // Continuation: the next window starts where this one ended and
        // keeps names lossless.
        let tail = extension_to_json(
            &installed_extension("escape_server", ExtensionKind::McpServer, tools),
            listed.len(),
            TOOL_LIST_TOOLS_PAGE_MAX,
        );
        let tail_names = tail["tools"].as_array().expect("tail tools");
        assert!(!tail_names.is_empty());
        assert_eq!(tail_names[0].as_str().expect("verbatim name"), name);
    }

    #[test]
    fn tool_list_long_identifiers_stay_lossless() {
        // MR !1221 finding 2: two tool names sharing a 198-character prefix
        // must remain distinguishable — identifiers are never elided, the
        // window shrinks instead.
        let shared = "t".repeat(198);
        let tools = vec![format!("{shared}A"), format!("{shared}B")];
        let entry = extension_to_json(
            &installed_extension("long_ids", ExtensionKind::McpServer, tools),
            0,
            TOOL_LIST_TOOLS_PAGE_MAX,
        );
        let listed = entry["tools"].as_array().expect("tools array");
        assert_eq!(listed.len(), 2, "both long names fit the entry budget");
        assert_eq!(listed[0].as_str(), Some(format!("{shared}A").as_str()));
        assert_eq!(listed[1].as_str(), Some(format!("{shared}B").as_str()));
    }

    #[test]
    fn tool_list_long_extension_name_stay_lossless() {
        // The entry name itself may exceed the descriptive field cap; it is
        // an identifier and must pass through verbatim (MR !1221 finding 2).
        let long_name = "n".repeat(600);
        let entry = extension_to_json(
            &installed_extension(&long_name, ExtensionKind::WasmTool, Vec::new()),
            0,
            TOOL_LIST_TOOLS_PAGE_DEFAULT,
        );
        assert_eq!(entry["name"].as_str(), Some(long_name.as_str()));
        assert!(
            !entry["name"]
                .as_str()
                .expect("name")
                .contains(TOOL_LIST_ELLIPSIS),
            "identifiers are never elided"
        );
    }

    #[test]
    fn tool_list_page_budget_metadata_stays_consistent() {
        // count/next_offset/truncated must describe the returned entries,
        // never claim completeness for a clipped page.
        let make_entry = |i: usize| {
            let tools: Vec<String> = (0..100).map(|t| format!("s{i}_t{t}")).collect();
            extension_to_json(
                &installed_extension(&format!("s{i:03}"), ExtensionKind::McpServer, tools),
                0,
                TOOL_LIST_TOOLS_PAGE_MAX,
            )
        };
        let page: Vec<serde_json::Value> = (0..100).map(make_entry).collect();
        let fitted = match fit_page_to_budget(page, 0, 100) {
            FittedPage::Entries(fitted) => fitted,
            other => panic!("oversized page must keep entries, got {other:?}"),
        };

        let next_offset = (fitted.len() < 100).then_some(fitted.len());
        let combined = serde_json::json!({
            "extensions": fitted,
            "count": fitted.len(),
            "total": 100,
            "offset": 0,
            "next_offset": next_offset,
            "truncated": next_offset.is_some(),
        });
        assert_eq!(combined["count"], serde_json::json!(fitted.len()));
        assert_eq!(combined["next_offset"], serde_json::json!(next_offset));
        assert_eq!(
            combined["truncated"],
            serde_json::json!(next_offset.is_some())
        );
        assert!(fitted.len() < 100, "oversized fixture must be shrunken");
    }

    #[test]
    fn tool_list_empty_inventory_returns_complete_empty_page() {
        // MR !1225 finding 1: a genuinely empty result (fresh tenant, or a
        // kind filter with no matches) must return the pre-pagination
        // {"extensions":[],"count":0} shape — not the budget-drop error.
        // Only a nonempty page dropped for exceeding the byte budget errors.
        assert!(matches!(
            fit_page_to_budget(Vec::new(), 0, 0),
            FittedPage::NoEntries
        ));
    }

    #[test]
    fn tool_list_nonempty_page_dropped_for_budget_is_distinguishable() {
        // The other half of MR !1225 finding 1: entries existed but none
        // fit — that stays a hard failure, never a silent empty page. The
        // extension name is an identifier and passes through verbatim, so a
        // name longer than the page budget is the one construct that can
        // still defeat the fitter today.
        let long_name = "n".repeat(TOOL_LIST_PAGE_BUDGET_BYTES * 2);
        let oversized = extension_to_json(
            &installed_extension(&long_name, ExtensionKind::McpServer, Vec::new()),
            0,
            TOOL_LIST_TOOLS_PAGE_DEFAULT,
        );
        let len = page_json_len(&[oversized.clone()], 0, 1).expect("serialize");
        assert!(
            len > TOOL_LIST_PAGE_BUDGET_BYTES,
            "fixture must exceed the page budget (got {len})"
        );
        assert!(matches!(
            fit_page_to_budget(vec![oversized], 0, 1),
            FittedPage::Empty
        ));
    }

    /// End-to-end WI#245 regression: a 31-server inventory (the reported
    /// failure size) must enumerate completely across pages, with every page
    /// small enough to survive the safety layer's byte truncation intact.
    /// Servers carry oversized descriptions to prove the byte budget, not
    /// just the entry cap, bounds each page.
    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn tool_list_paginates_full_inventory_without_silent_prefix() {
        use crate::context::JobContext;
        use crate::secrets::{InMemorySecretsStore, SecretsCrypto};
        use crate::tools::ToolRegistry;
        use crate::tools::mcp::process::McpProcessManager;
        use crate::tools::mcp::session::McpSessionManager;

        let (store, _db_dir) = crate::testing::test_db().await;
        let dir = tempfile::tempdir().expect("temp dir");
        let tools_dir = dir.path().join("tools");
        let channels_dir = dir.path().join("channels");
        std::fs::create_dir_all(&tools_dir).ok();
        std::fs::create_dir_all(&channels_dir).ok();

        let key = secrecy::SecretString::from(crate::secrets::keychain::generate_master_key_hex());
        let crypto = Arc::new(SecretsCrypto::new(key).expect("crypto"));
        let secrets: Arc<dyn crate::secrets::SecretsStore + Send + Sync> =
            Arc::new(InMemorySecretsStore::new(crypto));

        let manager = Arc::new(ExtensionManager::new(
            Arc::new(McpSessionManager::new()),
            Arc::new(McpProcessManager::new()),
            secrets,
            Arc::new(ToolRegistry::new()),
            None,
            None,
            tools_dir,
            channels_dir,
            None,
            "test".to_string(),
            Some(store),
            Vec::new(),
        ));

        for i in 0..31 {
            let config = McpServerConfig::new_stdio(
                format!("server_{i:02}"),
                "echo",
                vec![],
                HashMap::new(),
            )
            .with_description("x".repeat(5_000));
            manager
                .install_mcp_config(config, "test")
                .await
                .expect("install test MCP config");
        }

        let tool = ToolListTool::new(manager);
        let ctx = crate::context::JobContext {
            user_id: "test".to_string(),
            ..JobContext::default()
        };

        // Page through with limit=20, the reported broken size class: page 1
        // previously looked complete while 12 servers were silently dropped.
        let mut seen: Vec<String> = Vec::new();
        let mut offset = 0usize;
        let mut pages = 0;
        loop {
            let output = tool
                .execute(serde_json::json!({"limit": 20, "offset": offset}), &ctx)
                .await
                .expect("tool_list page");
            pages += 1;

            let result = &output.result;
            let entries = result["extensions"].as_array().expect("extensions array");
            let serialized = serde_json::to_string_pretty(&result).expect("serialize page pretty");
            assert!(
                serialized.len() < 100_000,
                "page must stay below the 100 KiB safety truncation limit (got {})",
                serialized.len()
            );

            for entry in entries {
                seen.push(entry["name"].as_str().expect("name").to_string());
                let description = entry["description"].as_str().expect("description");
                assert!(
                    description.chars().count() <= TOOL_LIST_FIELD_MAX_CHARS,
                    "oversized upstream description must be bounded in output"
                );
            }

            match result["next_offset"].as_u64() {
                Some(next) => offset = next as usize,
                None => {
                    assert_eq!(result["truncated"], serde_json::json!(false));
                    assert_eq!(result["total"], serde_json::json!(31));
                    break;
                }
            }
        }

        assert_eq!(pages, 2, "31 entries at limit 20 spans two pages");
        assert_eq!(seen.len(), 31, "every installed server must be enumerated");

        let mut sorted = seen.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 31, "no server may appear twice");
        assert_eq!(
            seen[0], "server_00",
            "deterministic ordering starts at the first name"
        );

        // offset past the end is a caller error, not an empty page.
        let err = tool
            .execute(serde_json::json!({"offset": 99}), &ctx)
            .await
            .expect_err("offset beyond total must fail");
        assert!(err.to_string().contains("exceeds total"));

        // offset == total is the empty tail page a pagination loop lands on
        // after the last entry: it must be a complete empty page, not the
        // budget-drop error (MR !1225 finding 1).
        let tail = tool
            .execute(serde_json::json!({"offset": 31, "limit": 20}), &ctx)
            .await
            .expect("tail page at offset == total");
        assert_eq!(tail.result["extensions"], serde_json::json!([]));
        assert_eq!(tail.result["count"], serde_json::json!(0));
        assert_eq!(tail.result["total"], serde_json::json!(31));
        assert_eq!(tail.result["truncated"], serde_json::json!(false));
    }

    /// Fresh-tenant shape (MR !1225 finding 1): an inventory with zero
    /// matching entries must return the pre-pagination empty result, not an
    /// error. A `kind` filter with no matches exercises the same path.
    #[cfg(feature = "libsql")]
    #[tokio::test]
    async fn tool_list_empty_inventory_and_unmatched_filter_return_empty_page() {
        use crate::context::JobContext;
        use crate::secrets::{InMemorySecretsStore, SecretsCrypto};
        use crate::tools::ToolRegistry;
        use crate::tools::mcp::process::McpProcessManager;
        use crate::tools::mcp::session::McpSessionManager;

        let (store, _db_dir) = crate::testing::test_db().await;
        let dir = tempfile::tempdir().expect("temp dir");
        let manager = Arc::new(ExtensionManager::new(
            Arc::new(McpSessionManager::new()),
            Arc::new(McpProcessManager::new()),
            {
                let key = secrecy::SecretString::from(
                    crate::secrets::keychain::generate_master_key_hex(),
                );
                let crypto = Arc::new(SecretsCrypto::new(key).expect("crypto"));
                Arc::new(InMemorySecretsStore::new(crypto))
                    as Arc<dyn crate::secrets::SecretsStore + Send + Sync>
            },
            Arc::new(ToolRegistry::new()),
            None,
            None,
            dir.path().join("tools"),
            dir.path().join("channels"),
            None,
            "test".to_string(),
            Some(store),
            Vec::new(),
        ));

        let tool = ToolListTool::new(manager);
        let ctx = crate::context::JobContext {
            user_id: "test".to_string(),
            ..JobContext::default()
        };

        let empty = tool
            .execute(serde_json::json!({}), &ctx)
            .await
            .expect("empty inventory must not error");
        assert_eq!(empty.result["extensions"], serde_json::json!([]));
        assert_eq!(empty.result["count"], serde_json::json!(0));
        assert_eq!(empty.result["total"], serde_json::json!(0));
        assert_eq!(empty.result["truncated"], serde_json::json!(false));

        let unmatched = tool
            .execute(
                serde_json::json!({"kind": "mcp_server", "include_available": true}),
                &ctx,
            )
            .await
            .expect("unmatched filter must not error");
        assert_eq!(unmatched.result["extensions"], serde_json::json!([]));
        assert_eq!(unmatched.result["count"], serde_json::json!(0));
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
