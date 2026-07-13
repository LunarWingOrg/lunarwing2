//! Deterministic stream fixtures shared by LLM decorator tests.

// JUSTIFICATION: Later plan tasks consume this shared fixture incrementally.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use rust_decimal::Decimal;

use crate::llm::error::LlmError;
use crate::llm::provider::{
    CompletionRequest, CompletionResponse, FinishReason, LlmProvider, LlmStream, LlmStreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};

pub(crate) enum StreamScript {
    SetupError(LlmError),
    Items(Vec<Result<LlmStreamChunk, LlmError>>),
    DelayedItems {
        delay: Duration,
        items: Vec<Result<LlmStreamChunk, LlmError>>,
    },
}

pub(crate) struct ScriptedStreamingProvider {
    model_name: String,
    plain_scripts: Mutex<VecDeque<StreamScript>>,
    tool_scripts: Mutex<VecDeque<StreamScript>>,
    plain_calls: AtomicUsize,
    tool_calls: AtomicUsize,
}

impl ScriptedStreamingProvider {
    pub(crate) fn new(
        model_name: impl Into<String>,
        plain_scripts: Vec<StreamScript>,
        tool_scripts: Vec<StreamScript>,
    ) -> Self {
        Self {
            model_name: model_name.into(),
            plain_scripts: Mutex::new(plain_scripts.into()),
            tool_scripts: Mutex::new(tool_scripts.into()),
            plain_calls: AtomicUsize::new(0),
            tool_calls: AtomicUsize::new(0),
        }
    }

    pub(crate) fn plain_calls(&self) -> usize {
        self.plain_calls.load(Ordering::Relaxed)
    }

    pub(crate) fn tool_calls(&self) -> usize {
        self.tool_calls.load(Ordering::Relaxed)
    }

    fn pop_script(
        queue: &Mutex<VecDeque<StreamScript>>,
        model_name: &str,
        request_kind: &str,
    ) -> Result<StreamScript, LlmError> {
        let mut scripts = queue.lock().unwrap_or_else(|error| error.into_inner());
        scripts.pop_front().ok_or_else(|| LlmError::RequestFailed {
            provider: model_name.to_string(),
            reason: format!("no scripted {request_kind} stream remains"),
        })
    }
}

fn script_stream(script: StreamScript) -> Result<LlmStream<'static>, LlmError> {
    match script {
        StreamScript::SetupError(error) => Err(error),
        StreamScript::Items(items) => Ok(futures::stream::iter(items).boxed()),
        StreamScript::DelayedItems { delay, items } => {
            let mut items = items.into_iter();
            let Some(first) = items.next() else {
                return Ok(futures::stream::empty().boxed());
            };
            let head = futures::stream::once(async move {
                tokio::time::sleep(delay).await;
                first
            });
            Ok(head.chain(futures::stream::iter(items)).boxed())
        }
    }
}

#[async_trait]
impl LlmProvider for ScriptedStreamingProvider {
    fn model_name(&self) -> &str {
        &self.model_name
    }

    fn cost_per_token(&self) -> (Decimal, Decimal) {
        (Decimal::ZERO, Decimal::ZERO)
    }

    async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        Ok(CompletionResponse {
            content: "blocking response".to_string(),
            input_tokens: 10,
            output_tokens: 5,
            finish_reason: FinishReason::Stop,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        })
    }

    async fn complete_stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        self.plain_calls.fetch_add(1, Ordering::Relaxed);
        let script = Self::pop_script(&self.plain_scripts, &self.model_name, "plain")?;
        script_stream(script)
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        Ok(ToolCompletionResponse {
            content: Some("blocking tool response".to_string()),
            tool_calls: Vec::new(),
            input_tokens: 10,
            output_tokens: 5,
            finish_reason: FinishReason::Stop,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        })
    }

    async fn complete_with_tools_stream(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        self.tool_calls.fetch_add(1, Ordering::Relaxed);
        let script = Self::pop_script(&self.tool_scripts, &self.model_name, "tool")?;
        script_stream(script)
    }
}
