//! Tool for retrieving durable cross-channel attachments.
//!
//! Attachments are persisted into the workspace under `attachments/` by the
//! agent pipeline (see `crate::agent::attachment_store`). This tool lets the
//! agent list and read them from any channel, so a file sent on one channel
//! stays retrievable from all other channels and the WebUI.

use std::sync::Arc;

use async_trait::async_trait;

use crate::agent::attachment_store::ATTACHMENT_DIR;
use crate::context::JobContext;
use crate::tools::builtin::memory::{
    FixedWorkspaceResolver, WorkspaceResolver, normalize_memory_tool_path,
};
use crate::tools::tool::{Tool, ToolError, ToolOutput, require_str};
use crate::workspace::Workspace;

/// Maximum number of attachment entries returned by a single list call.
const MAX_ATTACHMENT_LIST_ENTRIES: usize = 100;

/// Maximum bytes of attachment content returned by a single read call.
const MAX_ATTACHMENT_READ_BYTES: usize = 100_000;

/// Validate that a tool-supplied path stays inside the attachment directory.
fn normalize_attachment_path(path: &str) -> Result<String, ToolError> {
    let normalized = normalize_memory_tool_path(path)?;
    if normalized != ATTACHMENT_DIR && !normalized.starts_with(&format!("{ATTACHMENT_DIR}/")) {
        return Err(ToolError::NotAuthorized(format!(
            "path '{normalized}' is outside the attachment store ('{ATTACHMENT_DIR}/'); \
             use memory_read for other workspace paths"
        )));
    }
    Ok(normalized)
}

/// Flatten the attachment directory tree into a bounded list of paths.
async fn list_attachment_paths(workspace: &Arc<Workspace>) -> Result<Vec<String>, ToolError> {
    let mut pending = vec![ATTACHMENT_DIR.to_string()];
    let mut paths = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = workspace
            .list(&directory)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("List failed: {e}")))?;
        for entry in entries {
            if entry.is_directory {
                pending.push(entry.path);
            } else {
                paths.push(entry.path);
                if paths.len() >= MAX_ATTACHMENT_LIST_ENTRIES {
                    return Ok(paths);
                }
            }
        }
    }
    Ok(paths)
}

/// Tool for listing and reading durable cross-channel attachments.
pub struct AttachmentStoreTool {
    resolver: Arc<dyn WorkspaceResolver>,
}

impl AttachmentStoreTool {
    /// Create a new attachment store tool with a workspace resolver.
    pub fn new(resolver: Arc<dyn WorkspaceResolver>) -> Self {
        Self { resolver }
    }

    /// Create from a fixed workspace (backward compatibility).
    pub fn from_workspace(workspace: Arc<Workspace>) -> Self {
        Self {
            resolver: Arc::new(FixedWorkspaceResolver::new(workspace)),
        }
    }
}

#[async_trait]
impl Tool for AttachmentStoreTool {
    fn name(&self) -> &str {
        "attachment_store"
    }

    fn description(&self) -> &str {
        "List or read durable cross-channel attachments. Files sent on ANY channel \
         (XMPP, Signal, DarkIRC, WebUI, ...) are persisted into the shared attachment \
         store and stay retrievable from every other channel. Use 'action: list' to \
         enumerate stored attachments, then 'action: read' with a path to fetch one. \
         Paths always start with 'attachments/'. NOT for local filesystem files."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "read"],
                    "description": "Operation to perform: 'list' enumerates stored attachments, 'read' fetches a single attachment document."
                },
                "path": {
                    "type": "string",
                    "description": "Attachment path to read (required for action 'read'), e.g. 'attachments/xmpp/file-123/report.pdf.md'."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();
        let action = require_str(&params, "action")?;
        let workspace = self.resolver.resolve(&ctx.user_id).await;

        match action {
            "list" => {
                let paths = list_attachment_paths(&workspace).await?;
                let output = serde_json::json!({
                    "action": "list",
                    "count": paths.len(),
                    "truncated": paths.len() >= MAX_ATTACHMENT_LIST_ENTRIES,
                    "paths": paths,
                });
                Ok(ToolOutput::success(output, start.elapsed()))
            }
            "read" => {
                let path = require_str(&params, "path")?;
                let path = normalize_attachment_path(path)?;
                let doc = workspace
                    .read(&path)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("Read failed: {e}")))?;
                let content = truncate_utf8(&doc.content, MAX_ATTACHMENT_READ_BYTES);
                let output = serde_json::json!({
                    "action": "read",
                    "path": path,
                    "content": content,
                    "truncated": doc.content.len() > MAX_ATTACHMENT_READ_BYTES,
                });
                Ok(ToolOutput::success(output, start.elapsed()))
            }
            other => Err(ToolError::InvalidParameters(format!(
                "unknown action '{other}'; expected 'list' or 'read'"
            ))),
        }
    }

    fn requires_sanitization(&self) -> bool {
        false // Internal workspace memory, trusted content
    }
}

/// Truncate a string to at most `max_bytes` without splitting UTF-8 code points.
fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    let end = crate::util::floor_char_boundary(s, max_bytes);
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::normalize_path;

    #[test]
    fn normalize_attachment_path_rejects_outside_paths() {
        assert_eq!(
            normalize_attachment_path("attachments").unwrap(),
            "attachments"
        );
        assert_eq!(
            normalize_attachment_path("attachments/xmpp/file-1/report.pdf.md").unwrap(),
            "attachments/xmpp/file-1/report.pdf.md"
        );
        assert!(normalize_attachment_path("MEMORY.md").is_err());
        // `..` segments clamp at the workspace root, so this stays inside the store.
        assert_eq!(
            normalize_attachment_path("../attachments/x").unwrap(),
            "attachments/x"
        );
        assert!(normalize_attachment_path("attachments/../MEMORY.md").is_err());
    }

    #[test]
    fn normalize_attachment_path_rejects_engine_runtime_paths() {
        assert!(normalize_attachment_path("engine/.runtime/x").is_err());
    }

    #[test]
    fn truncate_utf8_preserves_code_points() {
        assert_eq!(truncate_utf8("hello", 100), "hello");
        assert_eq!(truncate_utf8("héllo", 3), "hé");
        assert_eq!(truncate_utf8("héllo", 2), "h");
    }

    #[test]
    fn path_normalization_matches_workspace_semantics() {
        assert_eq!(
            normalize_path("attachments//xmpp/./f"),
            "attachments/xmpp/f"
        );
        assert_eq!(normalize_path("attachments/x/../y"), "attachments/y");
    }
}
