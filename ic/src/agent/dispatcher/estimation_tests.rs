//! Legacy chat coverage for both initial and deferred approval hints.

use std::sync::Arc;
use std::time::Duration;

use rust_decimal::Decimal;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::tests::make_test_agent_with_runtime;
use super::{AgenticLoopResult, Session};
use crate::agent::submission::SubmissionResult;
use crate::channels::IncomingMessage;
use crate::context::JobContext;
use crate::estimation::ToolEstimationMode;
use crate::llm::{
    ChatMessage, CompletionRequest, CompletionResponse, FinishReason, LlmProvider, Role, ToolCall,
    ToolCompletionRequest, ToolCompletionResponse,
};
use crate::tools::{ApprovalRequirement, Tool, ToolError, ToolOutput, ToolRegistry};

struct TimedTool;

#[async_trait::async_trait]
impl Tool for TimedTool {
    fn name(&self) -> &str {
        "v1_timed_tool"
    }
    fn description(&self) -> &str {
        "A timed operation"
    }
    fn parameters_schema(&self) -> Value {
        json!({"type":"object"})
    }
    fn estimated_duration(&self, _: &Value) -> Option<Duration> {
        Some(Duration::from_secs(1))
    }
    fn sensitive_params(&self) -> &[&str] {
        &["token"]
    }
    fn requires_approval(&self, params: &Value) -> ApprovalRequirement {
        if params["gate"] == true {
            ApprovalRequirement::Always
        } else {
            ApprovalRequirement::Never
        }
    }
    async fn execute(&self, _: Value, _: &JobContext) -> Result<ToolOutput, ToolError> {
        tokio::time::sleep(Duration::from_secs(2)).await;
        Ok(ToolOutput::success(json!({"ok":true}), Duration::ZERO))
    }
}

struct ToolLlm;

#[async_trait::async_trait]
impl LlmProvider for ToolLlm {
    fn model_name(&self) -> &str {
        "v1-estimation-fixture"
    }
    fn cost_per_token(&self) -> (Decimal, Decimal) {
        (Decimal::ZERO, Decimal::ZERO)
    }
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, crate::error::LlmError> {
        crate::testing::StubLlm::new("Finished.")
            .complete(request)
            .await
    }
    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, crate::error::LlmError> {
        let mut calls = Vec::new();
        if !request
            .messages
            .iter()
            .any(|message| message.role == Role::Tool)
        {
            let gated = request
                .messages
                .iter()
                .any(|message| message.role == Role::User && message.content == "approve");
            for id in 0..if gated { 2 } else { 1 } {
                calls.push(ToolCall {
                    id: format!("call-{id}"),
                    name: "v1_timed_tool".into(),
                    arguments: json!({"gate":gated,"token":"must-not-leak"}),
                    reasoning: None,
                });
            }
        }
        Ok(ToolCompletionResponse {
            finish_reason: if calls.is_empty() {
                FinishReason::Stop
            } else {
                FinishReason::ToolUse
            },
            content: calls.is_empty().then(|| "Finished.".into()),
            tool_calls: calls,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        })
    }
}

async fn run_turn(
    agent: &crate::agent::Agent,
    message: &IncomingMessage,
    session: &Arc<Mutex<Session>>,
    thread_id: uuid::Uuid,
) -> AgenticLoopResult {
    session
        .lock()
        .await
        .threads
        .get_mut(&thread_id)
        .unwrap()
        .start_turn(&message.content);
    agent
        .run_agentic_loop(
            message,
            session.clone(),
            thread_id,
            vec![ChatMessage::user(&message.content)],
        )
        .await
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn v1_estimation_trains_for_message_user_and_survives_deferred_approval() {
    let tools = Arc::new(ToolRegistry::new().with_estimation_mode(ToolEstimationMode::Display));
    tools.register(Arc::new(TimedTool)).await;
    let agent = make_test_agent_with_runtime(tools.clone(), Arc::new(ToolLlm));
    let mut message = IncomingMessage::new("test", "alice", "warmup");
    message.sender_id = "upstream-sender".into();
    let mut session = Session::new(&message.user_id);
    let thread_id = session.create_thread().id;
    let session = Arc::new(Mutex::new(session));
    for _ in 0..5 {
        let result = run_turn(&agent, &message, &session, thread_id).await;
        let AgenticLoopResult::Response(response) = result else {
            panic!("warmup should execute without approval")
        };
        session
            .lock()
            .await
            .threads
            .get_mut(&thread_id)
            .unwrap()
            .complete_turn(response);
    }
    let estimate = tools
        .estimates()
        .estimate(&message.user_id, &TimedTool, &json!({}))
        .unwrap();
    assert_eq!(estimate.samples, 5);
    assert_eq!(estimate.duration, Duration::from_secs(2));
    for other in ["default", "bob", "upstream-sender"] {
        assert!(
            tools
                .estimates()
                .estimate(other, &TimedTool, &json!({}))
                .is_none()
        );
    }
    message.content = "approve".into();
    let result = run_turn(&agent, &message, &session, thread_id).await;
    let AgenticLoopResult::NeedApproval { pending } = result else {
        panic!("expected approval")
    };
    assert!(pending.description.contains("~2.00s"));
    assert!(pending.description.contains("5 completed calls"));
    assert_eq!(pending.deferred_tool_calls.len(), 1);
    let first_description = pending.description.clone();
    let request_id = pending.request_id;
    session
        .lock()
        .await
        .threads
        .get_mut(&thread_id)
        .unwrap()
        .await_approval(*pending);
    let next = agent
        .process_approval(
            &message,
            session.clone(),
            thread_id,
            Some(request_id),
            true,
            false,
        )
        .await
        .unwrap();
    let SubmissionResult::NeedApproval {
        description,
        parameters,
        ..
    } = next
    else {
        panic!("expected deferred approval")
    };
    assert!(description.contains("~2.00s"));
    assert!(description.contains("6 completed calls"));
    assert_eq!(description.matches("Typical tool runtime:").count(), 1);
    assert!(!parameters.to_string().contains("must-not-leak"));
    assert!(first_description.contains("5 completed calls"));
    let session = session.lock().await;
    let view = crate::channels::web::types::PendingApprovalInfo::from(
        session.threads[&thread_id]
            .pending_approval
            .as_ref()
            .unwrap(),
    );
    assert_eq!(view.description, description);
    assert!(!view.parameters.contains("must-not-leak"));
}
