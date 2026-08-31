//! Unified agentic loop engine.
//!
//! Provides a single implementation of the core LLM call → tool execution →
//! result processing → context update → repeat cycle. Three consumers
//! (chat dispatcher, job worker, container runtime) customize behavior
//! via the `LoopDelegate` trait.

use async_trait::async_trait;
use std::borrow::Cow;
use std::future::Future;

use tokio_util::sync::CancellationToken;

use crate::agent::session::PendingApproval;
use crate::error::Error;
use crate::llm::{ChatMessage, Reasoning, ReasoningContext, RespondResult};

/// Signal from the delegate indicating how the loop should proceed.
pub enum LoopSignal {
    /// Continue normally.
    Continue,
    /// Stop the loop gracefully.
    Stop,
    /// Inject a user message into context and continue.
    InjectMessage(String),
}

/// Outcome of a text response from the LLM.
pub enum TextAction {
    /// Return this as the final loop result.
    Return(LoopOutcome),
    /// Continue the loop (text was handled but loop should proceed).
    Continue,
}

/// Final outcome of the agentic loop.
pub enum LoopOutcome {
    /// Completed with a text response.
    Response(String),
    /// Loop was stopped by a signal.
    Stopped,
    /// Max iterations exceeded after the final recovery turn.
    MaxIterations,
    /// A provider failure could not be recovered; contains safe next steps.
    Failed(String),
    /// A tool requires user approval before continuing (chat delegate only).
    NeedApproval(Box<PendingApproval>),
}

const ITERATION_RECOVERY_INSTRUCTION: &str = "Loop warning: the iteration limit was reached. This is the final recovery turn. \
     Do not call more tools; provide the best final answer now, state any incomplete work, \
     and name the narrower scope or higher iteration limit needed to continue.";

/// Configuration for the agentic loop.
pub struct AgenticLoopConfig {
    pub max_iterations: usize,
    pub enable_tool_intent_nudge: bool,
    pub max_tool_intent_nudges: u32,
    /// Optional mid-turn interrupt token. When set, cancelling it aborts an
    /// in-flight LLM call or an in-flight tool batch immediately (the step's
    /// future is dropped, aborting the underlying LLM stream or tool tasks)
    /// and the loop returns [`LoopOutcome::Stopped`] instead of waiting for
    /// the step to finish. `None` preserves turn-boundary-only interruption.
    pub interrupt_token: Option<CancellationToken>,
}

impl Default for AgenticLoopConfig {
    fn default() -> Self {
        Self {
            max_iterations: 50,
            enable_tool_intent_nudge: true,
            max_tool_intent_nudges: 2,
            interrupt_token: None,
        }
    }
}

/// Strategy trait — each consumer implements this to customize I/O and lifecycle.
///
/// The shared loop calls these methods at well-defined points. Consumers
/// implement only the behavior that differs between chat, job, and container
/// contexts. The loop itself handles the common logic: tool intent nudge,
/// iteration counting, tool definition refresh, and the respond → execute → process cycle.
///
/// # `Send + Sync` requirement
///
/// This trait requires `Send + Sync` because the loop accepts `&dyn LoopDelegate`.
/// Delegates using borrowed references (e.g. `ChatDelegate<'a>`) must ensure all
/// borrowed fields are `Send + Sync`. This is a load-bearing constraint: if a
/// delegate needs to be spawned into a detached task, it must use `Arc`-based
/// ownership instead of borrows (as `JobDelegate` and `ContainerDelegate` do).
#[async_trait]
pub trait LoopDelegate: Send + Sync {
    /// Called at the start of each iteration. Check for external signals
    /// (cancellation, user messages, stop requests).
    async fn check_signals(&self) -> LoopSignal;

    /// Called before the LLM call. Allows the delegate to refresh tool
    /// definitions, enforce cost guards, or inject messages.
    /// Return `Some(outcome)` to break the loop early.
    async fn before_llm_call(
        &self,
        reason_ctx: &mut ReasoningContext,
        iteration: usize,
    ) -> Option<LoopOutcome>;

    /// Call the LLM and return the result. Delegates own the LLM call
    /// to handle consumer-specific concerns (rate limiting, auto-compaction,
    /// cost tracking, force_text mode).
    async fn call_llm(
        &self,
        reasoning: &Reasoning,
        reason_ctx: &mut ReasoningContext,
        iteration: usize,
    ) -> Result<crate::llm::RespondOutput, Error>;

