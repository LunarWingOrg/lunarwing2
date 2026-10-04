//! Worker runtime: the main execution loop inside a container.
//!
//! Reuses the existing `Reasoning` and `SafetyLayer` infrastructure but
//! connects to the orchestrator for LLM calls instead of calling APIs directly.
//! Streams real-time events (message, tool_use, tool_result, result) through
//! the orchestrator's job event pipeline for UI visibility.
//!
//! Uses the shared `AgenticLoop` engine via `ContainerDelegate`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use lunarwing_common::DEFAULT_WORKER_ITERATIONS;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::agent::agentic_loop::{
    AgenticLoopConfig, LoopDelegate, LoopOutcome, LoopSignal, TextAction, truncate_for_preview,
};
use crate::config::SafetyConfig;
use crate::context::JobContext;
use crate::error::WorkerError;
use crate::llm::{ChatMessage, LlmProvider, Reasoning, ReasoningContext};
use crate::safety::SafetyLayer;
use crate::tools::ToolRegistry;
use crate::tools::execute::{execute_tool_simple, process_tool_result};
use crate::worker::api::{CompletionReport, JobEventPayload, StatusUpdate, WorkerHttpClient};
use crate::worker::mcp::WorkerMcpRuntime;
use crate::worker::proxy_llm::ProxyLlmProvider;

/// Configuration for the worker runtime.
pub struct WorkerConfig {
    pub job_id: Uuid,
    pub orchestrator_url: String,
    pub max_iterations: u32,
    pub timeout: Duration,
    pub require_worker_mcp: bool,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            job_id: Uuid::nil(),
            orchestrator_url: String::new(),
            max_iterations: DEFAULT_WORKER_ITERATIONS,
            timeout: Duration::from_secs(600),
            require_worker_mcp: false,
        }
    }
}

/// The worker runtime runs inside a Docker container.
///
/// It connects to the orchestrator over HTTP, fetches its job description,
/// then runs a tool execution loop until the job is complete. Events are
/// streamed to the orchestrator so the UI can show real-time progress.
pub struct WorkerRuntime {
    config: WorkerConfig,
    client: Arc<WorkerHttpClient>,
    llm: Arc<dyn LlmProvider>,
    safety: Arc<SafetyLayer>,
    tools: Arc<ToolRegistry>,
    /// Credentials fetched from the orchestrator, injected into child processes
    /// via `Command::envs()` rather than mutating the global process environment.
    ///
    /// Wrapped in `Arc` to avoid deep-cloning the map on every tool invocation.
    extra_env: Arc<HashMap<String, String>>,
}

impl WorkerRuntime {
    /// Create a new worker runtime.
    ///
    /// Reads `LUNARWING_WORKER_TOKEN` from the environment for auth (legacy
    /// alias `IRONCLAW_WORKER_TOKEN` still accepted).
    pub fn new(config: WorkerConfig) -> Result<Self, WorkerError> {
        let client = Arc::new(WorkerHttpClient::from_env(
            config.orchestrator_url.clone(),
            config.job_id,
        )?);

        let llm: Arc<dyn LlmProvider> = Arc::new(ProxyLlmProvider::new(
            Arc::clone(&client),
            "proxied".to_string(),
        ));

        let safety = Arc::new(SafetyLayer::new(&SafetyConfig {
            max_output_length: 100_000,
            injection_check_enabled: true,
        }));

        let tools = Arc::new(ToolRegistry::new());
        // Register only container-safe tools
        tools.register_container_tools();

        Ok(Self {
            config,
            client,
            llm,
            safety,
            tools,
            extra_env: Arc::new(HashMap::new()),
        })
    }

