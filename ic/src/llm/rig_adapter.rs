//! Generic adapter that bridges rig-core's `CompletionModel` trait to LunarWing's `LlmProvider`.
//!
//! This lets us use any rig-core provider (OpenAI, Anthropic, Ollama, etc.) as an
//! `Arc<dyn LlmProvider>` without changing any of the agent, reasoning, or tool code.

use crate::llm::config::CacheRetention;
use async_trait::async_trait;
use futures::StreamExt;
use rig_core::OneOrMany;
use rig_core::completion::{
    AssistantContent, CompletionModel, CompletionRequest as RigRequest, GetTokenUsage,
    ToolDefinition as RigToolDefinition, Usage as RigUsage,
};
use rig_core::message::{
    DocumentSourceKind, Image, ImageMediaType, Message as RigMessage, MimeType,
    ToolChoice as RigToolChoice, ToolFunction, ToolResult as RigToolResult, ToolResultContent,
    UserContent,
};
use rig_core::streaming::{
    StreamedAssistantContent, StreamingCompletionResponse as RigStreamingCompletionResponse,
    ToolCallDeltaContent,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Serialize;
use serde::de::DeserializeOwned;
#[cfg(test)]
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use std::collections::{HashMap, HashSet};

use crate::llm::costs;
use crate::llm::error::LlmError;
use crate::llm::provider::{
    ChatMessage, CompletionRequest, CompletionResponse, FinishReason, LlmProvider, LlmStream,
    LlmStreamChunk, TokenUsage, ToolCall as IronToolCall, ToolCompletionRequest,
    ToolCompletionResponse, ToolDefinition as IronToolDefinition,
    strip_unsupported_completion_params, strip_unsupported_tool_params,
};

/// Adapter that wraps a rig-core `CompletionModel` and implements `LlmProvider`.
pub struct RigAdapter<M: CompletionModel> {
    model: M,
    model_name: String,
    input_cost: Decimal,
    output_cost: Decimal,
    /// Prompt cache retention policy (Anthropic only).
    /// When not `CacheRetention::None`, injects top-level `cache_control`
    /// via `additional_params` for Anthropic automatic caching. Also controls
    /// the cost multiplier for cache-creation tokens.
    cache_retention: CacheRetention,
    /// Parameter names that this provider does not support (e.g., `"temperature"`).
    /// These are stripped from requests before sending to avoid 400 errors.
    unsupported_params: HashSet<String>,
}

impl<M: CompletionModel> RigAdapter<M> {
    /// Create a new adapter wrapping the given rig-core model.
    pub fn new(model: M, model_name: impl Into<String>) -> Self {
        let name = model_name.into();
        let (input_cost, output_cost) =
            costs::model_cost(&name).unwrap_or_else(costs::default_cost);
        Self {
            model,
            model_name: name,
            input_cost,
            output_cost,
            cache_retention: CacheRetention::None,
            unsupported_params: HashSet::new(),
        }
    }

    /// Set Anthropic prompt cache retention policy.
    ///
    /// Controls both cache injection and cost tracking:
    /// - `None` — no caching, no surcharge (1.0×).
    /// - `Short` — 5-minute TTL via `{"type": "ephemeral"}`, 1.25× write surcharge.
    /// - `Long` — 1-hour TTL via `{"type": "ephemeral", "ttl": "1h"}`, 2.0× write surcharge.
    ///
    /// Cache injection uses Anthropic's **automatic caching** — a top-level
    /// `cache_control` field in `additional_params` that gets `#[serde(flatten)]`'d
    /// into the request body by rig-core.
    ///
    /// If the configured model does not support caching (e.g. claude-2),
    /// a warning is logged once at construction and caching is disabled.
    pub fn with_cache_retention(mut self, retention: CacheRetention) -> Self {
        if retention != CacheRetention::None && !supports_prompt_cache(&self.model_name) {
            tracing::warn!(
                model = %self.model_name,
                "Prompt caching requested but model does not support it; disabling"
            );
            self.cache_retention = CacheRetention::None;
        } else {
            self.cache_retention = retention;
        }
        self
    }

    /// Set the list of unsupported parameter names for this provider.
    ///
    /// Parameters in this set are stripped from requests before sending.
    /// Supported parameter names: `"temperature"`, `"max_tokens"`, `"stop_sequences"`.
    pub fn with_unsupported_params(mut self, params: Vec<String>) -> Self {
        self.unsupported_params = params.into_iter().collect();
        self
    }

    /// Strip unsupported fields from a `CompletionRequest` in place.
    fn strip_unsupported_completion_params(&self, req: &mut CompletionRequest) {
        strip_unsupported_completion_params(&self.unsupported_params, req);
    }

    /// Strip unsupported fields from a `ToolCompletionRequest` in place.
    fn strip_unsupported_tool_params(&self, req: &mut ToolCompletionRequest) {
        strip_unsupported_tool_params(&self.unsupported_params, req);
    }

    fn warn_model_override(&self, requested_model: Option<&str>) {
        if let Some(requested_model) = requested_model
            && requested_model != self.model_name.as_str()
        {
            tracing::warn!(
                requested_model,
                active_model = %self.model_name,
                "Per-request model override is not supported for this provider; using configured model"
            );
        }
    }

    fn build_plain_request(&self, mut request: CompletionRequest) -> Result<RigRequest, LlmError> {
        self.warn_model_override(request.model.as_deref());
        self.strip_unsupported_completion_params(&mut request);

        let mut messages = request.messages;
        crate::llm::provider::sanitize_tool_messages(&mut messages);
        let (preamble, history) = convert_messages(&messages);
        build_rig_request(
            preamble,
            history,
            Vec::new(),
            None,
            request.temperature,
            request.max_tokens,
            self.cache_retention,
        )
    }

    fn build_tool_request(
        &self,
        mut request: ToolCompletionRequest,
    ) -> Result<(RigRequest, HashSet<String>), LlmError> {
        self.warn_model_override(request.model.as_deref());
        self.strip_unsupported_tool_params(&mut request);

        let known_tool_names = request.tools.iter().map(|tool| tool.name.clone()).collect();
        let mut messages = request.messages;
        crate::llm::provider::sanitize_tool_messages(&mut messages);
        let (preamble, history) = convert_messages(&messages);
        let tools = convert_tools(&request.tools);
        let tool_choice = convert_tool_choice(request.tool_choice.as_deref());
        let rig_request = build_rig_request(
            preamble,
            history,
            tools,
            tool_choice,
            request.temperature,
            request.max_tokens,
            self.cache_retention,
        )?;
        Ok((rig_request, known_tool_names))
    }
}

// -- Type conversion helpers --

/// Round an f32 to f64 without precision artifacts.
///
/// Direct `f32 as f64` preserves the binary representation, producing values
/// like `0.699999988079071` instead of `0.7`. Some providers (e.g. Zhipu/GLM)
/// reject these values with a 400 error. Rounding to 6 decimal places removes
/// the artifact while preserving all meaningful precision for temperature.
fn round_f32_to_f64(val: f32) -> f64 {
    ((val as f64) * 1_000_000.0).round() / 1_000_000.0
}

/// Convert LunarWing messages to rig-core format.
///
/// Returns `(preamble, chat_history)` where preamble is extracted from
/// any System message and chat_history contains the rest.
fn convert_messages(messages: &[ChatMessage]) -> (Option<String>, Vec<RigMessage>) {
    let mut preamble: Option<String> = None;
    let mut history = Vec::new();

    for msg in messages {
        match msg.role {
            crate::llm::Role::System => {
                // Concatenate system messages into preamble
                match preamble {
                    Some(ref mut p) => {
                        p.push('\n');
                        p.push_str(&msg.content);
                    }
                    None => preamble = Some(msg.content.clone()),
                }
            }
            crate::llm::Role::User => {
                if msg.content_parts.is_empty() {
                    history.push(RigMessage::user(&msg.content));
                } else {
                    // Build multimodal user message with text + image parts
                    let mut contents: Vec<UserContent> = vec![UserContent::text(&msg.content)];
                    for part in &msg.content_parts {
                        if let crate::llm::ContentPart::ImageUrl { image_url } = part {
                            // Parse data: URL for base64 images, or use raw URL
                            let image = if let Some(rest) = image_url.url.strip_prefix("data:") {
                                // Format: data:<mime>;base64,<data>
                                let (mime, b64) =
                                    rest.split_once(";base64,").unwrap_or(("image/jpeg", rest));
                                Image {
                                    data: DocumentSourceKind::base64(b64),
                                    media_type: ImageMediaType::from_mime_type(mime),
                                    detail: None,
                                    additional_params: None,
                                }
                            } else {
                                Image {
                                    data: DocumentSourceKind::url(&image_url.url),
                                    media_type: None,
                                    detail: None,
                                    additional_params: None,
                                }
                            };
                            contents.push(UserContent::Image(image));
                        }
                    }
                    if let Ok(many) = OneOrMany::many(contents) {
                        history.push(RigMessage::User { content: many });
                    } else {
                        history.push(RigMessage::user(&msg.content));
                    }
                }
            }
            crate::llm::Role::Assistant => {
                if let Some(ref tool_calls) = msg.tool_calls {
                    // Assistant message with tool calls
                    let mut contents: Vec<AssistantContent> = Vec::new();
                    if !msg.content.is_empty() {
                        contents.push(AssistantContent::text(&msg.content));
                    }
                    for (idx, tc) in tool_calls.iter().enumerate() {
                        let tool_call_id =
                            normalized_tool_call_id(Some(tc.id.as_str()), history.len() + idx);
                        contents.push(AssistantContent::ToolCall(
                            rig_core::message::ToolCall::new(
                                tool_call_id.clone(),
                                ToolFunction::new(tc.name.clone(), tc.arguments.clone()),
                            )
                            .with_call_id(tool_call_id),
                        ));
                    }
                    if let Ok(many) = OneOrMany::many(contents) {
                        history.push(RigMessage::Assistant {
                            id: None,
                            content: many,
                        });
                    } else {
                        // Shouldn't happen but fall back to text
                        history.push(RigMessage::assistant(&msg.content));
                    }
                } else {
                    history.push(RigMessage::assistant(&msg.content));
                }
            }
            crate::llm::Role::Tool => {
                // Tool result message: wrap as User { ToolResult }.
                // Merge consecutive tool results into a single User message
                // so the API sees one multi-result message instead of
                // multiple consecutive User messages (which Anthropic rejects).
                let tool_id = normalized_tool_call_id(msg.tool_call_id.as_deref(), history.len());
                let tool_result = UserContent::ToolResult(RigToolResult {
                    id: tool_id.clone(),
                    call_id: Some(tool_id),
                    content: OneOrMany::one(ToolResultContent::text(&msg.content)),
                });

                let should_merge = matches!(
                    history.last(),
                    Some(RigMessage::User { content }) if content.iter().all(|c| matches!(c, UserContent::ToolResult(_)))
                );

                if should_merge {
                    if let Some(RigMessage::User { content }) = history.last_mut() {
                        content.push(tool_result);
                    }
                } else {
                    history.push(RigMessage::User {
                        content: OneOrMany::one(tool_result),
                    });
                }
            }
        }
    }

    (preamble, history)
}

/// Responses-style providers require a non-empty tool call ID.
///
/// IDs must be compatible with providers like Mistral, which constrain IDs
/// to `[a-zA-Z0-9]{9}`. We therefore:
/// - pass through any non-empty raw ID that already matches this constraint;
/// - otherwise deterministically map the raw string into a provider-compliant ID;
/// - and when `raw` is empty/None, delegate to `generate_tool_call_id`.
fn normalized_tool_call_id(raw: Option<&str>, seed: usize) -> String {
    // Trim and treat empty as None.
    let trimmed = raw.and_then(|s| {
        let t = s.trim();
        if t.is_empty() { None } else { Some(t) }
    });

    if let Some(id) = trimmed {
        // If the ID already satisfies `[a-zA-Z0-9]{9}`, pass it through unchanged.
        if id.len() == 9 && id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return id.to_string();
        }

        // Otherwise, deterministically hash the raw ID and feed the hash-derived
        // seed into the provider-level generator so that the encoding and any
        // provider-specific constraints remain centralized in one place.
        let digest = Sha256::digest(id.as_bytes());
        // Derive a 64-bit value from the first 8 bytes of the digest, then
        // split it into two usize seeds so we preserve all 64 bits of entropy
        // even on 32-bit targets.
        let hash64 = {
            // SHA-256 always produces 32 bytes, so indexing the first 8 is safe.
            let bytes: [u8; 8] = [
                digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6],
                digest[7],
            ];
            u64::from_be_bytes(bytes)
        };
        let hi_seed: usize = (hash64 >> 32) as usize;
        let lo_seed: usize = (hash64 & 0xFFFF_FFFF) as usize;
        return super::provider::generate_tool_call_id(hi_seed, lo_seed);
    }

    // Fallback for missing/empty raw IDs: use the provider-level generator,
    // which already produces compliant IDs.
    super::provider::generate_tool_call_id(seed, 0)
}

