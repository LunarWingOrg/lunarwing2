struct McpAuthLlm {
    calls: AtomicUsize,
    last_mcp_result: Mutex<Option<String>>,
}

impl McpAuthLlm {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            last_mcp_result: Mutex::new(None),
        }
    }

    fn has_tool_result(request: &ToolCompletionRequest, call_id: &str) -> bool {
        request.messages.iter().any(|message| {
            message.role == Role::Tool && message.tool_call_id.as_deref() == Some(call_id)
        })
    }

    fn advertises(request: &ToolCompletionRequest, action_name: &str) -> bool {
        request.tools.iter().any(|tool| tool.name == action_name)
    }

    fn has_successful_mcp_result(&self, request: &ToolCompletionRequest) -> bool {
        let result = request.messages.iter().find(|message| {
            message.role == Role::Tool && message.tool_call_id.as_deref() == Some(AUTH_MCP_CALL_ID)
        });
        if let Some(message) = result {
            *self.last_mcp_result.lock().expect("MCP result mutex") = Some(message.content.clone());
        }
        result.is_some_and(|message| message.content.contains("authenticated-search-result"))
    }

    fn tool_response(
        call_id: &str,
        name: &str,
        arguments: serde_json::Value,
    ) -> ToolCompletionResponse {
        ToolCompletionResponse {
            content: None,
            tool_calls: vec![ToolCall {
                id: call_id.to_string(),
                name: name.to_string(),
                arguments,
                reasoning: None,
            }],
            input_tokens: 1,
            output_tokens: 1,
            finish_reason: FinishReason::ToolUse,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        }
    }

    fn terminal_response() -> ToolCompletionResponse {
        ToolCompletionResponse {
            content: Some(format!(
                "```repl\nFINAL('{AUTH_TERMINAL_RESPONSE}')\n```"
            )),
            tool_calls: Vec::new(),
            input_tokens: 1,
            output_tokens: 1,
            finish_reason: FinishReason::Stop,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        }
    }

    fn unexpected_step(call: usize, request: &ToolCompletionRequest) -> LlmError {
        LlmError::RequestFailed {
            provider: "mcp-auth-compat-test".into(),
            reason: format!(
                "unexpected auth step {call}; actions={:?}; tool_results={:?}",
                request
                    .tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>(),
                request
                    .messages
                    .iter()
                    .filter(|message| message.role == Role::Tool)
                    .map(|message| (message.tool_call_id.as_deref(), message.content.as_str()))
                    .collect::<Vec<_>>()
            ),
        }
    }
}

#[async_trait]
impl LlmProvider for McpAuthLlm {
    fn model_name(&self) -> &str {
        "mcp-auth-compat-test"
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
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match call {
            0 if Self::advertises(&request, "tool_activate") => Ok(Self::tool_response(
                ACTIVATE_CALL_ID,
                "tool_activate",
                serde_json::json!({"name": SERVER_NAME}),
            )),
            1 if Self::has_tool_result(&request, ACTIVATE_CALL_ID)
                && Self::advertises(&request, ENGINE_ACTION_NAME) =>
            {
                Ok(Self::tool_response(
                    AUTH_MCP_CALL_ID,
                    ENGINE_ACTION_NAME,
                    serde_json::json!({"query": "after-auth"}),
                ))
            }
            2 if self.has_successful_mcp_result(&request) => Ok(Self::terminal_response()),
            _ => Err(Self::unexpected_step(call, &request)),
        }
    }

    async fn complete_with_tools_stream(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = match call {
            0 if Self::advertises(&request, "tool_activate") => tool_call_chunks(
                ACTIVATE_CALL_ID,
                "tool_activate",
                serde_json::json!({"name": SERVER_NAME}),
            ),
            1 if Self::has_tool_result(&request, ACTIVATE_CALL_ID)
                && Self::advertises(&request, ENGINE_ACTION_NAME) =>
            {
                tool_call_chunks(
                    AUTH_MCP_CALL_ID,
                    ENGINE_ACTION_NAME,
                    serde_json::json!({"query": "after-auth"}),
                )
            }
            2 if self.has_successful_mcp_result(&request) => vec![
                Ok(LlmStreamChunk::TextDelta(format!(
                    "```repl\nFINAL('{AUTH_TERMINAL_RESPONSE}')\n```"
                ))),
                Ok(LlmStreamChunk::Done {
                    usage: Some(TokenUsage::default()),
                    finish_reason: "stop".to_string(),
                }),
            ],
            _ => return Err(Self::unexpected_step(call, &request)),
        };
        Ok(futures::stream::iter(chunks).boxed())
    }
}

fn tool_call_chunks(
    call_id: &str,
    name: &str,
    arguments: serde_json::Value,
) -> Vec<Result<LlmStreamChunk, LlmError>> {
    vec![
        Ok(LlmStreamChunk::ToolCallDelta {
            index: 0,
            id: Some(call_id.to_string()),
            name: Some(name.to_string()),
            args_delta: arguments.to_string(),
        }),
        Ok(LlmStreamChunk::Done {
            usage: Some(TokenUsage::default()),
            finish_reason: "tool_calls".to_string(),
        }),
    ]
}