    /// Run the worker until the job is complete or an error occurs.
    pub async fn run(mut self) -> Result<(), WorkerError> {
        tracing::info!("Worker starting for job {}", self.config.job_id);

        // Fetch job description from orchestrator
        let job = self.client.get_job().await?;
        if self.config.require_worker_mcp && job.mcp_servers.is_empty() {
            return Err(WorkerError::ExecutionFailed {
                reason: "orchestrator required worker-local MCP but supplied no runtime descriptor"
                    .to_string(),
            });
        }

        tracing::info!(
            "Received job: {} - {}",
            job.title,
            truncate_for_preview(&job.description, 100)
        );

        // Fetch credentials and store them for injection into child processes
        // via Command::envs() (avoids unsafe std::env::set_var in multi-threaded runtime).
        let credentials = self.client.fetch_credentials().await?;
        {
            let mut env_map = HashMap::new();
            for cred in &credentials {
                env_map.insert(cred.env_var.clone(), cred.value.clone());
            }
            self.extra_env = Arc::new(env_map);
        }
        if !credentials.is_empty() {
            tracing::info!(
                "Fetched {} credential(s) for child process injection",
                credentials.len()
            );
        }

        let owner_id = if job.owner_id.is_empty() {
            "default".to_string()
        } else {
            job.owner_id.clone()
        };
        let mcp_credentials = self.client.fetch_mcp_credentials().await?;
        let mut mcp_runtime = match WorkerMcpRuntime::activate(
            Arc::clone(&self.tools),
            job.mcp_servers.clone(),
            mcp_credentials,
            &owner_id,
        )
        .await
        {
            Ok(runtime) => runtime,
            Err(error) => {
                let message = format!("Worker MCP activation failed: {error}");
                self.post_event(
                    "result",
                    serde_json::json!({
                        "status": "error",
                        "success": false,
                        "message": message,
                    }),
                )
                .await;
                self.client
                    .report_complete(&CompletionReport {
                        success: false,
                        message: Some(message),
                        iterations: 0,
                    })
                    .await?;
                return Ok(());
            }
        };

        // Report that we're starting
        if let Err(status_error) = self
            .client
            .report_status(&StatusUpdate {
                state: "in_progress".to_string(),
                message: Some("Worker started, beginning execution".to_string()),
                iteration: 0,
            })
            .await
        {
            return match mcp_runtime.shutdown().await {
                Ok(()) => Err(status_error),
                Err(cleanup_error) => Err(WorkerError::ExecutionFailed {
                    reason: format!(
                        "worker status failed: {status_error}; MCP cleanup failed: {cleanup_error}"
                    ),
                }),
            };
        }

        // Create reasoning engine
        let reasoning = Reasoning::new(self.llm.clone());

        // Build initial context
        let mut reason_ctx = ReasoningContext::new().with_job(&job.description);

        reason_ctx.messages.push(ChatMessage::system(format!(
            r#"You are an autonomous agent running inside a Docker container.

Job: {}
Description: {}

You have tools for shell commands, file operations, and code editing.
Work independently to complete this job. Report when done."#,
            job.title, job.description
        )));

        // Load tool definitions
        reason_ctx.available_tools = self.tools.tool_definitions().await;

        // Shared iteration tracker — read after the loop to report accurate counts.
        let iteration_tracker = Arc::new(Mutex::new(0u32));

        // Run with timeout using the shared agentic loop
        let result = tokio::time::timeout(self.config.timeout, async {
            let delegate = ContainerDelegate {
                client: self.client.clone(),
                safety: self.safety.clone(),
                tools: self.tools.clone(),
                extra_env: self.extra_env.clone(),
                owner_id,
                last_output: Mutex::new(String::new()),
                iteration_tracker: iteration_tracker.clone(),
                consecutive_text_only: Mutex::new(0),
                consecutive_all_failed: Mutex::new(0),
            };

            let config = AgenticLoopConfig {
                max_iterations: self.config.max_iterations as usize,
                enable_tool_intent_nudge: true,
                max_tool_intent_nudges: 2,
                // Container jobs keep boundary-only cancellation: the delegate's
                // check_signals already polls the worker's stop channel.
                interrupt_token: None,
            };

            crate::agent::agentic_loop::run_agentic_loop(
                &delegate,
                &reasoning,
                &mut reason_ctx,
                &config,
            )
            .await
        })
        .await;

        let iterations = *iteration_tracker.lock().await;

        let mut report = match result {
            Ok(Ok(LoopOutcome::Response(output))) => {
                tracing::info!("Worker completed job {} successfully", self.config.job_id);
                CompletionReport {
                    success: true,
                    message: Some(output),
                    iterations,
                }
            }
            Ok(Ok(LoopOutcome::MaxIterations)) => {
                let msg = format!(
                    "max iterations ({}) exceeded after a final recovery turn; retry with narrower scope or a higher max_iterations value",
                    self.config.max_iterations
                );
                tracing::warn!("Worker failed for job {}: {}", self.config.job_id, msg);
                CompletionReport {
                    success: false,
                    message: Some(format!("Execution failed: {msg}")),
                    iterations,
                }
            }
            Ok(Ok(LoopOutcome::Failed(message))) => {
                tracing::warn!(
                    "Worker failed for job {} after recovery: {}",
                    self.config.job_id,
                    message
                );
                CompletionReport {
                    success: false,
                    message: Some(format!("Execution failed: {message}")),
                    iterations,
                }
            }
            Ok(Ok(LoopOutcome::Stopped)) => {
                tracing::info!("Worker for job {} stopped", self.config.job_id);
                CompletionReport {
                    success: false,
                    message: Some("Execution stopped".to_string()),
                    iterations,
                }
            }
            Ok(Ok(LoopOutcome::NeedApproval(_))) => {
                tracing::info!(
                    "Worker for job {} requires approval before it can continue",
                    self.config.job_id
                );
                CompletionReport {
                    success: false,
                    message: Some("Awaiting approval".to_string()),
                    iterations,
                }
            }
            Ok(Err(e)) => {
                tracing::error!("Worker failed for job {}: {}", self.config.job_id, e);
                CompletionReport {
                    success: false,
                    message: Some(format!("Execution failed: {e}")),
                    iterations,
                }
            }
            Err(_) => {
                tracing::warn!("Worker timed out for job {}", self.config.job_id);
                CompletionReport {
                    success: false,
                    message: Some("Execution timed out".to_string()),
                    iterations,
                }
            }
        };

        if let Err(cleanup_error) = mcp_runtime.shutdown().await {
            tracing::warn!(
                job_id = %self.config.job_id,
                "Worker MCP cleanup failed"
            );
            report.success = false;
            let cleanup_message = format!("MCP cleanup failed: {cleanup_error}");
            report.message = Some(match report.message {
                Some(message) => format!("{message}; {cleanup_message}"),
                None => cleanup_message,
            });
        }

        let status = if report.success {
            "completed"
        } else if report.message.as_deref() == Some("Awaiting approval") {
            "stuck"
        } else {
            "error"
        };
        self.post_event(
            "result",
            serde_json::json!({
                "status": status,
                "success": report.success,
                "message": report.message.as_deref().map(|message| truncate_for_preview(message, 2000)),
            }),
        )
        .await;
        self.client.report_complete(&report).await?;

        Ok(())
    }

