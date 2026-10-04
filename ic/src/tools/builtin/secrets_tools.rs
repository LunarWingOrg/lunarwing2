//! Agent-callable tools for inspecting user secrets.
//!
//! These tools allow the LLM to query and manage secrets on behalf of the
//! user. The zero-exposure model is preserved throughout:
//!
//! - `secret_list` returns only names and metadata (no values).
//! - `secret_delete` removes a secret by name.
//!
//! Storing secrets is handled via the extensions setup flow — the user types
//! values directly into the secure UI, which submits them to
//! `/api/extensions/{name}/setup`. Values never appear in the LLM conversation,
//! logs, or ActionRecords.
//!
//! Approval posture: stored secrets are UI-entered and unrecoverable, so
//! `secret_delete` requires `ApprovalRequirement::Always` — global and session
//! auto-approve never bypass its prompt. Both tools are excluded from
//! autonomous execution via `allows_autonomous_execution()`, with
//! `AUTONOMOUS_TOOL_DENYLIST` retained as name-based defense-in-depth.

use std::sync::Arc;

use async_trait::async_trait;

use crate::context::JobContext;
use crate::secrets::SecretsStore;
use crate::tools::tool::{ApprovalRequirement, Tool, ToolError, ToolOutput, require_str};

// ── secret_list ──────────────────────────────────────────────────────────────

pub struct SecretListTool {
    store: Arc<dyn SecretsStore + Send + Sync>,
}

impl SecretListTool {
    pub fn new(store: Arc<dyn SecretsStore + Send + Sync>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for SecretListTool {
    fn name(&self) -> &str {
        "secret_list"
    }

    fn description(&self) -> &str {
        "List all stored secrets by name. Never returns values — only names and \
         optional provider metadata. Use this to check what credentials are available \
         before attempting a task that requires them."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(
        &self,
        _params: serde_json::Value,
        ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();

        let refs = self
            .store
            .list(&ctx.user_id)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let secrets: Vec<serde_json::Value> = refs
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name,
                    "provider": r.provider,
                })
            })
            .collect();

        let count = secrets.len();
        let output = serde_json::json!({
            "secrets": secrets,
            "count": count,
        });

        Ok(ToolOutput::success(output, start.elapsed()))
    }

    fn allows_autonomous_execution(&self) -> bool {
        // Credential metadata stays out of autonomous jobs/routines;
        // AUTONOMOUS_TOOL_DENYLIST remains as name-based defense-in-depth.
        false
    }
}

// ── secret_delete ─────────────────────────────────────────────────────────────

pub struct SecretDeleteTool {
    store: Arc<dyn SecretsStore + Send + Sync>,
}