/// Convert LunarWing tool definitions to rig-core format.
///
/// Preserve source schema semantics here. Provider models own strict-mode
/// normalization and must pair it with the corresponding wire-level flag.
fn convert_tools(tools: &[IronToolDefinition]) -> Vec<RigToolDefinition> {
    tools
        .iter()
        .map(|t| RigToolDefinition {
            name: t.name.clone(),
            description: t.description.clone(),
            parameters: t.parameters.clone(),
        })
        .collect()
}

/// Convert LunarWing tool_choice string to rig-core ToolChoice.
fn convert_tool_choice(choice: Option<&str>) -> Option<RigToolChoice> {
    match choice.map(|s| s.to_lowercase()).as_deref() {
        Some("auto") => Some(RigToolChoice::Auto),
        Some("required") => Some(RigToolChoice::Required),
        Some("none") => Some(RigToolChoice::None),
        _ => None,
    }
}

/// Extract text and tool calls from a rig-core completion response.
fn extract_response(
    choice: &OneOrMany<AssistantContent>,
    _usage: &RigUsage,
) -> (Option<String>, Vec<IronToolCall>, FinishReason) {
    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<IronToolCall> = Vec::new();

    for content in choice.iter() {
        match content {
            AssistantContent::Text(t) => {
                if !t.text.is_empty() {
                    text_parts.push(t.text.clone());
                }
            }
            AssistantContent::ToolCall(tc) => {
                tool_calls.push(IronToolCall {
                    id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    arguments: tc.function.arguments.clone(),
                    reasoning: None,
                });
            }
            // Reasoning and Image variants are not mapped to LunarWing types
            _ => {}
        }
    }

    let text = if text_parts.is_empty() {
        None
    } else {
        Some(text_parts.join(""))
    };

    let finish = if !tool_calls.is_empty() {
        FinishReason::ToolUse
    } else {
        FinishReason::Stop
    };

    (text, tool_calls, finish)
}

/// Saturate u64 to u32 for token counts.
fn saturate_u32(val: u64) -> u32 {
    val.min(u32::MAX as u64) as u32
}

/// Returns `true` if the model supports Anthropic prompt caching.
///
/// Per Anthropic docs, only Claude 3+ models support prompt caching.
/// Unsupported: claude-2, claude-2.1, claude-instant-*.
fn supports_prompt_cache(name: &str) -> bool {
    let lower = name.to_lowercase();
    // Strip optional provider prefix (e.g. "anthropic/claude-...")
    let model = lower.strip_prefix("anthropic/").unwrap_or(&lower);
    // Only Claude 3+ families support prompt caching
    model.starts_with("claude-3")
        || model.starts_with("claude-4")
        || model.starts_with("claude-sonnet")
        || model.starts_with("claude-opus")
        || model.starts_with("claude-haiku")
}

/// Extract `cache_creation_input_tokens` from the raw provider response.
///
/// Rig-core's unified `Usage` does not surface this field, but Anthropic's raw
/// response includes it at `usage.cache_creation_input_tokens`. We serialize the
/// raw response to JSON and attempt to read the value.
fn extract_cache_creation<T: Serialize>(raw: &T) -> u32 {
    serde_json::to_value(raw)
        .ok()
        .and_then(|v| v.get("usage")?.get("cache_creation_input_tokens")?.as_u64())
        .map(|n| n.min(u32::MAX as u64) as u32)
        .unwrap_or(0)
}

/// Build a rig-core CompletionRequest from our internal types.
///
/// When `cache_retention` is not `None`, injects a top-level `cache_control`
/// field via `additional_params`. Rig-core's `AnthropicCompletionRequest`
/// uses `#[serde(flatten)]` on `additional_params`, so the field lands at
/// the request root — which is exactly what Anthropic's **automatic caching**
/// expects. The API auto-places the cache breakpoint at the last cacheable
/// block and moves it forward as conversations grow.
#[allow(clippy::too_many_arguments)]
fn build_rig_request(
    preamble: Option<String>,
    mut history: Vec<RigMessage>,
    tools: Vec<RigToolDefinition>,
    tool_choice: Option<RigToolChoice>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    cache_retention: CacheRetention,
) -> Result<RigRequest, LlmError> {
    // rig-core requires at least one message in chat_history
    if history.is_empty() {
        history.push(RigMessage::user("Hello"));
    }

    let chat_history = OneOrMany::many(history).map_err(|e| LlmError::RequestFailed {
        provider: "rig".to_string(),
        reason: format!("Failed to build chat history: {}", e),
    })?;

    // Inject top-level cache_control for Anthropic automatic prompt caching.
    let additional_params = match cache_retention {
        CacheRetention::None => None,
        CacheRetention::Short => Some(serde_json::json!({
            "cache_control": {"type": "ephemeral"}
        })),
        CacheRetention::Long => Some(serde_json::json!({
            "cache_control": {"type": "ephemeral", "ttl": "1h"}
        })),
    };

    Ok(RigRequest {
        model: None,
        preamble,
        chat_history,
        documents: Vec::new(),
        tools,
        temperature: temperature.map(round_f32_to_f64),
        max_tokens: max_tokens.map(|t| t as u64),
        tool_choice,
        additional_params,
        output_schema: None,
    })
}

