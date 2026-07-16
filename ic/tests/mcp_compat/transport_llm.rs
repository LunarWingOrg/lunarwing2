struct McpTransportLlm {
    calls: AtomicUsize,
    saw_advertised_tool: AtomicBool,
    saw_expected_call_id: AtomicBool,
}

impl McpTransportLlm {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            saw_advertised_tool: AtomicBool::new(false),
            saw_expected_call_id: AtomicBool::new(false),
        }
    }

    fn has_tool_result(request: &ToolCompletionRequest) -> bool {
        request
            .messages
            .iter()
            .any(|msg| msg.role == Role::Tool && msg.tool_call_id.as_deref() == Some(CALL_ID))
    }

    fn observe_request(&self, request: &ToolCompletionRequest) {
        if request
            .tools
            .iter()
            .any(|tool| tool.name == ENGINE_ACTION_NAME)
        {
            self.saw_advertised_tool.store(true, Ordering::SeqCst);
        }
        if Self::has_tool_result(request) {
            self.saw_expected_call_id.store(true, Ordering::SeqCst);
        }
    }
}

fn provider_failure() -> LlmError {
    LlmError::RequestFailed {
        provider: "mcp-compat-test".into(),
        reason: "unexpected LLM call".into(),
    }
}

#[async_trait]
impl LlmProvider for McpTransportLlm {
    fn model_name(&self) -> &str {
        "mcp-compat-test"
    }

    fn cost_per_token(&self) -> (Decimal, Decimal) {
        (Decimal::ZERO, Decimal::ZERO)
    }

    async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        Err(provider_failure())
    }

    async fn complete_stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        Err(provider_failure())
    }

    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        self.observe_request(&request);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match call {
            0 => Ok(ToolCompletionResponse {
                content: None,
                tool_calls: vec![ToolCall {
                    id: CALL_ID.to_string(),
                    name: ENGINE_ACTION_NAME.to_string(),
                    arguments: serde_json::json!({"query": "test"}),
                    reasoning: None,
                }],
                input_tokens: 1,
                output_tokens: 1,
                finish_reason: FinishReason::ToolUse,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            }),
            1 if Self::has_tool_result(&request) => Ok(ToolCompletionResponse {
                content: Some(format!("```repl\nFINAL('{TERMINAL_RESPONSE}')\n```")),
                tool_calls: Vec::new(),
                input_tokens: 1,
                output_tokens: 1,
                finish_reason: FinishReason::Stop,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            }),
            _ => Err(provider_failure()),
        }
    }

    async fn complete_with_tools_stream(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        self.observe_request(&request);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let has_result = Self::has_tool_result(&request);
        let chunks: Vec<Result<LlmStreamChunk, LlmError>> = match call {
            0 => vec![
                Ok(LlmStreamChunk::ToolCallDelta {
                    index: 0,
                    id: Some(CALL_ID.to_string()),
                    name: Some(ENGINE_ACTION_NAME.to_string()),
                    args_delta: serde_json::json!({"query": "test"}).to_string(),
                }),
                Ok(LlmStreamChunk::Done {
                    usage: Some(TokenUsage::default()),
                    finish_reason: "tool_calls".to_string(),
                }),
            ],
            1 if has_result => vec![
                Ok(LlmStreamChunk::TextDelta(format!(
                    "```repl\nFINAL('{TERMINAL_RESPONSE}')\n```"
                ))),
                Ok(LlmStreamChunk::Done {
                    usage: Some(TokenUsage::default()),
                    finish_reason: "stop".to_string(),
                }),
            ],
            _ => return Err(provider_failure()),
        };
        Ok(futures::stream::iter(chunks).boxed())
    }
}