    /// Handle a text-only response from the LLM.
    /// Return `TextAction::Return` to exit the loop, `TextAction::Continue` to proceed.
    async fn handle_text_response(
        &self,
        text: &str,
        reason_ctx: &mut ReasoningContext,
    ) -> TextAction;

    /// Execute tool calls and add results to context.
    /// Return `Some(outcome)` to break the loop (e.g. approval needed).
    async fn execute_tool_calls(
        &self,
        tool_calls: Vec<crate::llm::ToolCall>,
        content: Option<String>,
        reason_ctx: &mut ReasoningContext,
    ) -> Result<Option<LoopOutcome>, Error>;

    /// Called when the LLM expresses tool intent without actually calling a tool.
    /// Delegates can use this to emit events or log the nudge for observability.
    async fn on_tool_intent_nudge(&self, _text: &str, _reason_ctx: &mut ReasoningContext) {}

    /// Called after each successful iteration (no error, no early return).
    async fn after_iteration(&self, _iteration: usize) {}
}

enum LlmCallOutcome {
    Output(crate::llm::RespondOutput),
    Retry,
    Failed(String),
}

async fn call_llm_with_recovery(
    delegate: &dyn LoopDelegate,
    reasoning: &Reasoning,
    reason_ctx: &mut ReasoningContext,
    iteration: usize,
    can_retry: bool,
    recovery_attempted: &mut bool,
) -> Result<LlmCallOutcome, Error> {
    match delegate.call_llm(reasoning, reason_ctx, iteration).await {
        Ok(output) => Ok(LlmCallOutcome::Output(output)),
        Err(Error::Llm(error)) => {
            let kind = error.recovery_kind();
            if can_retry
                && !*recovery_attempted
                && let Some(instruction) = error.recovery_instruction()
            {
                *recovery_attempted = true;
                tracing::warn!(
                    iteration,
                    failure_kind = kind,
                    error = %error,
                    "LLM call failed; scheduling one model-visible recovery turn"
                );
                reason_ctx.messages.push(ChatMessage::system(instruction));
                return Ok(LlmCallOutcome::Retry);
            }

            tracing::error!(
                iteration,
                failure_kind = kind,
                error = %error,
                "LLM call failed without remaining recovery path"
            );
            Ok(LlmCallOutcome::Failed(
                error.terminal_guidance().to_string(),
            ))
        }
        Err(error) => Err(error),
    }
}

/// Race a loop step against the mid-turn interrupt token.
///
/// Returns `None` when the token is cancelled, in which case the step's
/// future is dropped — aborting the in-flight LLM stream or tool batch
/// (dropping a `JoinSet`-based batch aborts its tool tasks). The token branch
/// is polled first so an already-cancelled token always wins over a step
/// that happens to be ready in the same poll.
async fn interruptible<T>(
    token: Option<&CancellationToken>,
    step: impl Future<Output = T>,
) -> Option<T> {
    match token {
        Some(token) => {
            tokio::select! {
                biased;
                _ = token.cancelled() => None,
                result = step => Some(result),
            }
        }
        None => Some(step.await),
    }
}

fn log_llm_output(iteration: usize, result: &RespondResult) {
    match result {
        RespondResult::Text(text) => {
            tracing::debug!(
                iteration,
                len = text.len(),
                has_suggestions = text.contains("<suggestions>"),
                response = %text,
                "LLM text response"
            );
        }
        RespondResult::ToolCalls {
            tool_calls,
            content,
        } => {
            let names: Vec<&str> = tool_calls.iter().map(|tc| tc.name.as_str()).collect();
            tracing::debug!(
                iteration,
                tools = ?names,
                has_content = content.is_some(),
                "LLM tool_calls response"
            );
        }
    }
}

