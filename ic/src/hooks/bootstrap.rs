//! Hook bootstrap helpers for loading bundled, plugin, and workspace hooks.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::channels::wasm::discover_channels;
use crate::hooks::bundled::{
    HookBundleConfig, HookRegistrationSummary, register_bundle, register_bundled_hooks,
};
use crate::hooks::coordination::{CoordinationHookConfig, coordination_channel_guard};
use crate::hooks::registry::HookRegistry;
use crate::tools::wasm::{discover_dev_tools, discover_tools};
use crate::workspace::Workspace;

/// Summary of hook bootstrap work done at startup.
#[derive(Debug, Default, Clone, Copy)]
pub struct HookBootstrapSummary {
    /// Number of bundled built-in hooks registered.
    pub bundled_hooks: usize,
    /// Number of plugin-provided rule hooks registered.
    pub plugin_hooks: usize,
    /// Number of workspace-provided rule hooks registered.
    pub workspace_hooks: usize,
    /// Number of outbound webhook hooks registered.
    pub outbound_webhooks: usize,
    /// Number of invalid hook configs skipped.
    pub errors: usize,
}

impl HookBootstrapSummary {
    /// Total number of hooks registered across all categories.
    pub fn total_hooks(&self) -> usize {
        self.bundled_hooks + self.plugin_hooks + self.workspace_hooks + self.outbound_webhooks
    }
}

const MANAGED_COORDINATION_BUNDLE: &str = include_str!("../../config/hooks/coordination.hook.json");

/// Register bundled hooks, then load managed, plugin, and workspace hook bundles.
#[allow(clippy::too_many_arguments)]
pub async fn bootstrap_hooks(
    registry: &Arc<HookRegistry>,
    workspace: Option<&Arc<Workspace>>,
    coordination_config: &CoordinationHookConfig,
    wasm_tools_dir: &Path,
    wasm_channels_dir: &Path,
    active_tool_names: &[String],
    active_channel_names: &[String],
    dev_loaded_tool_names: &[String],
) -> HookBootstrapSummary {
    let mut summary = HookBootstrapSummary::default();

    let bundled = register_bundled_hooks(registry).await;
    summary.bundled_hooks += bundled.hooks;
    summary.outbound_webhooks += bundled.outbound_webhooks;
    summary.errors += bundled.errors;

    let managed = register_managed_coordination_bundle(registry, coordination_config).await;
    summary.bundled_hooks += managed.hooks;
    summary.outbound_webhooks += managed.outbound_webhooks;
    summary.errors += managed.errors;

    let plugin = register_plugin_bundles(
        registry,
        wasm_tools_dir,
        wasm_channels_dir,
        active_tool_names,
        active_channel_names,
        dev_loaded_tool_names,
    )
    .await;
    summary.plugin_hooks += plugin.hooks;
    summary.outbound_webhooks += plugin.outbound_webhooks;
    summary.errors += plugin.errors;

    if let Some(workspace) = workspace {
        let workspace_loaded =
            register_workspace_bundles(registry, workspace, coordination_config).await;
        summary.workspace_hooks += workspace_loaded.hooks;
        summary.outbound_webhooks += workspace_loaded.outbound_webhooks;
        summary.errors += workspace_loaded.errors;
    }

    summary
}

async fn register_managed_coordination_bundle(
    registry: &Arc<HookRegistry>,
    config: &CoordinationHookConfig,
) -> HookRegistrationSummary {
    if !config.is_actionable() {
        return HookRegistrationSummary::default();
    }
    let parsed: serde_json::Value = match serde_json::from_str(MANAGED_COORDINATION_BUNDLE) {
        Ok(parsed) => parsed,
        Err(error) => {
            tracing::error!(%error, "Managed coordination hook bundle is invalid JSON");
            return HookRegistrationSummary {
                hooks: 0,
                outbound_webhooks: 0,
                errors: 1,
            };
        }
    };
    let mut bundle = match parse_workspace_bundle(&parsed) {
        Ok(bundle) => bundle,
        Err(error) => {
            tracing::error!(%error, "Managed coordination hook bundle is invalid");
            return HookRegistrationSummary {
                hooks: 0,
                outbound_webhooks: 0,
                errors: 1,
            };
        }
    };
    scope_coordination_bundle(&mut bundle, config);
    register_bundle(registry, "managed.coordination", bundle).await
}