impl SecretDeleteTool {
    pub fn new(store: Arc<dyn SecretsStore + Send + Sync>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for SecretDeleteTool {
    fn name(&self) -> &str {
        "secret_delete"
    }

    fn description(&self) -> &str {
        "Permanently delete a stored secret by name. This cannot be undone."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Name of the secret to delete."
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

        let deleted = self
            .store
            .delete(&ctx.user_id, name)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let output = if deleted {
            serde_json::json!({
                "status": "deleted",
                "name": name,
            })
        } else {
            serde_json::json!({
                "status": "not_found",
                "name": name,
                "message": format!("No secret named '{}' found.", name),
            })
        };

        Ok(ToolOutput::success(output, start.elapsed()))
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        // Deletion is irreversible — values were UI-entered and exist nowhere
        // else. Keep global auto-approve and autonomous execution blocked, so
        // a prompt-injected turn cannot silently wipe stored credentials.
        // Explicit user persistence is controlled separately below.
        ApprovalRequirement::Always
    }

    fn allows_always_approval(&self, _params: &serde_json::Value) -> bool {
        // Only an explicit per-user "always allow" grant persists; global
        // auto-approve and autonomous execution remain blocked regardless.
        true
    }

    fn allows_autonomous_execution(&self) -> bool {
        // Irreversible credential destruction is excluded from autonomous
        // jobs/routines; AUTONOMOUS_TOOL_DENYLIST remains as name-based
        // defense-in-depth.
        false
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::context::JobContext;
    use crate::secrets::CreateSecretParams;
    use crate::testing::credentials::{TEST_OPENAI_API_KEY_SHORT, test_secrets_store};

    fn test_store() -> Arc<crate::secrets::InMemorySecretsStore> {
        Arc::new(test_secrets_store())
    }

    fn test_ctx() -> JobContext {
        JobContext::new("test", "test job")
    }

    #[tokio::test]
    async fn test_secret_list() {
        let store = test_store();
        let list = SecretListTool::new(Arc::clone(&store) as Arc<dyn SecretsStore + Send + Sync>);
        let ctx = test_ctx();

        store
            .create(
                &ctx.user_id,
                CreateSecretParams::new("openai_key", TEST_OPENAI_API_KEY_SHORT),
            )
            .await
            .unwrap();

        let list_result = list.execute(serde_json::json!({}), &ctx).await.unwrap();
        assert_eq!(list_result.result["count"], 1);
        assert_eq!(list_result.result["secrets"][0]["name"], "openai_key");
        assert!(list_result.result["secrets"][0].get("value").is_none());
    }

    #[tokio::test]
    async fn test_secret_delete() {
        let store = test_store();
        let delete =
            SecretDeleteTool::new(Arc::clone(&store) as Arc<dyn SecretsStore + Send + Sync>);
        let ctx = test_ctx();

        store
            .create(&ctx.user_id, CreateSecretParams::new("to_delete", "secret"))
            .await
            .unwrap();

        let result = delete
            .execute(serde_json::json!({"name": "to_delete"}), &ctx)
            .await
            .unwrap();
        assert_eq!(result.result["status"], "deleted");

        // Deleting again returns not_found
        let result2 = delete
            .execute(serde_json::json!({"name": "to_delete"}), &ctx)
            .await
            .unwrap();
        assert_eq!(result2.result["status"], "not_found");
    }

    #[test]
    fn secret_delete_always_requires_approval_even_under_auto_approve() {
        let delete = SecretDeleteTool::new(test_store());
        let params = serde_json::json!({"name": "openai_key"});

        let requirement = delete.requires_approval(&params);
        assert_eq!(requirement, ApprovalRequirement::Always);
        // Neither global nor session auto-approve may bypass the prompt.
        assert!(requirement.needs_prompt(true, true));
    }

    #[test]
    fn secret_delete_honors_only_explicit_user_always_grant() {
        let delete = SecretDeleteTool::new(test_store());
        let params = serde_json::json!({"name": "openai_key"});

        // The tool opts in to persistent grants (mirrors the built-in ssh
        // tool): an explicit per-user grant skips the prompt, while global
        // auto-approve alone never does.
        assert!(delete.allows_always_approval(&params));
        let requirement = delete.requires_approval(&params);
        assert!(!requirement.needs_prompt_with_user_grant(true, true, true));
        assert!(requirement.needs_prompt_with_user_grant(true, false, true));
    }

    #[test]
    fn secrets_family_is_excluded_from_autonomous_execution() {
        let store = test_store();
        let list = SecretListTool::new(Arc::clone(&store) as Arc<dyn SecretsStore + Send + Sync>);
        let delete = SecretDeleteTool::new(store);

        assert!(!list.allows_autonomous_execution());
        assert!(!delete.allows_autonomous_execution());
        // Both names stay on the name-based denylist as defense-in-depth.
        assert!(crate::tools::autonomy::is_autonomous_tool_denylisted(
            "secret_list"
        ));
        assert!(crate::tools::autonomy::is_autonomous_tool_denylisted(
            "secret_delete"
        ));
    }

    #[test]
    fn secret_list_remains_interactive_readonly() {
        let list = SecretListTool::new(test_store());

        // Read-only metadata listing keeps its Never tier; only its
        // autonomous-execution gate changed.
        assert_eq!(
            list.requires_approval(&serde_json::json!({})),
            ApprovalRequirement::Never
        );
    }
}
