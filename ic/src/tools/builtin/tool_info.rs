//! On-demand tool discovery (like CLI `--help`).
//!
//! Three levels of detail:
//! - Default: name, description, parameter names (compact ~150 bytes)
//! - `detail: "summary"`: adds curated rules, notes, and examples
//! - `detail: "schema"` / `include_schema: true`: adds the full typed JSON Schema
//!
//! Keeps the tools array compact (WASM tools use permissive schemas)
//! while allowing precise discovery when needed.
//!
//! The tool itself only knows the tool registry. Engine V2 threads can also
//! call capability functions that are not registry tools (the `mission_*`
//! family); the effect bridge answers `tool_info` for those with
//! [`engine_action_info`] so a registry miss is never mistaken for "this
//! function does not exist" (WI#295).

use std::sync::Weak;

use async_trait::async_trait;

use crate::context::JobContext;
use crate::tools::registry::ToolRegistry;
use crate::tools::tool::{Tool, ToolDiscoverySummary, ToolError, ToolOutput};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolInfoDetail {
    Names,
    Summary,
    Schema,
}

impl ToolInfoDetail {
    fn parse(params: &serde_json::Value) -> Result<Self, ToolError> {
        if params
            .get("include_schema")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return Ok(Self::Schema);
        }

        match params.get("detail").and_then(|v| v.as_str()) {
            None | Some("names") => Ok(Self::Names),
            Some("summary") => Ok(Self::Summary),
            Some("schema") => Ok(Self::Schema),
            Some(other) => Err(ToolError::InvalidParameters(format!(
                "invalid detail '{other}' (expected 'names', 'summary', or 'schema')"
            ))),
        }
    }
}

fn schema_param_names(schema: &serde_json::Value) -> Vec<String> {
    let mut names = std::collections::BTreeSet::new();

    if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
        names.extend(props.keys().cloned());
    }

    for key in ["allOf", "oneOf", "anyOf"] {
        if let Some(variants) = schema.get(key).and_then(|v| v.as_array()) {
            for variant in variants {
                if let Some(props) = variant.get("properties").and_then(|p| p.as_object()) {
                    names.extend(props.keys().cloned());
                }
            }
        }
    }

    names.into_iter().collect()
}