async fn register_plugin_bundles(
    registry: &Arc<HookRegistry>,
    wasm_tools_dir: &Path,
    wasm_channels_dir: &Path,
    active_tool_names: &[String],
    active_channel_names: &[String],
    dev_loaded_tool_names: &[String],
) -> HookRegistrationSummary {
    let mut summary = HookRegistrationSummary::default();
    let files = collect_plugin_capability_files(
        wasm_tools_dir,
        wasm_channels_dir,
        active_tool_names,
        active_channel_names,
        dev_loaded_tool_names,
    )
    .await;

    for (source, path) in files {
        let registered =
            register_plugin_bundle_from_capabilities_file(registry, &source, &path).await;
        summary.merge(registered);
    }

    summary
}

/// Register a plugin hook bundle from a single capabilities file.
///
/// This is used by startup bootstrap and by runtime extension activation.
pub async fn register_plugin_bundle_from_capabilities_file(
    registry: &Arc<HookRegistry>,
    source: &str,
    path: &Path,
) -> HookRegistrationSummary {
    match load_plugin_bundle_from_capabilities_file(path).await {
        Ok(Some(bundle)) => register_bundle(registry, source, bundle).await,
        Ok(None) => HookRegistrationSummary::default(),
        Err(err) => {
            tracing::warn!(
                source = source,
                path = %path.display(),
                error = %err,
                "Skipping plugin hook bundle"
            );
            HookRegistrationSummary {
                hooks: 0,
                outbound_webhooks: 0,
                errors: 1,
            }
        }
    }
}

async fn collect_plugin_capability_files(
    wasm_tools_dir: &Path,
    wasm_channels_dir: &Path,
    active_tool_names: &[String],
    active_channel_names: &[String],
    dev_loaded_tool_names: &[String],
) -> Vec<(String, PathBuf)> {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let active_tools: HashSet<&str> = active_tool_names.iter().map(String::as_str).collect();
    let active_channels: HashSet<&str> = active_channel_names.iter().map(String::as_str).collect();
    let dev_loaded_tools: HashSet<&str> =
        dev_loaded_tool_names.iter().map(String::as_str).collect();

    if wasm_tools_dir.exists() {
        match discover_tools(wasm_tools_dir).await {
            Ok(tools) => {
                for (name, tool) in tools {
                    if let Some(path) = tool.capabilities_path
                        && active_tools.contains(name.as_str())
                        && !dev_loaded_tools.contains(name.as_str())
                    {
                        insert_unique(&mut files, &mut seen, format!("plugin.tool:{}", name), path);
                    }
                }
            }
            Err(err) => {
                tracing::warn!(
                    path = %wasm_tools_dir.display(),
                    error = %err,
                    "Failed to discover WASM tool capabilities for plugin hooks"
                );
            }
        }
    }

    match discover_dev_tools().await {
        Ok(dev_tools) => {
            for (name, tool) in dev_tools {
                if let Some(path) = tool.capabilities_path
                    && active_tools.contains(name.as_str())
                    && dev_loaded_tools.contains(name.as_str())
                {
                    insert_unique(
                        &mut files,
                        &mut seen,
                        format!("plugin.dev_tool:{}", name),
                        path,
                    );
                }
            }
        }
        Err(err) => {
            tracing::debug!(error = %err, "No dev tool capabilities discovered for plugin hooks");
        }
    }

    if wasm_channels_dir.exists() {
        match discover_channels(wasm_channels_dir).await {
            Ok(channels) => {
                for (name, channel) in channels {
                    if let Some(path) = channel.capabilities_path
                        && active_channels.contains(name.as_str())
                    {
                        insert_unique(
                            &mut files,
                            &mut seen,
                            format!("plugin.channel:{}", name),
                            path,
                        );
                    }
                }
            }
            Err(err) => {
                tracing::warn!(
                    path = %wasm_channels_dir.display(),
                    error = %err,
                    "Failed to discover WASM channel capabilities for plugin hooks"
                );
            }
        }
    }

    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

fn insert_unique(
    files: &mut Vec<(String, PathBuf)>,
    seen: &mut HashSet<String>,
    source: String,
    path: PathBuf,
) {
    let key = path.to_string_lossy().to_string();
    if seen.insert(key) {
        files.push((source, path));
    }
}

async fn load_plugin_bundle_from_capabilities_file(
    path: &Path,
) -> Result<Option<HookBundleConfig>, String> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("read failed: {e}"))?;

    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid JSON: {e}"))?;

    let Some(hooks_value) = extract_hooks_section(&value) else {
        return Ok(None);
    };

    HookBundleConfig::from_value(hooks_value)
        .map(Some)
        .map_err(|e| e.to_string())
}

