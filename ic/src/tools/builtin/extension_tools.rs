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

/// Maximum extension entries per tool_list page. Combined with the per-entry
/// tools cap this keeps a serialized page far below the safety layer's output
/// truncation limit, so a page can never be silently clipped mid-JSON.
const TOOL_LIST_MAX_PAGE: usize = 50;
/// Maximum per-extension tool names embedded in a tool_list entry. A server
/// with hundreds of exposed tools would otherwise consume the whole page
/// budget on one entry.
const TOOL_LIST_MAX_TOOLS_PER_ENTRY: usize = 25;

fn extension_to_json(extension: &crate::extensions::InstalledExtension) -> serde_json::Value {
    let tools_total = extension.tools.len();
    let tools_truncated = tools_total > TOOL_LIST_MAX_TOOLS_PER_ENTRY;
    let tools: Vec<&String> = extension
        .tools
        .iter()
        .take(TOOL_LIST_MAX_TOOLS_PER_ENTRY)
        .collect();

    let mut obj = serde_json::Map::new();
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
    obj.insert("tools".to_string(), serde_json::json!(tools));
    if tools_truncated {
        obj.insert(
            "tools_truncated".to_string(),
            serde_json::json!(tools_truncated),
        );
        obj.insert("tools_total".to_string(), serde_json::json!(tools_total));
    }
    // Optional fields ride along only when present to keep pages compact.
    for (field, value) in [
        ("display_name", &extension.display_name),
        ("description", &extension.description),
        ("version", &extension.version),
        ("activation_error", &extension.activation_error),
    ] {
        if value.is_some() {
            obj.insert(
                field.to_string(),
                serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
            );
        }
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

pub struct ToolListTool {
    manager: Arc<ExtensionManager>,
}

impl ToolListTool {
    pub fn new(manager: Arc<ExtensionManager>) -> Self {
        Self { manager }
    }

    fn parse_offset(params: &serde_json::Value) -> Result<usize, ToolError> {
        let raw = params.get("offset");
        if raw.is_none() || raw.is_some_and(serde_json::Value::is_null) {
            return Ok(0);
        }
        raw.and_then(serde_json::Value::as_u64)
            .map(|v| v as usize)
            .ok_or_else(|| {
                ToolError::InvalidParameters("offset must be a non-negative integer".to_string())
            })
    }

    fn parse_limit(params: &serde_json::Value) -> Result<usize, ToolError> {
        let raw = params.get("limit");
        if raw.is_none() || raw.is_some_and(serde_json::Value::is_null) {
            return Ok(TOOL_LIST_MAX_PAGE);
        }
        raw.and_then(serde_json::Value::as_u64)
            .map(|v| (v as usize).clamp(1, 100))
            .ok_or_else(|| {
                ToolError::InvalidParameters("limit must be a positive integer".to_string())
            })
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
         Per-extension tool names are capped; tools_truncated/tools_total mark entries \
         whose full tool inventory needs a follow-up."
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

        let offset = Self::parse_offset(&params)?;
        let limit = Self::parse_limit(&params)?;

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
            .map(extension_to_json)
            .collect();
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
        assert!(schema["properties"].get("kind").is_some());
        assert!(schema["properties"].get("offset").is_some());
        assert!(schema["properties"].get("limit").is_some());
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
    fn tool_list_entry_caps_per_extension_tools() {
        let tools: Vec<String> = (0..40).map(|i| format!("tool_{i}")).collect();
        let entry = extension_to_json(&installed_extension("big", ExtensionKind::McpServer, tools));

        let listed = entry["tools"].as_array().expect("tools array");
        assert_eq!(listed.len(), TOOL_LIST_MAX_TOOLS_PER_ENTRY);
        assert_eq!(entry["tools_truncated"], serde_json::json!(true));
        assert_eq!(entry["tools_total"], serde_json::json!(40));

        let small = extension_to_json(&installed_extension(
            "small",
            ExtensionKind::WasmTool,
            vec!["only".to_string()],
        ));
        assert_eq!(small["tools"].as_array().expect("tools array").len(), 1);
        assert!(small.get("tools_truncated").is_none());
        assert!(small.get("tools_total").is_none());
    }

    #[test]
    fn tool_list_entry_omits_absent_optional_fields() {
        let entry = extension_to_json(&installed_extension(
            "plain",
            ExtensionKind::WasmChannel,
            Vec::new(),
        ));
        for field in [
            "display_name",
            "description",
            "version",
            "activation_error",
            "recovering",
        ] {
            assert!(entry.get(field).is_none(), "field {field} should be absent");
        }
        assert_eq!(entry["name"], serde_json::json!("plain"));
        assert_eq!(entry["installed"], serde_json::json!(true));
    }

    #[test]
    fn tool_list_offset_and_limit_parse() {
        assert_eq!(
            ToolListTool::parse_offset(&serde_json::json!({})).expect("default"),
            0
        );
        assert_eq!(
            ToolListTool::parse_offset(&serde_json::json!({"offset": null})).expect("null"),
            0
        );
        assert_eq!(
            ToolListTool::parse_offset(&serde_json::json!({"offset": 12})).expect("int"),
            12
        );
        assert!(ToolListTool::parse_offset(&serde_json::json!({"offset": -1})).is_err());
        assert!(ToolListTool::parse_offset(&serde_json::json!({"offset": "5"})).is_err());

        assert_eq!(
            ToolListTool::parse_limit(&serde_json::json!({})).expect("default"),
            TOOL_LIST_MAX_PAGE
        );
        assert_eq!(
            ToolListTool::parse_limit(&serde_json::json!({"limit": 7})).expect("int"),
            7
        );
        assert_eq!(
            ToolListTool::parse_limit(&serde_json::json!({"limit": 500})).expect("clamped"),
            100
        );
        assert!(ToolListTool::parse_limit(&serde_json::json!({"limit": "many"})).is_err());
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
