//! Effect executor trait.
//!
//! The engine delegates actual action execution to the host through this
//! trait. The main crate implements it by wrapping `ToolRegistry` and
//! `SafetyLayer` — the engine itself has no knowledge of specific tools.

use std::sync::Arc;

use lunarwing_common::{
    HARD_PROMPT_CONTEXT_LIMITS, PromptContextMessage, PromptContextRole, select_prompt_context_by,
};

use crate::types::capability::{ActionDef, CapabilityLease};
use crate::types::error::EngineError;
use crate::types::message::{MessageRole, ThreadMessage};
use crate::types::project::ProjectId;
use crate::types::step::{ActionResult, StepId};
use crate::types::thread::{ThreadId, ThreadType};

/// Contextual information about the thread requesting an effect.
///
/// Passed to the executor so it can make context-dependent decisions
/// (e.g. different tool behavior in background vs foreground threads).
#[derive(Debug, Clone)]
pub struct ThreadExecutionContext {
    pub thread_id: ThreadId,
    pub thread_type: ThreadType,
    pub project_id: ProjectId,
    pub user_id: String,
    pub step_id: StepId,
    pub current_call_id: Option<String>,
    /// The channel this thread's conversation originated from (e.g. "gateway", "repl").
    /// Used by mission_create to default `notify_channels` to the current channel.
    pub source_channel: Option<String>,
    /// Human delay / supervision mode — when true, every action is gated
    /// through human approval regardless of tool tier.
    pub supervised_mode: bool,
    /// Whether multiple channel actors share this conversation scope.
    pub shared_conversation: bool,
    /// Timeout in seconds for supervised approval prompts.
    /// Default: 300 (5 minutes).
    pub supervised_timeout_secs: u64,
    /// Bounded visible conversation snapshot for delegated worker tools.
    pub prompt_context: Arc<[PromptContextMessage]>,
}

/// Convert visible thread messages into hard-bounded delegated-worker context.
pub fn prompt_context_from_thread_messages(
    messages: &[ThreadMessage],
) -> Arc<[PromptContextMessage]> {
    Arc::from(select_prompt_context_by(
        messages,
        HARD_PROMPT_CONTEXT_LIMITS,
        |message| {
            let role = match message.role {
                MessageRole::User => PromptContextRole::User,
                MessageRole::Assistant if message.action_calls.is_none() => {
                    PromptContextRole::Assistant
                }
                // Sealed machine step records are inference-only data
                // (WI#227): never delegate them to a worker prompt.
                MessageRole::Assistant
                | MessageRole::System
                | MessageRole::ActionResult
                | MessageRole::SealedStepRecord => return None,
            };
            Some((role, message.content.as_str()))
        },
    ))
}

/// Abstraction over capability action execution.
///
/// The main crate implements this by wrapping its `ToolRegistry`, `SafetyLayer`,
/// and tool execution pipeline. The engine calls `execute_action` and gets back
/// a result — all safety, sanitization, and actual tool invocation happens in
/// the host.
#[async_trait::async_trait]
pub trait EffectExecutor: Send + Sync {
    /// Execute a capability action.
    ///
    /// The executor is responsible for:
    /// 1. Looking up the actual tool implementation
    /// 2. Validating parameters
    /// 3. Applying safety checks (sanitization, leak detection)
    /// 4. Executing the tool
    /// 5. Returning the result
    async fn execute_action(
        &self,
        action_name: &str,
        parameters: serde_json::Value,
        lease: &CapabilityLease,
        context: &ThreadExecutionContext,
    ) -> Result<ActionResult, EngineError>;

    /// List available actions given the current set of active leases.
    ///
    /// Used to build the action definitions sent to the LLM. Implementations
    /// must not return actions that none of the supplied leases covers.
    async fn available_actions(
        &self,
        leases: &[CapabilityLease],
    ) -> Result<Vec<ActionDef>, EngineError>;

    /// List every action currently registered with the executor.
    ///
    /// This unscoped catalog provisions leases for newly registered actions and
    /// restores live action leases when persisted threads resume after restart.
    /// Include executable host capabilities as well as registry tools.
    /// Execution prompts must use
    /// [`EffectExecutor::available_actions`] instead.
    async fn registered_actions(&self) -> Result<Vec<ActionDef>, EngineError> {
        self.available_actions(&[]).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_prompt_context_excludes_system_and_action_results() {
        let mut action_message = ThreadMessage::assistant("private action reasoning");
        action_message.action_calls = Some(Vec::new());
        let messages = [
            ThreadMessage::system("secret system prompt"),
            ThreadMessage::user("question"),
            action_message,
            ThreadMessage::action_result("call", "tool", "secret tool output"),
            ThreadMessage::assistant("answer"),
        ];

        assert_eq!(
            prompt_context_from_thread_messages(&messages).as_ref(),
            [
                PromptContextMessage {
                    role: PromptContextRole::User,
                    content: "question".to_string(),
                },
                PromptContextMessage {
                    role: PromptContextRole::Assistant,
                    content: "answer".to_string(),
                },
            ]
        );
    }
}