/// Run the unified agentic loop.
///
/// This is the single implementation used by all three consumers (chat, job, container).
/// The `delegate` provides consumer-specific behavior via the `LoopDelegate` trait.
pub async fn run_agentic_loop(
    delegate: &dyn LoopDelegate,
    reasoning: &Reasoning,
    reason_ctx: &mut ReasoningContext,
    config: &AgenticLoopConfig,
) -> Result<LoopOutcome, Error> {
    let mut consecutive_tool_intent_nudges: u32 = 0;
    let mut provider_recovery_attempted = false;
    let hard_max_iterations = config.max_iterations.saturating_add(1);

    for iteration in 1..=hard_max_iterations {
        // Mid-turn interrupt check at the iteration boundary. The in-flight
        // selects below cover the LLM call and the tool batch; this covers
        // any cancellation that landed between them.
        if config
            .interrupt_token
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            tracing::info!(iteration, "Turn interrupted before step start");
            return Ok(LoopOutcome::Stopped);
        }

        match delegate.check_signals().await {
            LoopSignal::Continue => {}
            LoopSignal::Stop => return Ok(LoopOutcome::Stopped),
            LoopSignal::InjectMessage(msg) => reason_ctx.messages.push(ChatMessage::user(&msg)),
        }

        if let Some(outcome) = delegate.before_llm_call(reason_ctx, iteration).await {
            return Ok(outcome);
        }

        if iteration > config.max_iterations {
            reason_ctx
                .messages
                .push(ChatMessage::system(ITERATION_RECOVERY_INSTRUCTION));
            reason_ctx.available_tools.clear();
            reason_ctx.force_text = true;
        }

        let llm_step = call_llm_with_recovery(
            delegate,
            reasoning,
            reason_ctx,
            iteration,
            iteration < hard_max_iterations,
            &mut provider_recovery_attempted,
        );
        // Race the LLM call against the mid-turn interrupt token: cancelling
        // aborts the in-flight stream instead of waiting for it to complete.
        let Some(llm_result) = interruptible(config.interrupt_token.as_ref(), llm_step).await
        else {
            tracing::info!(
                iteration,
                "In-flight LLM call aborted by mid-turn interrupt"
            );
            return Ok(LoopOutcome::Stopped);
        };
        let output = match llm_result? {
            LlmCallOutcome::Output(output) => output,
            LlmCallOutcome::Retry => continue,
            LlmCallOutcome::Failed(message) => return Ok(LoopOutcome::Failed(message)),
        };
        log_llm_output(iteration, &output.result);

        match output.result {
            RespondResult::Text(text) => {
                if config.enable_tool_intent_nudge
                    && !reason_ctx.available_tools.is_empty()
                    && !reason_ctx.force_text
                    && consecutive_tool_intent_nudges < config.max_tool_intent_nudges
                    && crate::llm::llm_signals_tool_intent(&text)
                {
                    consecutive_tool_intent_nudges += 1;
                    tracing::info!(
                        iteration,
                        "LLM expressed tool intent without calling a tool, nudging"
                    );
                    delegate.on_tool_intent_nudge(&text, reason_ctx).await;
                    reason_ctx.messages.push(ChatMessage::assistant(&text));
                    reason_ctx
                        .messages
                        .push(ChatMessage::user(crate::llm::TOOL_INTENT_NUDGE));
                    delegate.after_iteration(iteration).await;
                    continue;
                }

                if !crate::llm::llm_signals_tool_intent(&text) {
                    consecutive_tool_intent_nudges = 0;
                }

                match delegate.handle_text_response(&text, reason_ctx).await {
                    TextAction::Return(outcome) => return Ok(outcome),
                    TextAction::Continue => {}
                }
            }
            RespondResult::ToolCalls {
                tool_calls,
                content,
            } => {
                consecutive_tool_intent_nudges = 0;
                // Race the tool batch against the mid-turn interrupt token:
                // cancelling drops the batch future, aborting in-flight tool
                // execution instead of waiting for it to finish.
                let exec_step = delegate.execute_tool_calls(tool_calls, content, reason_ctx);
                let Some(exec_result) =
                    interruptible(config.interrupt_token.as_ref(), exec_step).await
                else {
                    tracing::info!(
                        iteration,
                        "In-flight tool batch aborted by mid-turn interrupt"
                    );
                    return Ok(LoopOutcome::Stopped);
                };
                if let Some(outcome) = exec_result? {
                    return Ok(outcome);
                }
            }
        }

        delegate.after_iteration(iteration).await;
    }

    Ok(LoopOutcome::MaxIterations)
}