    /// Post a job event to the orchestrator (fire-and-forget).
    async fn post_event(&self, event_type: &str, data: serde_json::Value) {
        self.client
            .post_event(&JobEventPayload {
                event_type: event_type.to_string(),
                data,
            })
            .await;
    }
}

/// Container delegate: implements `LoopDelegate` for the Docker container context.
///
/// Tools execute sequentially. Events are posted to the orchestrator via HTTP.
/// Completion is detected via `llm_signals_completion()`.
struct ContainerDelegate {
    client: Arc<WorkerHttpClient>,
    safety: Arc<SafetyLayer>,
    tools: Arc<ToolRegistry>,
    extra_env: Arc<HashMap<String, String>>,
    owner_id: String,
    /// Tracks the last successful tool output for the final response.
    last_output: Mutex<String>,
    /// Tracks the current iteration — shared with the outer `run` method so
    /// `CompletionReport` can include accurate iteration counts.
    iteration_tracker: Arc<Mutex<u32>>,
    /// Consecutive text-only responses (no tool calls). When this reaches
    /// the threshold the loop exits — the LLM has stopped doing work.
    consecutive_text_only: Mutex<u32>,
    /// Consecutive iterations where every tool call failed. The third failure
    /// schedules one recovery turn; another all-failed round becomes terminal.
    consecutive_all_failed: Mutex<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailedWorkRecovery {
    Continue,
    Warn,
    Fail,
}

fn failed_work_recovery(consecutive_failures: u32) -> FailedWorkRecovery {
    match consecutive_failures {
        0..=2 => FailedWorkRecovery::Continue,
        3 => FailedWorkRecovery::Warn,
        _ => FailedWorkRecovery::Fail,
    }
}

/// Detect post-work chatter: suggestions, follow-up offers, or XML tags
/// that indicate the LLM finished the task but is looping on meta-content.
fn is_post_work_chatter(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("<suggestions")
        || lower.contains("<suggestion>")
        || lower.contains("would you like me to")
        || lower.contains("shall i ")
        || lower.contains("let me know if")
        || lower.contains("here are some suggestions")
        || lower.contains("what would you like to do next")
}

impl ContainerDelegate {
    async fn post_event(&self, event_type: &str, data: serde_json::Value) {
        self.client
            .post_event(&JobEventPayload {
                event_type: event_type.to_string(),
                data,
            })
            .await;
    }