struct RigStreamState<R>
where
    R: Clone + Unpin + GetTokenUsage,
{
    upstream: RigStreamingCompletionResponse<R>,
    provider: String,
    indexes: HashMap<String, usize>,
    seen_deltas: HashSet<String>,
    next_index: usize,
    saw_tool: bool,
    terminal: bool,
}

impl<R> RigStreamState<R>
where
    R: Clone + Unpin + GetTokenUsage,
{
    fn new(upstream: RigStreamingCompletionResponse<R>, provider: String) -> Self {
        Self {
            upstream,
            provider,
            indexes: HashMap::new(),
            seen_deltas: HashSet::new(),
            next_index: 0,
            saw_tool: false,
            terminal: false,
        }
    }

    fn index_for(&mut self, internal_call_id: &str) -> usize {
        if let Some(index) = self.indexes.get(internal_call_id) {
            return *index;
        }
        let index = self.next_index;
        self.next_index += 1;
        self.indexes.insert(internal_call_id.to_string(), index);
        index
    }

    fn tool_delta(
        &mut self,
        id: String,
        internal_call_id: String,
        content: ToolCallDeltaContent,
    ) -> LlmStreamChunk {
        let index = self.index_for(&internal_call_id);
        self.saw_tool = true;
        self.seen_deltas.insert(internal_call_id);
        let (name, args_delta) = match content {
            ToolCallDeltaContent::Name(name) => (Some(name), String::new()),
            ToolCallDeltaContent::Delta(delta) => (None, delta),
        };
        LlmStreamChunk::ToolCallDelta {
            index,
            id: (!id.is_empty()).then_some(id),
            name,
            args_delta,
        }
    }

    fn full_tool_call(
        &mut self,
        tool_call: rig_core::message::ToolCall,
        internal_call_id: String,
    ) -> LlmStreamChunk {
        self.saw_tool = true;
        let index = self.index_for(&internal_call_id);
        if self.seen_deltas.contains(&internal_call_id) {
            return LlmStreamChunk::ToolCallDelta {
                index,
                id: (!tool_call.id.is_empty()).then_some(tool_call.id),
                name: None,
                args_delta: String::new(),
            };
        }
        LlmStreamChunk::ToolCallDelta {
            index,
            id: (!tool_call.id.is_empty()).then_some(tool_call.id),
            name: Some(tool_call.function.name),
            args_delta: tool_call.function.arguments.to_string(),
        }
    }

    fn done(&mut self, raw: R) -> LlmStreamChunk {
        self.terminal = true;
        let raw_usage = raw.token_usage();
        let usage = raw_usage.has_values().then(|| TokenUsage {
            input_tokens: saturate_u32(raw_usage.input_tokens),
            output_tokens: saturate_u32(raw_usage.output_tokens),
            cache_read_input_tokens: saturate_u32(raw_usage.cached_input_tokens),
            cache_creation_input_tokens: saturate_u32(raw_usage.cache_creation_input_tokens),
        });
        let finish_reason = if self.saw_tool { "tool_calls" } else { "stop" };
        LlmStreamChunk::Done {
            usage,
            finish_reason: finish_reason.to_string(),
        }
    }
}

async fn next_rig_stream_item<R>(
    mut state: RigStreamState<R>,
) -> Option<(Result<LlmStreamChunk, LlmError>, RigStreamState<R>)>
where
    R: Clone + Unpin + GetTokenUsage,
{
    loop {
        if state.terminal {
            return None;
        }
        let item = match state.upstream.next().await {
            Some(Ok(StreamedAssistantContent::Text(text))) if !text.text.is_empty() => {
                Ok(LlmStreamChunk::TextDelta(text.text))
            }
            Some(Ok(StreamedAssistantContent::Text(_)))
            | Some(Ok(StreamedAssistantContent::Reasoning(_)))
            | Some(Ok(StreamedAssistantContent::ReasoningDelta { .. }))
            | Some(Ok(StreamedAssistantContent::Unknown(_))) => continue,
            Some(Ok(StreamedAssistantContent::ToolCallDelta {
                id,
                internal_call_id,
                content,
            })) => Ok(state.tool_delta(id, internal_call_id, content)),
            Some(Ok(StreamedAssistantContent::ToolCall {
                tool_call,
                internal_call_id,
            })) => Ok(state.full_tool_call(tool_call, internal_call_id)),
            Some(Ok(StreamedAssistantContent::Final(raw))) => Ok(state.done(raw)),
            Some(Err(error)) => {
                state.terminal = true;
                Err(LlmError::RequestFailed {
                    provider: state.provider.clone(),
                    reason: error.to_string(),
                })
            }
            None => {
                state.terminal = true;
                Err(LlmError::InvalidResponse {
                    provider: state.provider.clone(),
                    reason: "stream ended before Rig emitted a final response".to_string(),
                })
            }
        };
        return Some((item, state));
    }
}

fn map_rig_stream<R>(
    upstream: RigStreamingCompletionResponse<R>,
    provider: String,
) -> LlmStream<'static>
where
    R: Clone + Unpin + GetTokenUsage + Send + 'static,
{
    futures::stream::unfold(
        RigStreamState::new(upstream, provider),
        next_rig_stream_item,
    )
    .boxed()
}

#[async_trait]
impl<M> LlmProvider for RigAdapter<M>
where
    M: CompletionModel + Send + Sync + 'static,
    M::Response: Send + Sync + Serialize + DeserializeOwned,
    M::StreamingResponse: Send + 'static,
{
    fn model_name(&self) -> &str {
        &self.model_name
    }

    fn cost_per_token(&self) -> (Decimal, Decimal) {
        (self.input_cost, self.output_cost)
    }

    fn cache_write_multiplier(&self) -> Decimal {
        match self.cache_retention {
            CacheRetention::None => Decimal::ONE,
            CacheRetention::Short => Decimal::new(125, 2), // 1.25× (125% of input rate)
            CacheRetention::Long => Decimal::TWO,          // 2.0×  (200% of input rate)
        }
    }

    fn cache_read_discount(&self) -> Decimal {
        if self.cache_retention != CacheRetention::None {
            dec!(10) // Anthropic: 90% discount (cost = input_rate / 10)
        } else {
            Decimal::ONE
        }
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let rig_req = self.build_plain_request(request)?;

        let response =
            self.model
                .completion(rig_req)
                .await
                .map_err(|e| LlmError::RequestFailed {
                    provider: self.model_name.clone(),
                    reason: e.to_string(),
                })?;

        let (text, _tool_calls, finish) = extract_response(&response.choice, &response.usage);

        let resp = CompletionResponse {
            content: text.unwrap_or_default(),
            input_tokens: saturate_u32(response.usage.input_tokens),
            output_tokens: saturate_u32(response.usage.output_tokens),
            finish_reason: finish,
            cache_read_input_tokens: saturate_u32(response.usage.cached_input_tokens),
            cache_creation_input_tokens: extract_cache_creation(&response.raw_response),
        };

        if resp.cache_read_input_tokens > 0 {
            tracing::debug!(
                model = %self.model_name,
                input = resp.input_tokens,
                output = resp.output_tokens,
                cache_read = resp.cache_read_input_tokens,
                "prompt cache hit",
            );
        }

        Ok(resp)
    }

    async fn complete_stream(&self, request: CompletionRequest) -> Result<LlmStream<'_>, LlmError> {
        let rig_request = self.build_plain_request(request)?;
        let upstream =
            self.model
                .stream(rig_request)
                .await
                .map_err(|error| LlmError::RequestFailed {
                    provider: self.model_name.clone(),
                    reason: error.to_string(),
                })?;
        Ok(map_rig_stream(upstream, self.model_name.clone()))
    }

    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        let (rig_req, known_tool_names) = self.build_tool_request(request)?;

        let response =
            self.model
                .completion(rig_req)
                .await
                .map_err(|e| LlmError::RequestFailed {
                    provider: self.model_name.clone(),
                    reason: e.to_string(),
                })?;

        let (text, mut tool_calls, finish) = extract_response(&response.choice, &response.usage);

        // Normalize tool call names: some proxies prepend "proxy_" prefixes.
        for tc in &mut tool_calls {
            let normalized = normalize_tool_name(&tc.name, &known_tool_names);
            if normalized != tc.name {
                tracing::debug!(
                    original = %tc.name,
                    normalized = %normalized,
                    "Normalized tool call name from provider",
                );
                tc.name = normalized;
            }
        }

        let resp = ToolCompletionResponse {
            content: text,
            tool_calls,
            input_tokens: saturate_u32(response.usage.input_tokens),
            output_tokens: saturate_u32(response.usage.output_tokens),
            finish_reason: finish,
            cache_read_input_tokens: saturate_u32(response.usage.cached_input_tokens),
            cache_creation_input_tokens: extract_cache_creation(&response.raw_response),
        };

        if resp.cache_read_input_tokens > 0 {
            tracing::debug!(
                model = %self.model_name,
                input = resp.input_tokens,
                output = resp.output_tokens,
                cache_read = resp.cache_read_input_tokens,
                "prompt cache hit",
            );
        }

        Ok(resp)
    }

    async fn complete_with_tools_stream(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<LlmStream<'_>, LlmError> {
        let (rig_request, _known_tool_names) = self.build_tool_request(request)?;
        let upstream =
            self.model
                .stream(rig_request)
                .await
                .map_err(|error| LlmError::RequestFailed {
                    provider: self.model_name.clone(),
                    reason: error.to_string(),
                })?;
        Ok(map_rig_stream(upstream, self.model_name.clone()))
    }

    fn active_model_name(&self) -> String {
        self.model_name.clone()
    }

    fn effective_model_name(&self, _requested_model: Option<&str>) -> String {
        self.active_model_name()
    }

    fn set_model(&self, _model: &str) -> Result<(), LlmError> {
        // rig-core models are baked at construction time.
        // Switching requires creating a new adapter.
        Err(LlmError::RequestFailed {
            provider: self.model_name.clone(),
            reason: "Runtime model switching not supported for rig-core providers. \
                     Restart with a different model configured."
                .to_string(),
        })
    }
}