/// Truncate a string for log/status previews.
///
/// `max` is a byte budget. The result is truncated at the last valid char
/// boundary at or before `max` bytes, so it is always valid UTF-8.
pub fn truncate_for_preview(s: &str, max: usize) -> Cow<'_, str> {
    if s.len() <= max {
        Cow::Borrowed(s)
    } else {
        let end = crate::util::floor_char_boundary(s, max);
        Cow::Owned(format!("{}...", &s[..end]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{LlmError, RespondOutput, TokenUsage, ToolCall, ToolDefinition};
    use crate::testing::StubLlm;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::sync::Mutex;

    fn stub_reasoning() -> Reasoning {
        Reasoning::new(Arc::new(StubLlm::default()))
    }

    fn zero_usage() -> TokenUsage {
        TokenUsage {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        }
    }

    fn text_output(text: &str) -> RespondOutput {
        RespondOutput {
            result: RespondResult::Text(text.to_string()),
            usage: zero_usage(),
        }
    }

    fn tool_calls_output(calls: Vec<ToolCall>) -> RespondOutput {
        RespondOutput {
            result: RespondResult::ToolCalls {
                tool_calls: calls,
                content: None,
            },
            usage: zero_usage(),
        }
    }

    /// Configurable mock delegate for testing run_agentic_loop.
    struct MockDelegate {
        signal: Mutex<LoopSignal>,
        /// Signal armed while a tool batch executes; `check_signals` picks it
        /// up at the next iteration, simulating a mid-loop steer/interrupt.
        signal_after_first_exec: Mutex<Option<LoopSignal>>,
        llm_responses: Mutex<Vec<Result<RespondOutput, Error>>>,
        tool_exec_count: AtomicUsize,
        tool_exec_outcome: Mutex<Option<LoopOutcome>>,
        iterations_seen: Mutex<Vec<usize>>,
        early_exit: Mutex<Option<(usize, LoopOutcome)>>,
        nudge_count: AtomicUsize,
    }

    impl MockDelegate {
        fn new(responses: Vec<RespondOutput>) -> Self {
            Self::with_llm_results(responses.into_iter().map(Ok).collect())
        }

        fn with_llm_results(responses: Vec<Result<RespondOutput, Error>>) -> Self {
            Self {
                signal: Mutex::new(LoopSignal::Continue),
                signal_after_first_exec: Mutex::new(None),
                llm_responses: Mutex::new(responses),
                tool_exec_count: AtomicUsize::new(0),
                tool_exec_outcome: Mutex::new(None),
                iterations_seen: Mutex::new(Vec::new()),
                early_exit: Mutex::new(None),
                nudge_count: AtomicUsize::new(0),
            }
        }

        fn with_signal(mut self, signal: LoopSignal) -> Self {
            self.signal = Mutex::new(signal);
            self
        }

        fn with_early_exit(mut self, iteration: usize, outcome: LoopOutcome) -> Self {
            self.early_exit = Mutex::new(Some((iteration, outcome)));
            self
        }
    }

    #[async_trait]
    impl LoopDelegate for MockDelegate {
        async fn check_signals(&self) -> LoopSignal {
            let mut sig = self.signal.lock().await;
            std::mem::replace(&mut *sig, LoopSignal::Continue)
        }

        async fn before_llm_call(
            &self,
            _reason_ctx: &mut ReasoningContext,
            iteration: usize,
        ) -> Option<LoopOutcome> {
            let mut guard = self.early_exit.lock().await;
            let should_take = guard
                .as_ref()
                .is_some_and(|(target, _)| *target == iteration);
            if should_take {
                guard.take().map(|(_, o)| o)
            } else {
                None
            }
        }

        async fn call_llm(
            &self,
            _reasoning: &Reasoning,
            _reason_ctx: &mut ReasoningContext,
            _iteration: usize,
        ) -> Result<crate::llm::RespondOutput, crate::error::Error> {
            let mut responses = self.llm_responses.lock().await;
            if responses.is_empty() {
                panic!("MockDelegate: no more LLM responses queued");
            }
            responses.remove(0)
        }

        async fn handle_text_response(
            &self,
            text: &str,
            _reason_ctx: &mut ReasoningContext,
        ) -> TextAction {
            TextAction::Return(LoopOutcome::Response(text.to_string()))
        }

        async fn execute_tool_calls(
            &self,
            _tool_calls: Vec<ToolCall>,
            _content: Option<String>,
            reason_ctx: &mut ReasoningContext,
        ) -> Result<Option<LoopOutcome>, crate::error::Error> {
            self.tool_exec_count.fetch_add(1, Ordering::SeqCst);
            reason_ctx
                .messages
                .push(ChatMessage::user("tool result stub"));
            if let Some(armed) = self.signal_after_first_exec.lock().await.take() {
                *self.signal.lock().await = armed;
            }
            let outcome = self.tool_exec_outcome.lock().await.take();
            Ok(outcome)
        }

        async fn on_tool_intent_nudge(&self, _text: &str, _reason_ctx: &mut ReasoningContext) {
            self.nudge_count.fetch_add(1, Ordering::SeqCst);
        }

        async fn after_iteration(&self, iteration: usize) {
            self.iterations_seen.lock().await.push(iteration);
        }
    }

    // --- Tests ---

    #[tokio::test]
    async fn test_text_response_returns_immediately() {
        let delegate = MockDelegate::new(vec![text_output("Hello, world!")]);
        let reasoning = stub_reasoning();
        let mut ctx = ReasoningContext::new();
        let config = AgenticLoopConfig::default();

        let outcome = run_agentic_loop(&delegate, &reasoning, &mut ctx, &config)
            .await
            .unwrap();

        match outcome {
            LoopOutcome::Response(text) => assert_eq!(text, "Hello, world!"),
            _ => panic!("Expected LoopOutcome::Response"),
        }
        // after_iteration is NOT called when handle_text_response returns Return
        // (the loop exits before reaching after_iteration).
        assert!(delegate.iterations_seen.lock().await.is_empty());
    }

    #[tokio::test]
    async fn test_tool_call_then_text_response() {
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "echo".to_string(),
            arguments: serde_json::json!({}),
            reasoning: None,
        };
        let delegate = MockDelegate::new(vec![
            tool_calls_output(vec![tool_call]),
            text_output("Done!"),
        ]);
        let reasoning = stub_reasoning();
        let mut ctx = ReasoningContext::new();
        let config = AgenticLoopConfig::default();

        let outcome = run_agentic_loop(&delegate, &reasoning, &mut ctx, &config)
            .await
            .unwrap();

        match outcome {
            LoopOutcome::Response(text) => assert_eq!(text, "Done!"),
            _ => panic!("Expected LoopOutcome::Response"),
        }
        assert_eq!(delegate.tool_exec_count.load(Ordering::SeqCst), 1);
        // after_iteration called for iteration 1 (tool call), but not 2
        // (text response exits before after_iteration).
        assert_eq!(*delegate.iterations_seen.lock().await, vec![1]);
    }

    #[tokio::test]
    async fn test_stop_signal_exits_immediately() {
        let delegate =
            MockDelegate::new(vec![text_output("unreachable")]).with_signal(LoopSignal::Stop);
        let reasoning = stub_reasoning();
        let mut ctx = ReasoningContext::new();
        let config = AgenticLoopConfig::default();

        let outcome = run_agentic_loop(&delegate, &reasoning, &mut ctx, &config)
            .await
            .unwrap();

        assert!(matches!(outcome, LoopOutcome::Stopped));
        assert!(delegate.iterations_seen.lock().await.is_empty());
    }

    #[tokio::test]
    async fn test_inject_message_adds_user_message() {
        let delegate = MockDelegate::new(vec![text_output("Got it")])
            .with_signal(LoopSignal::InjectMessage("injected prompt".to_string()));
        let reasoning = stub_reasoning();
        let mut ctx = ReasoningContext::new();
        let config = AgenticLoopConfig::default();

        let outcome = run_agentic_loop(&delegate, &reasoning, &mut ctx, &config)
            .await
            .unwrap();

        assert!(matches!(outcome, LoopOutcome::Response(_)));
        assert!(
            ctx.messages
                .iter()
                .any(|m| m.role == crate::llm::Role::User && m.content.contains("injected prompt")),
            "Injected message should appear in context"
        );
    }

    #[tokio::test]
    async fn test_max_iterations_gets_one_text_only_recovery_turn() {
        struct ContinueDelegate;

        #[async_trait]
        impl LoopDelegate for ContinueDelegate {
            async fn check_signals(&self) -> LoopSignal {
                LoopSignal::Continue
            }
            async fn before_llm_call(
                &self,
                _: &mut ReasoningContext,
                _: usize,
            ) -> Option<LoopOutcome> {
                None
            }
            async fn call_llm(
                &self,
                _: &Reasoning,
                _: &mut ReasoningContext,
                _: usize,
            ) -> Result<crate::llm::RespondOutput, crate::error::Error> {
                Ok(text_output("still working"))
            }
            async fn handle_text_response(
                &self,
                _: &str,
                ctx: &mut ReasoningContext,
            ) -> TextAction {
                ctx.messages.push(ChatMessage::assistant("still working"));
                TextAction::Continue
            }
            async fn execute_tool_calls(
                &self,
                _: Vec<ToolCall>,
                _: Option<String>,
                _: &mut ReasoningContext,
            ) -> Result<Option<LoopOutcome>, crate::error::Error> {
                Ok(None)
            }
        }

        let delegate = ContinueDelegate;
        let reasoning = stub_reasoning();
        let mut ctx = ReasoningContext::new();
        ctx.available_tools.push(ToolDefinition {
            name: "search".to_string(),
            description: "Search files".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        });
        let config = AgenticLoopConfig {
            max_iterations: 3,
            ..Default::default()
        };

        let outcome = run_agentic_loop(&delegate, &reasoning, &mut ctx, &config)
            .await
            .unwrap();

        assert!(matches!(outcome, LoopOutcome::MaxIterations));
        let assistant_count = ctx
            .messages
            .iter()
            .filter(|m| m.role == crate::llm::Role::Assistant)
            .count();
        assert_eq!(assistant_count, 4);
        assert!(ctx.force_text);
        assert!(ctx.available_tools.is_empty());
        assert!(ctx.messages.iter().any(|message| {
            message.role == crate::llm::Role::System
                && message.content.contains("final recovery turn")
                && message.content.contains("higher iteration limit")
        }));
    }

    #[tokio::test]
    async fn test_recoverable_provider_failure_gets_one_model_visible_retry() {
        let delegate = MockDelegate::with_llm_results(vec![
            Err(LlmError::RequestFailed {
                provider: "test-provider".to_string(),
                reason: "sensitive upstream diagnostic".to_string(),
            }
            .into()),
            Ok(text_output("Recovered safely")),
        ]);
        let reasoning = stub_reasoning();
        let mut ctx = ReasoningContext::new();
        let config = AgenticLoopConfig {
            max_iterations: 3,
            ..Default::default()
        };

        let outcome = run_agentic_loop(&delegate, &reasoning, &mut ctx, &config)
            .await
            .unwrap();

        assert!(matches!(outcome, LoopOutcome::Response(ref text) if text == "Recovered safely"));
        assert!(delegate.llm_responses.lock().await.is_empty());
        assert!(ctx.messages.iter().any(|message| {
            message.role == crate::llm::Role::System
                && message.content.contains("provider temporarily failed")
        }));
        assert!(
            ctx.messages
                .iter()
                .all(|message| !message.content.contains("sensitive upstream diagnostic"))
        );
    }

    #[tokio::test]
    async fn test_nonrecoverable_provider_failure_returns_actionable_next_steps() {
        let delegate = MockDelegate::with_llm_results(vec![
            Err(LlmError::AuthFailed {
                provider: "private-provider-name".to_string(),
            }
            .into()),
            Ok(text_output("unreachable")),
        ]);
        let reasoning = stub_reasoning();
        let mut ctx = ReasoningContext::new();
        let config = AgenticLoopConfig {
            max_iterations: 3,
            ..Default::default()
        };

        let outcome = run_agentic_loop(&delegate, &reasoning, &mut ctx, &config)
            .await
            .unwrap();

        match outcome {
            LoopOutcome::Failed(message) => {
                assert!(message.contains("renew provider credentials"));
                assert!(!message.contains("private-provider-name"));
            }
            _ => panic!("Expected LoopOutcome::Failed"),
        }
        assert_eq!(delegate.llm_responses.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn test_tool_intent_nudge_fires_and_caps() {
        let delegate = MockDelegate::new(vec![
            text_output("Let me search for that file"),
            text_output("Let me search for that file"),
            text_output("Let me search for that file"),
        ]);
        let reasoning = stub_reasoning();
        let mut ctx = ReasoningContext::new();
        ctx.available_tools.push(crate::llm::ToolDefinition {
            name: "search".to_string(),
            description: "Search files".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        });
        let config = AgenticLoopConfig {
            max_iterations: 10,
            enable_tool_intent_nudge: true,
            max_tool_intent_nudges: 2,
            interrupt_token: None,
        };

        let outcome = run_agentic_loop(&delegate, &reasoning, &mut ctx, &config)
            .await
            .unwrap();

        assert!(matches!(outcome, LoopOutcome::Response(_)));
        assert_eq!(delegate.nudge_count.load(Ordering::SeqCst), 2);
        let nudge_messages = ctx
            .messages
            .iter()
            .filter(|m| {
                m.role == crate::llm::Role::User
                    && m.content.contains("you did not include any tool calls")
            })
            .count();
        assert_eq!(
            nudge_messages, 2,
            "Should have exactly 2 nudge messages in context"
        );
    }

    #[tokio::test]
    async fn test_before_llm_call_early_exit() {
        let delegate = MockDelegate::new(vec![text_output("unreachable")])
            .with_early_exit(1, LoopOutcome::Stopped);
        let reasoning = stub_reasoning();
        let mut ctx = ReasoningContext::new();
        let config = AgenticLoopConfig::default();

        let outcome = run_agentic_loop(&delegate, &reasoning, &mut ctx, &config)
            .await
            .unwrap();

        assert!(matches!(outcome, LoopOutcome::Stopped));
        assert!(delegate.iterations_seen.lock().await.is_empty());
    }

    // --- Mid-turn interrupt (Hermes-style) regression tests ---

    /// Delegate whose LLM call hangs like a stalled stream. The interrupt
    /// token is cancelled from inside the blocked call to simulate an
    /// interrupt arriving while the stream is mid-flight.
    struct HungLlmDelegate {
        token: CancellationToken,
        llm_started: AtomicBool,
    }

    #[async_trait]
    impl LoopDelegate for HungLlmDelegate {
        async fn check_signals(&self) -> LoopSignal {
            LoopSignal::Continue
        }
        async fn before_llm_call(&self, _: &mut ReasoningContext, _: usize) -> Option<LoopOutcome> {
            None
        }
        async fn call_llm(
            &self,
            _: &Reasoning,
            _: &mut ReasoningContext,
            _: usize,
        ) -> Result<crate::llm::RespondOutput, crate::error::Error> {
            self.llm_started.store(true, Ordering::SeqCst);
            // Interrupt arrives while the "stream" is in flight.
            self.token.cancel();
            std::future::pending().await
        }
        async fn handle_text_response(&self, _: &str, _: &mut ReasoningContext) -> TextAction {
            TextAction::Continue
        }
        async fn execute_tool_calls(
            &self,
            _: Vec<ToolCall>,
            _: Option<String>,
            _: &mut ReasoningContext,
        ) -> Result<Option<LoopOutcome>, crate::error::Error> {
            Ok(None)
        }
    }

    #[tokio::test(start_paused = true)]
    async fn interrupt_token_aborts_in_flight_llm_call() {
        let token = CancellationToken::new();
        let delegate = HungLlmDelegate {
            token: token.clone(),
            llm_started: AtomicBool::new(false),
        };
        let reasoning = stub_reasoning();
        let mut ctx = ReasoningContext::new();
        let config = AgenticLoopConfig {
            interrupt_token: Some(token),
            ..Default::default()
        };

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            run_agentic_loop(&delegate, &reasoning, &mut ctx, &config),
        )
        .await
        .expect("interrupt must unblock the loop from a hung LLM call")
        .unwrap();

        assert!(delegate.llm_started.load(Ordering::SeqCst));
        assert!(
            matches!(outcome, LoopOutcome::Stopped),
            "mid-stream interrupt must surface as Stopped"
        );
    }

    /// Delegate whose tool batch hangs mid-execution. The interrupt token is
    /// cancelled from inside the batch to simulate an interrupt arriving
    /// during tool execution.
    struct HungToolDelegate {
        token: CancellationToken,
        tools_started: AtomicBool,
    }

    #[async_trait]
    impl LoopDelegate for HungToolDelegate {
        async fn check_signals(&self) -> LoopSignal {
            LoopSignal::Continue
        }
        async fn before_llm_call(&self, _: &mut ReasoningContext, _: usize) -> Option<LoopOutcome> {
            None
        }
        async fn call_llm(
            &self,
            _: &Reasoning,
            _: &mut ReasoningContext,
            _: usize,
        ) -> Result<crate::llm::RespondOutput, crate::error::Error> {
            Ok(tool_calls_output(vec![ToolCall {
                id: "call_1".to_string(),
                name: "slow_tool".to_string(),
                arguments: serde_json::json!({}),
                reasoning: None,
            }]))
        }
        async fn handle_text_response(&self, _: &str, _: &mut ReasoningContext) -> TextAction {
            TextAction::Continue
        }
        async fn execute_tool_calls(
            &self,
            _: Vec<ToolCall>,
            _: Option<String>,
            _: &mut ReasoningContext,
        ) -> Result<Option<LoopOutcome>, crate::error::Error> {
            self.tools_started.store(true, Ordering::SeqCst);
            // Interrupt arrives while the tool batch is in flight.
            self.token.cancel();
            std::future::pending().await
        }
    }

    #[tokio::test(start_paused = true)]
    async fn interrupt_token_aborts_in_flight_tool_execution() {
        let token = CancellationToken::new();
        let delegate = HungToolDelegate {
            token: token.clone(),
            tools_started: AtomicBool::new(false),
        };
        let reasoning = stub_reasoning();
        let mut ctx = ReasoningContext::new();
        let config = AgenticLoopConfig {
            interrupt_token: Some(token),
            ..Default::default()
        };

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            run_agentic_loop(&delegate, &reasoning, &mut ctx, &config),
        )
        .await
        .expect("interrupt must unblock the loop from a hung tool batch")
        .unwrap();

        assert!(delegate.tools_started.load(Ordering::SeqCst));
        assert!(
            matches!(outcome, LoopOutcome::Stopped),
            "mid-tool interrupt must surface as Stopped"
        );
    }

    #[tokio::test]
    async fn pre_cancelled_token_stops_before_llm_call() {
        let token = CancellationToken::new();
        token.cancel();
        let delegate = MockDelegate::new(vec![text_output("unreachable")]);
        let reasoning = stub_reasoning();
        let mut ctx = ReasoningContext::new();
        let config = AgenticLoopConfig {
            interrupt_token: Some(token),
            ..Default::default()
        };

        let outcome = run_agentic_loop(&delegate, &reasoning, &mut ctx, &config)
            .await
            .unwrap();

        assert!(matches!(outcome, LoopOutcome::Stopped));
        assert_eq!(
            delegate.llm_responses.lock().await.len(),
            1,
            "LLM must not be called after an already-fired interrupt"
        );
    }

    /// A steer-style injected message queued by the delegate while a tool
    /// batch runs must arrive at the next iteration boundary — after the
    /// tool results — and preserve message ordering.
    #[tokio::test]
    async fn steer_injection_arrives_after_tool_results() {
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "echo".to_string(),
            arguments: serde_json::json!({}),
            reasoning: None,
        };
        let delegate = MockDelegate::new(vec![
            tool_calls_output(vec![tool_call]),
            text_output("Steered answer"),
        ]);
        // Arm the steer-style injection in MockDelegate::execute_tool_calls:
        // check_signals takes the held signal each iteration, so seeding an
        // InjectMessage directly would fire before the first LLM call. Arming
        // it during the tool batch lands it at the iteration boundary right
        // after the tool results — the steer contract.
        *delegate.signal_after_first_exec.lock().await = Some(LoopSignal::InjectMessage(
            "steer: switch to plan B".to_string(),
        ));
        let reasoning = stub_reasoning();
        let mut ctx = ReasoningContext::new();
        let config = AgenticLoopConfig::default();

        let outcome = run_agentic_loop(&delegate, &reasoning, &mut ctx, &config)
            .await
            .unwrap();

        assert!(matches!(outcome, LoopOutcome::Response(ref t) if t == "Steered answer"));
        let roles: Vec<(&crate::llm::Role, &str)> = ctx
            .messages
            .iter()
            .map(|m| (&m.role, m.content.as_str()))
            .collect();
        let tool_result_pos = roles
            .iter()
            .position(|(_, c)| c.contains("tool result stub"))
            .expect("tool result recorded");
        let steer_pos = roles
            .iter()
            .position(|(_, c)| c.contains("steer: switch to plan B"))
            .expect("steer note injected");
        assert!(
            steer_pos > tool_result_pos,
            "steer note must arrive after the current tool call completes"
        );
    }

    #[test]
    fn test_truncate_short_string_unchanged() {
        assert_eq!(truncate_for_preview("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_short_string_borrows() {
        let result = truncate_for_preview("hello", 10);
        assert!(matches!(result, Cow::Borrowed("hello")));
    }

    #[test]
    fn test_truncate_long_string_adds_ellipsis() {
        let result = truncate_for_preview("hello world", 5);
        assert_eq!(result, "hello...");
    }

    #[test]
    fn test_truncate_long_string_owns() {
        let result = truncate_for_preview("hello world", 5);
        assert!(matches!(result, Cow::Owned(_)));
    }

    #[test]
    fn test_truncate_multibyte_safe() {
        let result = truncate_for_preview("café", 4);
        assert_eq!(result, "caf...");
    }
}