fn extract_hooks_section(root: &serde_json::Value) -> Option<&serde_json::Value> {
    root.get("hooks")
        .or_else(|| root.get("capabilities").and_then(|c| c.get("hooks")))
}

async fn register_workspace_bundles(
    registry: &Arc<HookRegistry>,
    workspace: &Arc<Workspace>,
    coordination_config: &CoordinationHookConfig,
) -> HookRegistrationSummary {
    let mut summary = HookRegistrationSummary::default();

    let paths = match workspace.list_all().await {
        Ok(paths) => paths,
        Err(err) => {
            summary.errors += 1;
            tracing::warn!(error = %err, "Failed to list workspace paths for hooks");
            return summary;
        }
    };

    let mut hook_paths: Vec<String> = paths
        .into_iter()
        .filter(|path| is_workspace_hook_file(path))
        .collect();
    hook_paths.sort();

    for path in hook_paths {
        if path == "hooks/coordination.hook.json" {
            tracing::debug!(
                path = %path,
                enabled = coordination_config.is_actionable(),
                "Ignoring legacy workspace coordination bundle; managed bundle owns these rules"
            );
            continue;
        }
        let doc = match workspace.read(&path).await {
            Ok(doc) => doc,
            Err(err) => {
                summary.errors += 1;
                tracing::warn!(path = %path, error = %err, "Skipping unreadable workspace hook file");
                continue;
            }
        };

        let parsed: serde_json::Value = match serde_json::from_str(&doc.content) {
            Ok(value) => value,
            Err(err) => {
                summary.errors += 1;
                tracing::warn!(path = %path, error = %err, "Workspace hook file is not valid JSON");
                continue;
            }
        };

        let bundle = match parse_workspace_bundle(&parsed) {
            Ok(bundle) => bundle,
            Err(err) => {
                summary.errors += 1;
                tracing::warn!(path = %path, error = %err, "Skipping invalid workspace hook bundle");
                continue;
            }
        };

        let source = format!("workspace:{}", path);
        let registered = register_bundle(registry, &source, bundle).await;
        summary.merge(registered);
    }

    summary
}

fn scope_coordination_bundle(bundle: &mut HookBundleConfig, config: &CoordinationHookConfig) {
    let guard = coordination_channel_guard(&config.coordination_channels);
    for rule in &mut bundle.rules {
        rule.when_channel_regex = Some(guard.clone());
    }
}

fn parse_workspace_bundle(value: &serde_json::Value) -> Result<HookBundleConfig, String> {
    if let Some(nested) = value.get("hooks") {
        HookBundleConfig::from_value(nested).map_err(|e| e.to_string())
    } else {
        HookBundleConfig::from_value(value).map_err(|e| e.to_string())
    }
}