    /// Poll the orchestrator for a follow-up prompt. If one is available,
    /// inject it as a user message into the reasoning context.
    async fn poll_and_inject_prompt(&self, reason_ctx: &mut ReasoningContext) {
        match self.client.poll_prompt().await {
            Ok(Some(prompt)) => {
                tracing::info!(
                    "Received follow-up prompt: {}",
                    truncate_for_preview(&prompt.content, 100)
                );
                self.post_event(
                    "message",
                    serde_json::json!({
                        "role": "user",
                        "content": truncate_for_preview(&prompt.content, 2000),
                    }),
                )
                .await;
                reason_ctx.messages.push(ChatMessage::user(&prompt.content));
            }
            Ok(None) => {}
            Err(e) => {
                tracing::debug!("Failed to poll for prompt: {}", e);
            }
        }
    }
}

#[async_trait]
impl LoopDelegate for ContainerDelegate {
    async fn check_signals(&self) -> LoopSignal {
        // Container runtime has no stop signals — the orchestrator manages lifecycle.
        LoopSignal::Continue
    }

    async fn before_llm_call(
        &self,
        reason_ctx: &mut ReasoningContext,
        iteration: usize,
    ) -> Option<LoopOutcome> {
        let iteration = iteration as u32;
        *self.iteration_tracker.lock().await = iteration;

        // Report progress every 5 iterations
        if iteration % 5 == 1 {
            let _ = self
                .client
                .report_status(&StatusUpdate {
                    state: "in_progress".to_string(),
                    message: Some(format!("Iteration {}", iteration)),
                    iteration,
                })
                .await;
        }

        // Poll for follow-up prompts from the user
        self.poll_and_inject_prompt(reason_ctx).await;

        // Refresh tools (in case WASM tools were built)
        reason_ctx.available_tools = self.tools.tool_definitions().await;

        None
    }

    async fn call_llm(
        &self,
        reasoning: &Reasoning,
        reason_ctx: &mut ReasoningContext,
        _iteration: usize,
    ) -> Result<crate::llm::RespondOutput, crate::error::Error> {
        // Container uses respond_with_tools (which may return either text or tool calls)
        reasoning
            .respond_with_tools(reason_ctx)
            .await
            .map_err(Into::into)
    }

    async fn handle_text_response(
        &self,
        text: &str,
        reason_ctx: &mut ReasoningContext,
    ) -> TextAction {
        self.post_event(
            "message",
            serde_json::json!({
                "role": "assistant",
                "content": truncate_for_preview(text, 2000),
            }),
        )
        .await;

        // Bound text-only chatter without treating repetition as success.
        let mut text_count = self.consecutive_text_only.lock().await;
        *text_count += 1;
        let consecutive = *text_count;
        drop(text_count);

        let is_done = crate::util::llm_signals_completion(text) || is_post_work_chatter(text);

        if is_done {
            let last = self.last_output.lock().await;
            let output = if last.is_empty() {
                text.to_string()
            } else {
                last.clone()
            };
            return TextAction::Return(LoopOutcome::Response(output));
        }

        if consecutive >= 3 {
            return TextAction::Return(LoopOutcome::Failed(
                "No completion signal after three text-only responses; job not confirmed complete"
                    .into(),
            ));
        }

        reason_ctx.messages.push(ChatMessage::assistant(text));
        TextAction::Continue
    }