fn fallback_summary(schema: &serde_json::Value) -> ToolDiscoverySummary {
    ToolDiscoverySummary {
        always_required: schema
            .get("required")
            .and_then(|v| v.as_array())
            .map(|required| {
                required
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        ..ToolDiscoverySummary::default()
    }
}

/// The name a `tool_info` call asks about: the `name` parameter, or the first
/// positional argument when CodeAct calls `tool_info("x")` (`{"_args": ["x"]}`).
pub(crate) fn requested_tool_name(params: &serde_json::Value) -> Result<&str, ToolError> {
    params
        .get("name")
        .or_else(|| params.get("_args").and_then(|args| args.get(0)))
        .and_then(|value| value.as_str())
        .ok_or_else(|| ToolError::InvalidParameters("missing 'name' parameter".to_string()))
}

/// Shared `tool_info` answer shape: name, description and parameter names,
/// plus the curated summary or the full schema when `detail` asks for them.
fn render_info(
    name: &str,
    description: &str,
    schema: serde_json::Value,
    summary: Option<ToolDiscoverySummary>,
    detail: ToolInfoDetail,
) -> Result<serde_json::Value, ToolError> {
    let mut info = serde_json::json!({
        "name": name,
        "description": description,
        "parameters": schema_param_names(&schema),
    });

    match detail {
        ToolInfoDetail::Names => {}
        ToolInfoDetail::Summary => {
            let summary = summary.unwrap_or_else(|| fallback_summary(&schema));
            info["summary"] = serde_json::to_value(summary).map_err(|err| {
                ToolError::ExecutionFailed(format!("failed to serialize discovery summary: {err}"))
            })?;
        }
        ToolInfoDetail::Schema => {
            info["schema"] = schema;
        }
    }

    Ok(info)
}

/// `tool_info` answer for an Engine V2 capability function that is not a
/// registry tool, such as `mission_create` (WI#295). Same shape as a registry
/// answer at every detail level, plus fields saying what it is and how to call
/// it. Only call this where the function really is callable — the effect
/// bridge does, for engine threads with the capability wired.
pub(crate) fn engine_action_info(
    name: &str,
    description: &str,
    schema: serde_json::Value,
    capability: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, ToolError> {
    let detail = ToolInfoDetail::parse(params)?;
    let mut info = render_info(name, description, schema, None, detail)?;
    info["kind"] = serde_json::json!("engine_capability");
    info["capability"] = serde_json::json!(capability);
    info["registered_tool"] = serde_json::json!(false);
    info["callable"] = serde_json::json!(true);
    info["note"] = serde_json::json!(format!(
        "'{name}' is not a registry tool, so the tool registry and tool_list do not show \
         it. It is an Engine V2 '{capability}' capability function, directly callable \
         from this thread's CodeAct code, e.g. `await {name}(...)`."
    ));
    Ok(info)
}

pub struct ToolInfoTool {
    registry: Weak<ToolRegistry>,
}

impl ToolInfoTool {
    pub fn new(registry: Weak<ToolRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for ToolInfoTool {
    fn name(&self) -> &str {
        "tool_info"
    }

    fn description(&self) -> &str {
        "Get info about any tool: description, parameter names, curated summary guidance, or full discovery schema."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Name of the tool to get info about"
                },
                "detail": {
                    "type": "string",
                    "enum": ["names", "summary", "schema"],
                    "description": "Response detail level. 'names' returns parameter names only. 'summary' adds curated rules/examples. 'schema' returns the full discovery schema.",
                    "default": "names"
                },
                "include_schema": {
                    "type": "boolean",
                    "description": "Deprecated compatibility alias for detail='schema'. If true, include the full discovery schema.",
                    "default": false
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
        let name = requested_tool_name(&params)?;
        let detail = ToolInfoDetail::parse(&params)?;

        let registry = self.registry.upgrade().ok_or_else(|| {
            ToolError::ExecutionFailed(
                "tool registry is no longer available for tool_info".to_string(),
            )
        })?;

        // Resolve legacy hyphen/underscore aliases the way dispatch does, so a
        // tool callable as `gitlab_tool` is not reported unregistered.
        let (_, tool) = registry
            .get_resolved_for_user(name, &ctx.user_id)
            .await
            .ok_or_else(|| {
                ToolError::InvalidParameters(format!("No tool named '{name}' is registered"))
            })?;

        let summary = if detail == ToolInfoDetail::Summary {
            tool.discovery_summary()
        } else {
            None
        };
        let info = render_info(
            tool.name(),
            tool.description(),
            tool.discovery_schema(),
            summary,
            detail,
        )?;

        Ok(ToolOutput::success(info, start.elapsed()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::EchoTool;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_tool_info_default_returns_param_names() {
        let registry = Arc::new(ToolRegistry::new());
        registry.register(Arc::new(EchoTool)).await;

        let tool = ToolInfoTool::new(Arc::downgrade(&registry));
        let ctx = JobContext::default();
        let result = tool
            .execute(serde_json::json!({"name": "echo"}), &ctx)
            .await
            .unwrap();

        let info = &result.result;
        assert_eq!(info["name"], "echo");
        assert!(!info["description"].as_str().unwrap().is_empty());
        // Default: parameters is an array of names, not the full schema
        assert!(info["parameters"].is_array());
        assert!(
            info["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v.as_str() == Some("message")),
            "echo tool should have 'message' parameter: {:?}",
            info["parameters"]
        );
        // No schema field by default
        assert!(info.get("schema").is_none());
    }

    #[tokio::test]
    async fn test_tool_info_with_summary() {
        let registry = Arc::new(ToolRegistry::new());
        registry.register(Arc::new(EchoTool)).await;

        let tool = ToolInfoTool::new(Arc::downgrade(&registry));
        let ctx = JobContext::default();
        let result = tool
            .execute(
                serde_json::json!({"name": "echo", "detail": "summary"}),
                &ctx,
            )
            .await
            .unwrap();

        let info = &result.result;
        assert_eq!(info["name"], "echo");
        assert!(info["summary"].is_object());
        assert_eq!(
            info["summary"]["always_required"],
            serde_json::json!(["message"])
        );
    }

    #[tokio::test]
    async fn test_tool_info_with_schema() {
        let registry = Arc::new(ToolRegistry::new());
        registry.register(Arc::new(EchoTool)).await;

        let tool = ToolInfoTool::new(Arc::downgrade(&registry));
        let ctx = JobContext::default();
        let result = tool
            .execute(
                serde_json::json!({"name": "echo", "include_schema": true}),
                &ctx,
            )
            .await
            .unwrap();

        let info = &result.result;
        assert_eq!(info["name"], "echo");
        // With include_schema: true, schema field should be present
        assert!(info["schema"].is_object());
        assert!(info["schema"]["properties"].is_object());
    }

    #[tokio::test]
    async fn test_tool_info_invalid_detail() {
        let registry = Arc::new(ToolRegistry::new());
        registry.register(Arc::new(EchoTool)).await;

        let tool = ToolInfoTool::new(Arc::downgrade(&registry));
        let ctx = JobContext::default();
        let result = tool
            .execute(
                serde_json::json!({"name": "echo", "detail": "verbose"}),
                &ctx,
            )
            .await;
        assert!(matches!(result, Err(ToolError::InvalidParameters(_))));
    }

    #[tokio::test]
    async fn test_tool_info_unknown_tool() {
        let registry = Arc::new(ToolRegistry::new());
        let tool = ToolInfoTool::new(Arc::downgrade(&registry));
        let ctx = JobContext::default();
        let result = tool
            .execute(serde_json::json!({"name": "nonexistent"}), &ctx)
            .await;
        // The registry-only answer stays truthful outside engine threads
        // (v1 has no mission functions); the effect bridge answers for engine
        // capability functions before this tool runs (WI#295).
        match result {
            Err(ToolError::InvalidParameters(message)) => {
                assert_eq!(message, "No tool named 'nonexistent' is registered");
            }
            other => panic!("expected InvalidParameters, got {other:?}"),
        }
    }

    /// CodeAct passes positional arguments as `_args`, so `tool_info("echo")`
    /// must resolve the name instead of failing on a missing parameter.
    #[tokio::test]
    async fn test_tool_info_accepts_positional_name() {
        let registry = Arc::new(ToolRegistry::new());
        registry.register(Arc::new(EchoTool)).await;
        let tool = ToolInfoTool::new(Arc::downgrade(&registry));
        let result = tool
            .execute(
                serde_json::json!({"_args": ["echo"]}),
                &JobContext::default(),
            )
            .await
            .expect("positional name resolves");
        assert_eq!(result.result["name"], "echo");
    }

    struct HyphenatedTool;

    #[async_trait]
    impl Tool for HyphenatedTool {
        fn name(&self) -> &str {
            "legacy-tool"
        }

        fn description(&self) -> &str {
            "Legacy hyphen-named tool"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {"q": {"type": "string"}}})
        }

        async fn execute(
            &self,
            _params: serde_json::Value,
            _ctx: &JobContext,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text("ok", std::time::Duration::ZERO))
        }
    }

    /// Dispatch resolves `legacy_tool` to a hyphen-registered `legacy-tool`;
    /// tool_info must resolve it the same way instead of reporting it
    /// unregistered (WI#295 false-negative class).
    #[tokio::test]
    async fn test_tool_info_resolves_hyphen_alias() {
        let registry = Arc::new(ToolRegistry::new());
        registry.register(Arc::new(HyphenatedTool)).await;
        let tool = ToolInfoTool::new(Arc::downgrade(&registry));
        let result = tool
            .execute(
                serde_json::json!({"name": "legacy_tool"}),
                &JobContext::default(),
            )
            .await
            .expect("underscore alias resolves");
        assert_eq!(result.result["name"], "legacy-tool");
        assert_eq!(result.result["parameters"], serde_json::json!(["q"]));
    }

    #[test]
    fn test_engine_action_info_matches_registry_shape_and_marks_kind() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"name": {"type": "string"}, "goal": {"type": "string"}},
            "required": ["name", "goal"]
        });
        let names = engine_action_info(
            "mission_create",
            "Create a mission",
            schema.clone(),
            "missions",
            &serde_json::json!({"name": "mission_create"}),
        )
        .expect("names detail");
        assert_eq!(names["name"], "mission_create");
        assert_eq!(names["description"], "Create a mission");
        assert_eq!(names["parameters"], serde_json::json!(["goal", "name"]));
        assert_eq!(names["kind"], "engine_capability");
        assert_eq!(names["capability"], "missions");
        assert_eq!(names["registered_tool"], false);
        assert_eq!(names["callable"], true);
        let note = names["note"].as_str().expect("note");
        assert!(note.contains("not a registry tool"), "{note}");
        assert!(note.contains("CodeAct"), "{note}");
        assert!(names.get("schema").is_none());

        let summary = engine_action_info(
            "mission_create",
            "Create a mission",
            schema.clone(),
            "missions",
            &serde_json::json!({"name": "mission_create", "detail": "summary"}),
        )
        .expect("summary detail");
        assert_eq!(
            summary["summary"]["always_required"],
            serde_json::json!(["name", "goal"])
        );

        let full = engine_action_info(
            "mission_create",
            "Create a mission",
            schema.clone(),
            "missions",
            &serde_json::json!({"name": "mission_create", "detail": "schema"}),
        )
        .expect("schema detail");
        assert_eq!(full["schema"], schema);

        assert!(matches!(
            engine_action_info(
                "mission_create",
                "Create a mission",
                schema,
                "missions",
                &serde_json::json!({"name": "mission_create", "detail": "verbose"}),
            ),
            Err(ToolError::InvalidParameters(_))
        ));
    }

    #[tokio::test]
    async fn test_tool_info_registry_dropped() {
        let registry = Arc::new(ToolRegistry::new());
        let tool = ToolInfoTool::new(Arc::downgrade(&registry));
        drop(registry);

        let ctx = JobContext::default();
        let result = tool
            .execute(serde_json::json!({"name": "echo"}), &ctx)
            .await;
        assert!(matches!(result, Err(ToolError::ExecutionFailed(_))));
    }
}