fn is_workspace_hook_file(path: &str) -> bool {
    path == "hooks/hooks.json" || (path.starts_with("hooks/") && path.ends_with(".hook.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_hooks_section_from_tool_caps() {
        let value = serde_json::json!({
            "http": {"allowlist": []},
            "hooks": {"rules": []}
        });

        let extracted = extract_hooks_section(&value).unwrap();
        assert!(extracted.get("rules").is_some());
    }

    #[test]
    fn test_extract_hooks_section_from_channel_caps() {
        let value = serde_json::json!({
            "type": "channel",
            "capabilities": {
                "hooks": {
                    "rules": []
                }
            }
        });

        let extracted = extract_hooks_section(&value).unwrap();
        assert!(extracted.get("rules").is_some());
    }

    #[test]
    fn test_workspace_hook_file_filter() {
        assert!(is_workspace_hook_file("hooks/hooks.json"));
        assert!(is_workspace_hook_file("hooks/redact.hook.json"));
        assert!(!is_workspace_hook_file("hooks/readme.md"));
        assert!(!is_workspace_hook_file("MEMORY.md"));
    }

    #[test]
    fn coordination_bundle_scope_is_exact_and_escaped() {
        let mut bundle = HookBundleConfig {
            rules: vec![crate::hooks::bundled::HookRuleConfig {
                name: "coord-test".into(),
                points: vec![crate::hooks::HookPoint::BeforeOutbound],
                priority: None,
                failure_mode: None,
                timeout_ms: None,
                when_regex: None,
                when_channel_regex: Some("conference\\.".into()),
                reject_reason: Some("blocked".into()),
                replacements: Vec::new(),
                prepend: None,
                append: None,
            }],
            outbound_webhooks: Vec::new(),
        };
        let config = CoordinationHookConfig {
            enabled: true,
            agent_id: "agent-1".into(),
            coordination_channels: vec![
                "ops+one@conference.example".into(),
                "room.two@conference.example".into(),
                "Ä-team@conference.example".into(),
            ],
            ..CoordinationHookConfig::default()
        };

        scope_coordination_bundle(&mut bundle, &config);
        let guard = regex::Regex::new(bundle.rules[0].when_channel_regex.as_deref().unwrap())
            .expect("generated scope must compile");
        assert!(guard.is_match("OPS+ONE@CONFERENCE.EXAMPLE"));
        assert!(guard.is_match("room.two@conference.example"));
        assert!(guard.is_match("ä-team@conference.example"));
        assert!(!guard.is_match("other@conference.example"));
        assert!(!guard.is_match("xops+one@conference.example"));
    }

    #[tokio::test]
    async fn managed_coordination_bundle_registers_only_when_actionable() {
        let registry = Arc::new(HookRegistry::new());
        let disabled =
            register_managed_coordination_bundle(&registry, &CoordinationHookConfig::default())
                .await;
        assert_eq!(disabled.hooks, 0);
        assert!(registry.list().await.is_empty());

        let config = CoordinationHookConfig {
            enabled: true,
            agent_id: "agent-1".into(),
            coordination_channels: vec!["coord@conference.example".into()],
            ..CoordinationHookConfig::default()
        };
        let enabled = register_managed_coordination_bundle(&registry, &config).await;
        assert_eq!(enabled.hooks, 7);
        assert_eq!(enabled.errors, 0);
        assert_eq!(registry.list().await.len(), 7);
    }

    #[tokio::test]
    async fn managed_coordination_rules_are_exactly_room_scoped() {
        let registry = Arc::new(HookRegistry::new());
        let config = CoordinationHookConfig {
            enabled: true,
            agent_id: "agent-1".into(),
            coordination_channels: vec!["coord@conference.example".into()],
            ..CoordinationHookConfig::default()
        };
        register_managed_coordination_bundle(&registry, &config).await;
        let event = |scope: &str| crate::hooks::HookEvent::Inbound {
            user_id: "remote".into(),
            sender_id: Some("remote".into()),
            channel: "xmpp".into(),
            content: "STATUS: token=abcdefghijklmnop".into(),
            thread_id: None,
            policy_scope: Some(scope.into()),
        };
        assert!(
            registry
                .run(&event("coord@conference.example"))
                .await
                .is_err()
        );
        assert!(
            registry
                .run(&event("casual@conference.example"))
                .await
                .is_ok()
        );
    }

    #[test]
    fn test_parse_workspace_bundle_wrapped_hooks() {
        let value = serde_json::json!({
            "hooks": {
                "rules": [
                    {
                        "name": "append-bang",
                        "points": ["beforeInbound"],
                        "append": "!"
                    }
                ]
            }
        });

        let bundle = parse_workspace_bundle(&value).unwrap();
        assert_eq!(bundle.rules.len(), 1);
    }
}