    async fn execute_tool_calls(
        &self,
        tool_calls: Vec<crate::llm::ToolCall>,
        content: Option<String>,
        reason_ctx: &mut ReasoningContext,
    ) -> Result<Option<LoopOutcome>, crate::error::Error> {
        // Tool calls mean the LLM is still working — reset text counter.
        *self.consecutive_text_only.lock().await = 0;

        if let Some(ref text) = content {
            self.post_event(
                "message",
                serde_json::json!({
                    "role": "assistant",
                    "content": truncate_for_preview(text, 2000),
                }),
            )
            .await;
        }

        // Add assistant message with tool_calls (OpenAI protocol)
        reason_ctx
            .messages
            .push(ChatMessage::assistant_with_tool_calls(
                content,
                tool_calls.clone(),
            ));

        // Execute tools sequentially (container context — no parallel execution)
        let mut any_succeeded = false;

        for tc in tool_calls {
            self.post_event(
                "tool_use",
                serde_json::json!({
                    "tool_name": tc.name,
                    "input": truncate_for_preview(&tc.arguments.to_string(), 500),
                }),
            )
            .await;

            let job_ctx = JobContext {
                user_id: self.owner_id.clone(),
                extra_env: self.extra_env.clone(),
                ..Default::default()
            };

            let result = execute_tool_simple(
                &self.tools,
                &self.safety,
                &tc.name,
                tc.arguments.clone(),
                &job_ctx,
            )
            .await;

            self.post_event(
                "tool_result",
                serde_json::json!({
                    "tool_name": tc.name,
                    "output": match &result {
                        Ok(output) => truncate_for_preview(output, 2000),
                        Err(e) => format!("Error: {}", truncate_for_preview(e, 500)).into(),
                    },
                    "success": result.is_ok(),
                }),
            )
            .await;

            if let Ok(output) = &result {
                any_succeeded = true;
                *self.last_output.lock().await = output.clone();
            }

            // Use shared result processing
            let (_, message) = process_tool_result(&self.safety, &tc.name, &tc.id, &result);
            reason_ctx.messages.push(message);
        }

        // Track consecutive all-fail iterations to catch error loops
        // (e.g., "Permission denied" retried endlessly).
        let mut fail_count = self.consecutive_all_failed.lock().await;
        let recovery = if any_succeeded {
            *fail_count = 0;
            FailedWorkRecovery::Continue
        } else {
            *fail_count += 1;
            failed_work_recovery(*fail_count)
        };
        drop(fail_count);

        match recovery {
            FailedWorkRecovery::Continue => Ok(None),
            FailedWorkRecovery::Warn => {
                tracing::warn!(
                    "3 consecutive iterations had no successful tool calls; scheduling recovery"
                );
                reason_ctx.messages.push(ChatMessage::system(
                    "Tool failure warning: three consecutive tool rounds produced no successful work. This is the recovery turn. Do not repeat the same call unchanged; use different allowed tools or parameters, or state which capability or permission must be enabled and provide the best partial result.",
                ));
                Ok(None)
            }
            FailedWorkRecovery::Fail => Ok(Some(LoopOutcome::Failed(
                "Tool calls kept failing after a recovery turn. Change approach or parameters, enable the required capability or permissions, then retry."
                    .to_string(),
            ))),
        }
    }

    async fn on_tool_intent_nudge(&self, text: &str, _reason_ctx: &mut ReasoningContext) {
        self.post_event(
            "message",
            serde_json::json!({
                "role": "assistant",
                "content": truncate_for_preview(text, 2000),
                "nudge": true,
            }),
        )
        .await;
    }

    async fn after_iteration(&self, _iteration: usize) {
        // Brief pause between iterations
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::agentic_loop::truncate_for_preview;

    #[test]
    fn test_truncate_within_limit() {
        assert_eq!(truncate_for_preview("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_at_limit() {
        assert_eq!(truncate_for_preview("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_beyond_limit() {
        let result = truncate_for_preview("hello world", 5);
        assert_eq!(result, "hello...");
    }

    #[test]
    fn test_truncate_multibyte_safe() {
        let result = truncate_for_preview("é is fancy", 1);
        assert_eq!(result, "...");
    }

    #[test]
    fn test_post_work_chatter_detection() {
        assert!(is_post_work_chatter(
            "Would you like me to do something else?"
        ));
        assert!(is_post_work_chatter(
            "<suggestions>\n<suggestion>try X</suggestion>\n</suggestions>"
        ));
        assert!(is_post_work_chatter(
            "Let me know if you need anything else."
        ));
        assert!(is_post_work_chatter("Shall I run the tests now?"));
        assert!(is_post_work_chatter("What would you like to do next?"));
        assert!(!is_post_work_chatter("Running command: echo hello"));
        assert!(!is_post_work_chatter("The file has been updated."));
    }

    #[test]
    fn failed_work_gets_exactly_one_recovery_round() {
        assert_eq!(failed_work_recovery(1), FailedWorkRecovery::Continue);
        assert_eq!(failed_work_recovery(2), FailedWorkRecovery::Continue);
        assert_eq!(failed_work_recovery(3), FailedWorkRecovery::Warn);
        assert_eq!(failed_work_recovery(4), FailedWorkRecovery::Fail);
    }
}