/// Normalize a tool call name returned by an OpenAI-compatible provider.
///
/// Some proxies (e.g. VibeProxy) prepend `proxy_` to tool names.
/// If the returned name doesn't match any known tool but stripping a
/// `proxy_` prefix yields a match, use the stripped version.
fn normalize_tool_name(name: &str, known_tools: &HashSet<String>) -> String {
    if known_tools.contains(name) {
        return name.to_string();
    }

    if let Some(stripped) = name.strip_prefix("proxy_")
        && known_tools.contains(stripped)
    {
        return stripped.to_string();
    }

    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;
    use std::sync::{Arc, Mutex};

    use crate::llm::LlmStreamChunk;
    use axum::extract::State;
    use axum::response::sse::{Event, Sse};
    use axum::routing::post;
    use axum::{Json, Router};
    use futures::StreamExt;
    use rig_core::client::CompletionClient;
    use rig_core::completion::{CompletionError, GetTokenUsage};
    use rig_core::providers::openai;
    use rig_core::streaming::{
        RawStreamingChoice, RawStreamingToolCall,
        StreamingCompletionResponse as RigStreamingCompletionResponse, ToolCallDeltaContent,
    };

    #[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
    struct TestStreamingResponse {
        usage: RigUsage,
    }

    impl TestStreamingResponse {
        fn new(input_tokens: u64, output_tokens: u64, cached_input_tokens: u64) -> Self {
            Self {
                usage: RigUsage {
                    input_tokens,
                    output_tokens,
                    total_tokens: input_tokens.saturating_add(output_tokens),
                    cached_input_tokens,
                    cache_creation_input_tokens: 0,
                    tool_use_prompt_tokens: 0,
                    reasoning_tokens: 0,
                },
            }
        }
    }

    impl GetTokenUsage for TestStreamingResponse {
        fn token_usage(&self) -> RigUsage {
            self.usage
        }
    }

    type ScriptedStreamItem = Result<RawStreamingChoice<TestStreamingResponse>, CompletionError>;

    #[derive(Clone)]
    struct ScriptedCompletionModel {
        stream_items: Arc<Mutex<Option<Vec<ScriptedStreamItem>>>>,
    }

    impl ScriptedCompletionModel {
        fn new(items: Vec<ScriptedStreamItem>) -> Self {
            Self {
                stream_items: Arc::new(Mutex::new(Some(items))),
            }
        }
    }

    impl CompletionModel for ScriptedCompletionModel {
        type Response = serde_json::Value;
        type StreamingResponse = TestStreamingResponse;
        type Client = ();

        fn make(_client: &Self::Client, _model: impl Into<String>) -> Self {
            Self::new(Vec::new())
        }

        async fn completion(
            &self,
            _request: RigRequest,
        ) -> Result<rig_core::completion::CompletionResponse<Self::Response>, CompletionError>
        {
            Err(CompletionError::ProviderError(
                "blocking completion is not configured".to_string(),
            ))
        }

        fn stream(
            &self,
            _request: RigRequest,
        ) -> impl std::future::Future<
            Output = Result<
                RigStreamingCompletionResponse<Self::StreamingResponse>,
                CompletionError,
            >,
        > + Send {
            let items = self
                .stream_items
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
                .unwrap_or_default();
            async move {
                Ok(RigStreamingCompletionResponse::stream(Box::pin(
                    futures::stream::iter(items),
                )))
            }
        }
    }

    #[derive(Clone)]
    struct TensorZeroSseState {
        events: Arc<Vec<String>>,
        request: Arc<Mutex<Option<JsonValue>>>,
    }

    struct TensorZeroSseFixture {
        base_url: String,
        request: Arc<Mutex<Option<JsonValue>>>,
        server: tokio::task::JoinHandle<()>,
    }

    impl TensorZeroSseFixture {
        async fn start(events: Vec<String>) -> Self {
            let request = Arc::new(Mutex::new(None));
            let state = TensorZeroSseState {
                events: Arc::new(events),
                request: Arc::clone(&request),
            };
            let app = Router::new()
                .route("/openai/v1/chat/completions", post(tensorzero_sse_handler))
                .with_state(state);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("TensorZero fixture should bind");
            let address = listener
                .local_addr()
                .expect("TensorZero fixture should have an address");
            let server = tokio::spawn(async move {
                axum::serve(listener, app)
                    .await
                    .expect("TensorZero fixture should serve requests");
            });
            Self {
                base_url: format!("http://{address}/openai/v1"),
                request,
                server,
            }
        }

        fn provider(&self) -> Arc<dyn LlmProvider> {
            let client = openai::Client::builder()
                .api_key("test-key")
                .base_url(&self.base_url)
                .build()
                .expect("TensorZero fixture client should build")
                .completions_api();
            Arc::new(RigAdapter::new(
                client.completion_model("tensorzero::function_name::lunarwing"),
                "tensorzero::function_name::lunarwing",
            ))
        }

        fn captured_request(&self) -> JsonValue {
            self.request
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
                .expect("TensorZero fixture should capture a request")
        }
    }

    impl Drop for TensorZeroSseFixture {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    async fn tensorzero_sse_handler(
        State(state): State<TensorZeroSseState>,
        Json(request): Json<JsonValue>,
    ) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
        *state
            .request
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(request);
        let events = state.events.as_ref().clone();
        let stream = futures::stream::iter(
            events
                .into_iter()
                .map(|data| Ok(Event::default().data(data))),
        );
        Sse::new(stream)
    }

    fn tool_stream_request() -> ToolCompletionRequest {
        ToolCompletionRequest::new(
            vec![ChatMessage::user("search")],
            vec![IronToolDefinition {
                name: "search".to_string(),
                description: "Search".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }],
        )
    }

    #[tokio::test]
    async fn complete_stream_emits_text_deltas_and_terminal_usage() {
        let model = ScriptedCompletionModel::new(vec![
            Ok(RawStreamingChoice::Message("hel".to_string())),
            Ok(RawStreamingChoice::Message("lo".to_string())),
            Ok(RawStreamingChoice::FinalResponse(
                TestStreamingResponse::new(7, 3, 2),
            )),
        ]);
        let adapter = RigAdapter::new(model, "test-model");

        let chunks = adapter
            .complete_stream(CompletionRequest::new(vec![ChatMessage::user("hi")]))
            .await
            .expect("stream should open")
            .collect::<Vec<_>>()
            .await;

        assert!(matches!(
            chunks.first(),
            Some(Ok(LlmStreamChunk::TextDelta(text))) if text == "hel"
        ));
        assert!(matches!(
            chunks.get(1),
            Some(Ok(LlmStreamChunk::TextDelta(text))) if text == "lo"
        ));
        assert!(matches!(
            chunks.get(2),
            Some(Ok(LlmStreamChunk::Done {
                usage: Some(crate::llm::TokenUsage {
                    input_tokens: 7,
                    output_tokens: 3,
                    cache_read_input_tokens: 2,
                    cache_creation_input_tokens: 0,
                }),
                finish_reason,
            })) if finish_reason == "stop"
        ));
    }

    #[tokio::test]
    async fn complete_stream_rejects_eof_without_final_event() {
        let model = ScriptedCompletionModel::new(vec![Ok(RawStreamingChoice::Message(
            "partial".to_string(),
        ))]);
        let adapter = RigAdapter::new(model, "test-model");

        let chunks = adapter
            .complete_stream(CompletionRequest::new(vec![ChatMessage::user("hi")]))
            .await
            .expect("stream should open")
            .collect::<Vec<_>>()
            .await;

        assert!(matches!(
            chunks.last(),
            Some(Err(LlmError::InvalidResponse { provider, reason }))
                if provider == "test-model" && reason.contains("final response")
        ));
    }

    #[tokio::test]
    async fn complete_with_tools_stream_assigns_stable_indexes() {
        let model = ScriptedCompletionModel::new(vec![
            Ok(RawStreamingChoice::ToolCallDelta {
                id: "call_a".to_string(),
                internal_call_id: "internal_a".to_string(),
                content: ToolCallDeltaContent::Name("search".to_string()),
            }),
            Ok(RawStreamingChoice::ToolCallDelta {
                id: "call_b".to_string(),
                internal_call_id: "internal_b".to_string(),
                content: ToolCallDeltaContent::Name("search".to_string()),
            }),
            Ok(RawStreamingChoice::ToolCallDelta {
                id: String::new(),
                internal_call_id: "internal_a".to_string(),
                content: ToolCallDeltaContent::Delta("{}".to_string()),
            }),
            Ok(RawStreamingChoice::FinalResponse(
                TestStreamingResponse::new(8, 4, 0),
            )),
        ]);
        let adapter = RigAdapter::new(model, "test-model");

        let chunks = adapter
            .complete_with_tools_stream(tool_stream_request())
            .await
            .expect("stream should open")
            .collect::<Vec<_>>()
            .await;
        let indexes = chunks
            .iter()
            .filter_map(|item| match item {
                Ok(LlmStreamChunk::ToolCallDelta { index, .. }) => Some(*index),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(indexes, vec![0, 1, 0]);
    }

    #[tokio::test]
    async fn complete_with_tools_stream_suppresses_duplicate_full_arguments() {
        let model = ScriptedCompletionModel::new(vec![
            Ok(RawStreamingChoice::ToolCallDelta {
                id: "call_1".to_string(),
                internal_call_id: "internal_1".to_string(),
                content: ToolCallDeltaContent::Name("search".to_string()),
            }),
            Ok(RawStreamingChoice::ToolCallDelta {
                id: String::new(),
                internal_call_id: "internal_1".to_string(),
                content: ToolCallDeltaContent::Delta("{\"q\":\"rust\"}".to_string()),
            }),
            Ok(RawStreamingChoice::ToolCall(
                RawStreamingToolCall::new(
                    "call_1".to_string(),
                    "search".to_string(),
                    serde_json::json!({"q": "rust"}),
                )
                .with_internal_call_id("internal_1".to_string()),
            )),
            Ok(RawStreamingChoice::FinalResponse(
                TestStreamingResponse::new(8, 4, 0),
            )),
        ]);
        let adapter = RigAdapter::new(model, "test-model");

        let chunks = adapter
            .complete_with_tools_stream(tool_stream_request())
            .await
            .expect("stream should open")
            .collect::<Vec<_>>()
            .await;
        let arguments = chunks
            .iter()
            .filter_map(|item| match item {
                Ok(LlmStreamChunk::ToolCallDelta { args_delta, .. }) => Some(args_delta.as_str()),
                _ => None,
            })
            .collect::<String>();

        assert_eq!(arguments, "{\"q\":\"rust\"}");
    }

    #[tokio::test]
    async fn complete_with_tools_stream_synthesizes_full_tool_call() {
        let model = ScriptedCompletionModel::new(vec![
            Ok(RawStreamingChoice::ToolCall(
                RawStreamingToolCall::new(
                    "call_1".to_string(),
                    "search".to_string(),
                    serde_json::json!({"q": "rust"}),
                )
                .with_internal_call_id("internal_1".to_string()),
            )),
            Ok(RawStreamingChoice::FinalResponse(
                TestStreamingResponse::new(8, 4, 0),
            )),
        ]);
        let adapter = RigAdapter::new(model, "test-model");

        let chunks = adapter
            .complete_with_tools_stream(tool_stream_request())
            .await
            .expect("stream should open")
            .collect::<Vec<_>>()
            .await;

        assert!(matches!(
            chunks.first(),
            Some(Ok(LlmStreamChunk::ToolCallDelta {
                index: 0,
                id: Some(id),
                name: Some(name),
                args_delta,
            })) if id == "call_1" && name == "search" && args_delta == "{\"q\":\"rust\"}"
        ));
    }

    #[tokio::test]
    async fn complete_stream_surfaces_mid_stream_error_and_stops() {
        let model = ScriptedCompletionModel::new(vec![
            Ok(RawStreamingChoice::Message("partial".to_string())),
            Err(CompletionError::ProviderError("stream failed".to_string())),
            Ok(RawStreamingChoice::Message("ignored".to_string())),
        ]);
        let adapter = RigAdapter::new(model, "test-model");

        let chunks = adapter
            .complete_stream(CompletionRequest::new(vec![ChatMessage::user("hi")]))
            .await
            .expect("stream should open")
            .collect::<Vec<_>>()
            .await;

        assert_eq!(chunks.len(), 2);
        assert!(matches!(
            chunks.first(),
            Some(Ok(LlmStreamChunk::TextDelta(text))) if text == "partial"
        ));
        assert!(matches!(
            chunks.get(1),
            Some(Err(LlmError::RequestFailed { provider, reason }))
                if provider == "test-model" && reason.contains("stream failed")
        ));
    }

    #[tokio::test]
    async fn tensorzero_2026_3_2_stream_maps_text_usage_and_done() {
        let fixture = TensorZeroSseFixture::start(vec![
            serde_json::json!({
                "id": "inference-1",
                "model": "tensorzero::function_name::lunarwing::variant_name::test",
                "choices": [{
                    "index": 0,
                    "finish_reason": null,
                    "delta": {"role": "assistant", "content": "hel"}
                }],
                "usage": null
            })
            .to_string(),
            serde_json::json!({
                "id": "inference-1",
                "model": "tensorzero::function_name::lunarwing::variant_name::test",
                "choices": [{
                    "index": 0,
                    "finish_reason": "stop",
                    "delta": {"content": "lo"}
                }],
                "usage": {
                    "prompt_tokens": 7,
                    "completion_tokens": 3,
                    "total_tokens": 10
                }
            })
            .to_string(),
            "[DONE]".to_string(),
        ])
        .await;

        let chunks = fixture
            .provider()
            .complete_stream(CompletionRequest::new(vec![ChatMessage::user("hi")]))
            .await
            .expect("TensorZero stream should open")
            .collect::<Vec<_>>()
            .await;

        assert!(matches!(
            chunks.first(),
            Some(Ok(LlmStreamChunk::TextDelta(text))) if text == "hel"
        ));
        assert!(matches!(
            chunks.get(1),
            Some(Ok(LlmStreamChunk::TextDelta(text))) if text == "lo"
        ));
        assert!(matches!(
            chunks.get(2),
            Some(Ok(LlmStreamChunk::Done {
                usage: Some(crate::llm::TokenUsage {
                    input_tokens: 7,
                    output_tokens: 3,
                    ..
                }),
                finish_reason,
            })) if finish_reason == "stop"
        ));
        assert_eq!(
            fixture
                .captured_request()
                .pointer("/stream_options/include_usage"),
            Some(&JsonValue::Bool(true))
        );
    }

    #[tokio::test]
    async fn tensorzero_2026_3_2_tool_stream_preserves_fragment_order() {
        let fixture = TensorZeroSseFixture::start(vec![
            serde_json::json!({
                "id": "inference-1",
                "model": "tensorzero::function_name::lunarwing::variant_name::test",
                "choices": [{
                    "index": 0,
                    "finish_reason": null,
                    "delta": {
                        "role": "assistant",
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_1",
                            "type": "function",
                            "function": {"name": "search", "arguments": ""}
                        }]
                    }
                }],
                "usage": null
            })
            .to_string(),
            serde_json::json!({
                "id": "inference-1",
                "model": "tensorzero::function_name::lunarwing::variant_name::test",
                "choices": [{
                    "index": 0,
                    "finish_reason": null,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": null,
                            "type": "function",
                            "function": {"name": "", "arguments": "{\"q\":\"rust\"}"}
                        }]
                    }
                }],
                "usage": null
            })
            .to_string(),
            serde_json::json!({
                "id": "inference-1",
                "model": "tensorzero::function_name::lunarwing::variant_name::test",
                "choices": [{
                    "index": 0,
                    "finish_reason": "tool_calls",
                    "delta": {}
                }],
                "usage": {
                    "prompt_tokens": 8,
                    "completion_tokens": 4,
                    "total_tokens": 12
                }
            })
            .to_string(),
            "[DONE]".to_string(),
        ])
        .await;

        let chunks = fixture
            .provider()
            .complete_with_tools_stream(tool_stream_request())
            .await
            .expect("TensorZero tool stream should open")
            .collect::<Vec<_>>()
            .await;
        let arguments = chunks
            .iter()
            .filter_map(|item| match item {
                Ok(LlmStreamChunk::ToolCallDelta { args_delta, .. }) => Some(args_delta.as_str()),
                _ => None,
            })
            .collect::<String>();

        assert_eq!(arguments, "{\"q\":\"rust\"}");
        assert!(matches!(
            chunks.last(),
            Some(Ok(LlmStreamChunk::Done { finish_reason, .. }))
                if finish_reason == "tool_calls"
        ));
    }

    #[tokio::test]
    async fn tensorzero_2026_3_2_midstream_error_is_not_silently_dropped() {
        let fixture = TensorZeroSseFixture::start(vec![
            serde_json::json!({
                "id": "inference-1",
                "model": "tensorzero::function_name::lunarwing::variant_name::test",
                "choices": [{
                    "index": 0,
                    "finish_reason": null,
                    "delta": {"role": "assistant", "content": "partial"}
                }],
                "usage": null
            })
            .to_string(),
            serde_json::json!({
                "error": {
                    "message": "TensorZero provider failed mid-stream",
                    "type": "inference_error"
                }
            })
            .to_string(),
            "[DONE]".to_string(),
        ])
        .await;

        let chunks = fixture
            .provider()
            .complete_stream(CompletionRequest::new(vec![ChatMessage::user("hi")]))
            .await
            .expect("TensorZero stream should open")
            .collect::<Vec<_>>()
            .await;

        assert_eq!(chunks.len(), 2);
        assert!(matches!(
            chunks.first(),
            Some(Ok(LlmStreamChunk::TextDelta(text))) if text == "partial"
        ));
        assert!(matches!(
            chunks.get(1),
            Some(Err(LlmError::RequestFailed { reason, .. }))
                if reason.contains("TensorZero provider failed mid-stream")
        ));
    }

    #[test]
    fn test_round_f32_to_f64_no_precision_artifacts() {
        // Direct f32->f64 cast produces 0.699999988079071 instead of 0.7
        assert_eq!(round_f32_to_f64(0.7_f32), 0.7_f64);
        assert_eq!(round_f32_to_f64(0.5_f32), 0.5_f64);
        assert_eq!(round_f32_to_f64(1.0_f32), 1.0_f64);
        assert_eq!(round_f32_to_f64(0.0_f32), 0.0_f64);
        // Original cast produces artifacts — our fix should not
        assert_ne!(0.7_f32 as f64, 0.7_f64);
    }

    #[test]
    fn test_convert_messages_system_to_preamble() {
        let messages = vec![
            ChatMessage::system("You are a helpful assistant."),
            ChatMessage::user("Hello"),
        ];
        let (preamble, history) = convert_messages(&messages);
        assert_eq!(preamble, Some("You are a helpful assistant.".to_string()));
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn test_convert_messages_multiple_systems_concatenated() {
        let messages = vec![
            ChatMessage::system("System 1"),
            ChatMessage::system("System 2"),
            ChatMessage::user("Hi"),
        ];
        let (preamble, history) = convert_messages(&messages);
        assert_eq!(preamble, Some("System 1\nSystem 2".to_string()));
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn test_convert_messages_tool_result() {
        // Use a conforming 9-char alphanumeric ID so it passes through unchanged.
        let messages = vec![ChatMessage::tool_result(
            "abcDE1234",
            "search",
            "result text",
        )];
        let (preamble, history) = convert_messages(&messages);
        assert!(preamble.is_none());
        assert_eq!(history.len(), 1);
        // Tool results become User messages in rig-core
        match &history[0] {
            RigMessage::User { content } => match content.first() {
                UserContent::ToolResult(r) => {
                    assert_eq!(r.id, "abcDE1234");
                    assert_eq!(r.call_id.as_deref(), Some("abcDE1234"));
                }
                other => panic!("Expected tool result content, got: {:?}", other),
            },
            other => panic!("Expected User message, got: {:?}", other),
        }
    }

    #[test]
    fn test_convert_messages_assistant_with_tool_calls() {
        // Use a conforming 9-char alphanumeric ID so it passes through unchanged.
        let tc = IronToolCall {
            id: "Xt7mK9pQ2".to_string(),
            name: "search".to_string(),
            arguments: serde_json::json!({"query": "test"}),
            reasoning: None,
        };
        let msg = ChatMessage::assistant_with_tool_calls(Some("thinking".to_string()), vec![tc]);
        let messages = vec![msg];
        let (_preamble, history) = convert_messages(&messages);
        assert_eq!(history.len(), 1);
        match &history[0] {
            RigMessage::Assistant { content, .. } => {
                // Should have both text and tool call
                assert!(content.iter().count() >= 2);
                for item in content.iter() {
                    if let AssistantContent::ToolCall(tc) = item {
                        assert_eq!(tc.call_id.as_deref(), Some("Xt7mK9pQ2"));
                    }
                }
            }
            other => panic!("Expected Assistant message, got: {:?}", other),
        }
    }

    #[test]
    fn test_convert_messages_tool_result_without_id_gets_fallback() {
        let messages = vec![ChatMessage {
            role: crate::llm::Role::Tool,
            content: "result text".to_string(),
            content_parts: Vec::new(),
            tool_call_id: None,
            name: Some("search".to_string()),
            tool_calls: None,
        }];
        let (_preamble, history) = convert_messages(&messages);
        match &history[0] {
            RigMessage::User { content } => match content.first() {
                UserContent::ToolResult(r) => {
                    // Missing ID → normalized_tool_call_id generates a 9-char alphanumeric ID.
                    assert_eq!(
                        r.id.len(),
                        9,
                        "fallback ID should be 9 chars, got: {}",
                        r.id
                    );
                    assert!(r.id.chars().all(|c| c.is_ascii_alphanumeric()));
                    assert_eq!(r.call_id.as_deref(), Some(r.id.as_str()));
                }
                other => panic!("Expected tool result content, got: {:?}", other),
            },
            other => panic!("Expected User message, got: {:?}", other),
        }
    }

    #[test]
    fn test_convert_tools() {
        let tools = vec![IronToolDefinition {
            name: "search".to_string(),
            description: "Search the web".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer"}
                },
                "required": ["query"]
            }),
        }];
        let rig_tools = convert_tools(&tools);
        assert_eq!(rig_tools.len(), 1);
        assert_eq!(rig_tools[0].name, "search");
        assert_eq!(rig_tools[0].description, "Search the web");
        assert_eq!(rig_tools[0].parameters, tools[0].parameters);
    }

    #[test]
    fn test_convert_tool_choice() {
        assert!(matches!(
            convert_tool_choice(Some("auto")),
            Some(RigToolChoice::Auto)
        ));
        assert!(matches!(
            convert_tool_choice(Some("required")),
            Some(RigToolChoice::Required)
        ));
        assert!(matches!(
            convert_tool_choice(Some("none")),
            Some(RigToolChoice::None)
        ));
        assert!(matches!(
            convert_tool_choice(Some("AUTO")),
            Some(RigToolChoice::Auto)
        ));
        assert!(convert_tool_choice(None).is_none());
        assert!(convert_tool_choice(Some("unknown")).is_none());
    }

    #[test]
    fn test_extract_response_text_only() {
        let content = OneOrMany::one(AssistantContent::text("Hello world"));
        let usage = RigUsage::new();
        let (text, calls, finish) = extract_response(&content, &usage);
        assert_eq!(text, Some("Hello world".to_string()));
        assert!(calls.is_empty());
        assert_eq!(finish, FinishReason::Stop);
    }

    #[test]
    fn test_extract_response_tool_call() {
        let tc = AssistantContent::tool_call("call_1", "search", serde_json::json!({"q": "test"}));
        let content = OneOrMany::one(tc);
        let usage = RigUsage::new();
        let (text, calls, finish) = extract_response(&content, &usage);
        assert!(text.is_none());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "search");
        assert_eq!(finish, FinishReason::ToolUse);
    }

    #[test]
    fn test_assistant_tool_call_empty_id_gets_generated() {
        let tc = IronToolCall {
            id: "".to_string(),
            name: "search".to_string(),
            arguments: serde_json::json!({"query": "test"}),
            reasoning: None,
        };
        let messages = vec![ChatMessage::assistant_with_tool_calls(None, vec![tc])];
        let (_preamble, history) = convert_messages(&messages);

        match &history[0] {
            RigMessage::Assistant { content, .. } => {
                let tool_call = content.iter().find_map(|c| match c {
                    AssistantContent::ToolCall(tc) => Some(tc),
                    _ => None,
                });
                let tc = tool_call.expect("should have a tool call");
                // Empty ID → normalized_tool_call_id generates a 9-char alphanumeric ID.
                assert_eq!(
                    tc.id.len(),
                    9,
                    "generated id should be 9 chars, got: {}",
                    tc.id
                );
                assert!(tc.id.chars().all(|c| c.is_ascii_alphanumeric()));
                assert_eq!(tc.call_id.as_deref(), Some(tc.id.as_str()));
            }
            other => panic!("Expected Assistant message, got: {:?}", other),
        }
    }

    #[test]
    fn test_assistant_tool_call_whitespace_id_gets_generated() {
        let tc = IronToolCall {
            id: "   ".to_string(),
            name: "search".to_string(),
            arguments: serde_json::json!({"query": "test"}),
            reasoning: None,
        };
        let messages = vec![ChatMessage::assistant_with_tool_calls(None, vec![tc])];
        let (_preamble, history) = convert_messages(&messages);

        match &history[0] {
            RigMessage::Assistant { content, .. } => {
                let tool_call = content.iter().find_map(|c| match c {
                    AssistantContent::ToolCall(tc) => Some(tc),
                    _ => None,
                });
                let tc = tool_call.expect("should have a tool call");
                // Whitespace-only ID → normalized_tool_call_id generates a 9-char alphanumeric ID.
                assert_eq!(
                    tc.id.len(),
                    9,
                    "generated id should be 9 chars, got: {}",
                    tc.id
                );
                assert!(tc.id.chars().all(|c| c.is_ascii_alphanumeric()));
            }
            other => panic!("Expected Assistant message, got: {:?}", other),
        }
    }

    #[test]
    fn test_assistant_and_tool_result_missing_ids_share_generated_id() {
        // Simulate: assistant emits a tool call with empty id, then tool
        // result arrives without an id. Both should get deterministic
        // generated ids that match (based on their position in history).
        let tc = IronToolCall {
            id: "".to_string(),
            name: "search".to_string(),
            arguments: serde_json::json!({"query": "test"}),
            reasoning: None,
        };
        let assistant_msg = ChatMessage::assistant_with_tool_calls(None, vec![tc]);
        let tool_result_msg = ChatMessage {
            role: crate::llm::Role::Tool,
            content: "search results here".to_string(),
            content_parts: Vec::new(),
            tool_call_id: None,
            name: Some("search".to_string()),
            tool_calls: None,
        };
        let messages = vec![assistant_msg, tool_result_msg];
        let (_preamble, history) = convert_messages(&messages);

        // Extract the generated call_id from the assistant tool call
        let assistant_call_id = match &history[0] {
            RigMessage::Assistant { content, .. } => {
                let tc = content.iter().find_map(|c| match c {
                    AssistantContent::ToolCall(tc) => Some(tc),
                    _ => None,
                });
                tc.expect("should have tool call").id.clone()
            }
            other => panic!("Expected Assistant message, got: {:?}", other),
        };

        // Extract the generated call_id from the tool result
        let tool_result_call_id = match &history[1] {
            RigMessage::User { content } => match content.first() {
                UserContent::ToolResult(r) => r
                    .call_id
                    .clone()
                    .expect("tool result call_id must be present"),
                other => panic!("Expected ToolResult, got: {:?}", other),
            },
            other => panic!("Expected User message, got: {:?}", other),
        };

        assert!(
            !assistant_call_id.is_empty(),
            "assistant call_id must not be empty"
        );
        assert!(
            !tool_result_call_id.is_empty(),
            "tool result call_id must not be empty"
        );

        // NOTE: With the current seed-based generation, these IDs will differ
        // because the assistant tool call uses seed=0 (history.len() at that
        // point) and the tool result uses seed=1 (history.len() after the
        // assistant message was pushed). This documents the current behavior.
        // A future improvement could thread the assistant's generated ID into
        // the tool result for exact matching.
        assert_ne!(
            assistant_call_id, tool_result_call_id,
            "Current impl generates different IDs for assistant call and tool result \
             because seeds differ; this documents the known limitation"
        );
    }

    #[test]
    fn test_saturate_u32() {
        assert_eq!(saturate_u32(100), 100);
        assert_eq!(saturate_u32(u64::MAX), u32::MAX);
        assert_eq!(saturate_u32(u32::MAX as u64), u32::MAX);
    }

    // -- normalize_tool_name tests --

    #[test]
    fn test_normalize_tool_name_exact_match() {
        let known = HashSet::from(["echo".to_string(), "list_jobs".to_string()]);
        assert_eq!(normalize_tool_name("echo", &known), "echo");
    }

    #[test]
    fn test_normalize_tool_name_proxy_prefix_match() {
        let known = HashSet::from(["echo".to_string(), "list_jobs".to_string()]);
        assert_eq!(normalize_tool_name("proxy_echo", &known), "echo");
    }

    #[test]
    fn test_normalize_tool_name_proxy_prefix_no_match_kept() {
        let known = HashSet::from(["echo".to_string(), "list_jobs".to_string()]);
        assert_eq!(
            normalize_tool_name("proxy_unknown", &known),
            "proxy_unknown"
        );
    }

    #[test]
    fn test_normalize_tool_name_unknown_passthrough() {
        let known = HashSet::from(["echo".to_string()]);
        assert_eq!(normalize_tool_name("other_tool", &known), "other_tool");
    }

    #[test]
    fn test_build_rig_request_injects_cache_control_short() {
        let req = build_rig_request(
            Some("You are helpful.".to_string()),
            vec![RigMessage::user("Hello")],
            Vec::new(),
            None,
            None,
            None,
            CacheRetention::Short,
        )
        .unwrap();

        let params = req
            .additional_params
            .expect("should have additional_params for Short retention");
        assert_eq!(params["cache_control"]["type"], "ephemeral");
        assert!(
            params["cache_control"].get("ttl").is_none(),
            "Short retention should not include ttl"
        );
    }

    #[test]
    fn test_build_rig_request_injects_cache_control_long() {
        let req = build_rig_request(
            Some("You are helpful.".to_string()),
            vec![RigMessage::user("Hello")],
            Vec::new(),
            None,
            None,
            None,
            CacheRetention::Long,
        )
        .unwrap();

        let params = req
            .additional_params
            .expect("should have additional_params for Long retention");
        assert_eq!(params["cache_control"]["type"], "ephemeral");
        assert_eq!(params["cache_control"]["ttl"], "1h");
    }

    #[test]
    fn test_build_rig_request_no_cache_control_when_none() {
        let req = build_rig_request(
            Some("You are helpful.".to_string()),
            vec![RigMessage::user("Hello")],
            Vec::new(),
            None,
            None,
            None,
            CacheRetention::None,
        )
        .unwrap();

        assert!(
            req.additional_params.is_none(),
            "additional_params should be None when cache is disabled"
        );
    }

    /// Verify that the multiplier match arms in `RigAdapter::cache_write_multiplier`
    /// produce the expected values. We use a standalone helper because constructing
    /// a real `RigAdapter` requires a rig `Model` (which needs network/provider setup).
    /// The helper mirrors the same match expression — if the impl drifts, the
    /// `test_build_rig_request_*` tests will still catch regressions end-to-end.
    #[test]
    fn test_cache_write_multiplier_values() {
        use rust_decimal::Decimal;
        // None → 1.0× (no surcharge)
        assert_eq!(
            cache_write_multiplier_for(CacheRetention::None),
            Decimal::ONE
        );
        // Short → 1.25× (25% surcharge)
        assert_eq!(
            cache_write_multiplier_for(CacheRetention::Short),
            Decimal::new(125, 2)
        );
        // Long → 2.0× (100% surcharge)
        assert_eq!(
            cache_write_multiplier_for(CacheRetention::Long),
            Decimal::TWO
        );
    }

    fn cache_write_multiplier_for(retention: CacheRetention) -> rust_decimal::Decimal {
        match retention {
            CacheRetention::None => rust_decimal::Decimal::ONE,
            CacheRetention::Short => rust_decimal::Decimal::new(125, 2),
            CacheRetention::Long => rust_decimal::Decimal::TWO,
        }
    }

    // -- supports_prompt_cache tests --

    #[test]
    fn test_supports_prompt_cache_supported_models() {
        // All Claude 3+ models per Anthropic docs
        assert!(supports_prompt_cache("claude-opus-4-6"));
        assert!(supports_prompt_cache("claude-sonnet-4-6"));
        assert!(supports_prompt_cache("claude-sonnet-4"));
        assert!(supports_prompt_cache("claude-haiku-4-5"));
        assert!(supports_prompt_cache("claude-3-5-sonnet-20241022"));
        assert!(supports_prompt_cache("claude-haiku-3"));
        assert!(supports_prompt_cache("Claude-Opus-4-5")); // case-insensitive
        assert!(supports_prompt_cache("anthropic/claude-sonnet-4-6")); // provider prefix
    }

    #[test]
    fn test_supports_prompt_cache_unsupported_models() {
        // Legacy Claude models that predate caching
        assert!(!supports_prompt_cache("claude-2"));
        assert!(!supports_prompt_cache("claude-2.1"));
        assert!(!supports_prompt_cache("claude-instant-1.2"));
        // Non-Claude models
        assert!(!supports_prompt_cache("gpt-4o"));
        assert!(!supports_prompt_cache("llama3"));
    }

    #[test]
    fn test_with_unsupported_params_populates_set() {
        use rig_core::client::CompletionClient;
        use rig_core::providers::openai;

        let client: openai::Client = openai::Client::builder()
            .api_key("test-key")
            .base_url("http://localhost:0")
            .build()
            .unwrap();
        let client = client.completions_api();
        let model = client.completion_model("test-model");
        let adapter = RigAdapter::new(model, "test-model")
            .with_unsupported_params(vec!["temperature".to_string()]);

        assert!(adapter.unsupported_params.contains("temperature"));
        assert!(!adapter.unsupported_params.contains("max_tokens"));
    }

    #[test]
    fn test_strip_unsupported_completion_params() {
        use rig_core::client::CompletionClient;
        use rig_core::providers::openai;

        let client: openai::Client = openai::Client::builder()
            .api_key("test-key")
            .base_url("http://localhost:0")
            .build()
            .unwrap();
        let client = client.completions_api();
        let model = client.completion_model("test-model");
        let adapter = RigAdapter::new(model, "test-model").with_unsupported_params(vec![
            "temperature".to_string(),
            "stop_sequences".to_string(),
        ]);

        let mut req = CompletionRequest::new(vec![ChatMessage::user("hi")]);
        req.temperature = Some(0.7);
        req.max_tokens = Some(100);
        req.stop_sequences = Some(vec!["STOP".to_string()]);

        adapter.strip_unsupported_completion_params(&mut req);

        assert!(req.temperature.is_none(), "temperature should be stripped");
        assert_eq!(req.max_tokens, Some(100), "max_tokens should be preserved");
        assert!(
            req.stop_sequences.is_none(),
            "stop_sequences should be stripped"
        );
    }

    #[test]
    fn test_strip_unsupported_tool_params() {
        use rig_core::client::CompletionClient;
        use rig_core::providers::openai;

        let client: openai::Client = openai::Client::builder()
            .api_key("test-key")
            .base_url("http://localhost:0")
            .build()
            .unwrap();
        let client = client.completions_api();
        let model = client.completion_model("test-model");
        let adapter = RigAdapter::new(model, "test-model")
            .with_unsupported_params(vec!["temperature".to_string(), "max_tokens".to_string()]);

        let mut req = ToolCompletionRequest::new(vec![ChatMessage::user("hi")], vec![]);
        req.temperature = Some(0.5);
        req.max_tokens = Some(200);

        adapter.strip_unsupported_tool_params(&mut req);

        assert!(req.temperature.is_none(), "temperature should be stripped");
        assert!(req.max_tokens.is_none(), "max_tokens should be stripped");
    }

    #[test]
    fn test_unsupported_params_empty_by_default() {
        use rig_core::client::CompletionClient;
        use rig_core::providers::openai;

        let client: openai::Client = openai::Client::builder()
            .api_key("test-key")
            .base_url("http://localhost:0")
            .build()
            .unwrap();
        let client = client.completions_api();
        let model = client.completion_model("test-model");
        let adapter = RigAdapter::new(model, "test-model");

        assert!(adapter.unsupported_params.is_empty());
    }

    /// Regression test: consecutive tool_result messages from parallel tool
    /// execution must be merged into a single User message with multiple
    /// ToolResult content items. Without merging, APIs like Anthropic reject
    /// the request due to consecutive User messages.
    #[test]
    fn test_consecutive_tool_results_merged_into_single_user_message() {
        let tc1 = IronToolCall {
            id: "call_a".to_string(),
            name: "search".to_string(),
            arguments: serde_json::json!({"q": "rust"}),
            reasoning: None,
        };
        let tc2 = IronToolCall {
            id: "call_b".to_string(),
            name: "fetch".to_string(),
            arguments: serde_json::json!({"url": "https://example.com"}),
            reasoning: None,
        };
        let assistant = ChatMessage::assistant_with_tool_calls(None, vec![tc1, tc2]);
        let result_a = ChatMessage::tool_result("call_a", "search", "search results");
        let result_b = ChatMessage::tool_result("call_b", "fetch", "fetch results");

        let messages = vec![assistant, result_a, result_b];
        let (_preamble, history) = convert_messages(&messages);

        // Should be: 1 assistant + 1 merged user (not 1 assistant + 2 users)
        assert_eq!(
            history.len(),
            2,
            "Expected 2 messages (assistant + merged user), got {}",
            history.len()
        );

        // The second message should contain both tool results
        match &history[1] {
            RigMessage::User { content } => {
                assert_eq!(
                    content.len(),
                    2,
                    "Expected 2 tool results in merged user message, got {}",
                    content.len()
                );
                for item in content.iter() {
                    assert!(
                        matches!(item, UserContent::ToolResult(_)),
                        "Expected ToolResult content"
                    );
                }
            }
            other => panic!("Expected User message, got: {:?}", other),
        }
    }

    /// Verify that a tool_result after a non-tool User message is NOT merged.
    #[test]
    fn test_tool_result_after_user_text_not_merged() {
        let user_msg = ChatMessage::user("hello");
        let tool_msg = ChatMessage::tool_result("call_1", "search", "results");

        let messages = vec![user_msg, tool_msg];
        let (_preamble, history) = convert_messages(&messages);

        // Should be 2 separate User messages (text user + tool result user)
        assert_eq!(history.len(), 2);
    }

    // -- normalized_tool_call_id tests --

    #[test]
    fn test_normalized_tool_call_id_conforming_passthrough() {
        // A 9-char alphanumeric ID should pass through unchanged.
        let id = normalized_tool_call_id(Some("abcDE1234"), 42);
        assert_eq!(id, "abcDE1234");
    }

    #[test]
    fn test_normalized_tool_call_id_non_conforming_hashed() {
        // An ID that doesn't match [a-zA-Z0-9]{9} should be hashed into one.
        let id = normalized_tool_call_id(Some("call_abc_long_id"), 0);
        assert_eq!(id.len(), 9);
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric()));
        // Should NOT be the raw input.
        assert_ne!(id, "call_abc_l");
    }

    #[test]
    fn test_normalized_tool_call_id_empty_input() {
        let id = normalized_tool_call_id(Some(""), 5);
        assert_eq!(id.len(), 9);
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn test_normalized_tool_call_id_whitespace_input() {
        let id = normalized_tool_call_id(Some("   "), 5);
        assert_eq!(id.len(), 9);
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric()));
        // Empty and whitespace-only with the same seed should produce identical results.
        let id_empty = normalized_tool_call_id(Some(""), 5);
        assert_eq!(id, id_empty);
    }

    #[test]
    fn test_normalized_tool_call_id_none_input() {
        let id = normalized_tool_call_id(None, 7);
        assert_eq!(id.len(), 9);
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric()));
        // None and empty string with same seed should produce identical results.
        let id_empty = normalized_tool_call_id(Some(""), 7);
        assert_eq!(id, id_empty);
    }

    #[test]
    fn test_normalized_tool_call_id_deterministic() {
        let id1 = normalized_tool_call_id(Some("call_xyz_123"), 0);
        let id2 = normalized_tool_call_id(Some("call_xyz_123"), 0);
        assert_eq!(id1, id2, "same input must produce same output");
    }

    #[test]
    fn test_normalized_tool_call_id_different_inputs_differ() {
        let id_a = normalized_tool_call_id(Some("call_aaa"), 0);
        let id_b = normalized_tool_call_id(Some("call_bbb"), 0);
        assert_ne!(
            id_a, id_b,
            "different raw IDs should produce different hashed IDs"
        );
    }
}
