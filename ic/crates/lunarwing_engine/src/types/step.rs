//! Step — the unit of execution within a thread.
//!
//! Each step corresponds to one LLM call plus its subsequent action
//! executions. This replaces the implicit "iteration" counter in the
//! existing `run_agentic_loop`.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::repl::extract_repl_code;
use crate::types::thread::ThreadId;

/// Strongly-typed step identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StepId(pub Uuid);

impl StepId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for StepId {
    fn default() -> Self {
        Self::new()
    }
}

/// Status of a step within its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepStatus {
    Pending,
    LlmCalling,
    Executing,
    Completed,
    Failed,
}

/// Which execution tier handles the step's code/actions.
///
/// Monty is the sole CodeAct/RLM executor. WASM and Docker are used for
/// third-party tool isolation and thread sandboxing (Phase 8), not for
/// running LLM-generated Python.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionTier {
    /// Structured tool calls (JSON action calls from LLM).
    Structured,
    /// Embedded Python via Monty (CodeAct/RLM pattern).
    Scripting,
}

/// A single execution step within a thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub id: StepId,
    pub thread_id: ThreadId,
    /// 1-indexed sequence within the thread.
    pub sequence: usize,
    pub status: StepStatus,
    pub tier: ExecutionTier,
    pub llm_response: Option<LlmResponse>,
    pub action_results: Vec<ActionResult>,
    pub tokens_used: TokenUsage,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl Step {
    pub fn new(thread_id: ThreadId, sequence: usize) -> Self {
        Self {
            id: StepId::new(),
            thread_id,
            sequence,
            status: StepStatus::Pending,
            tier: ExecutionTier::Structured,
            llm_response: None,
            action_results: Vec::new(),
            tokens_used: TokenUsage::default(),
            started_at: Utc::now(),
            completed_at: None,
        }
    }
}

// ── LLM response types ─────────────────────────────────────

/// Response from the LLM: text, action calls, or executable code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LlmResponse {
    /// Final text response.
    Text(String),
    /// One or more action calls (with optional reasoning text).
    ActionCalls {
        calls: Vec<ActionCall>,
        content: Option<String>,
    },
    /// Executable Python code (CodeAct). Tool calls happen as function
    /// calls within the code; the runtime suspends at each one and
    /// delegates to the EffectExecutor.
    Code {
        code: String,
        content: Option<String>,
    },
}

impl LlmResponse {
    /// Classify provider text as plain assistant output or executable CodeAct.
    pub fn from_text(text: String) -> Self {
        match extract_repl_code(&text).ok().flatten() {
            Some(code) => Self::Code {
                code,
                content: Some(text),
            },
            None => Self::Text(text),
        }
    }

    /// Build a response from provider-supplied structured action calls,
    /// consuming CodeAct final-answer pseudo-actions.
    ///
    /// Some OpenAI-compatible endpoints surface the model's CodeAct
    /// `FINAL(answer)` syntax as structured tool calls (`FINAL`,
    /// `FINAL()`, `FINAL(answer...</arg_value>`). Those names have no
    /// capability lease, so executing them only records junk
    /// `No lease for action` failures in thread history. Consume
    /// final-answer syntax here instead (mirroring the orchestrator's
    /// `extract_final` handling): an all-FINAL batch becomes a text
    /// response carrying a parseable `FINAL(...)`, while FINAL-shaped junk
    /// mixed with real capability calls is dropped.
    pub fn from_action_calls(calls: Vec<ActionCall>, content: Option<String>) -> Self {
        let (finals, real_calls): (Vec<ActionCall>, Vec<ActionCall>) = calls
            .into_iter()
            .partition(|call| is_final_pseudo_action(&call.action_name));

        if real_calls.is_empty() {
            if finals.is_empty() {
                return Self::from_text(content.unwrap_or_default());
            }
            let answer = finals
                .first()
                .and_then(final_answer_of)
                .filter(|answer| !answer.is_empty())
                .or_else(|| content.clone().filter(|text| !text.is_empty()));
            let mut text = content.unwrap_or_default();
            if let Some(answer) = answer {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&format_final_call(&answer));
            }
            return Self::from_text(text);
        }

        if !finals.is_empty() {
            tracing::warn!(
                dropped = finals.len(),
                "dropped FINAL pseudo-action(s) mixed into structured tool calls"
            );
        }
        Self::ActionCalls {
            calls: real_calls,
            content,
        }
    }
}

/// Detect a CodeAct final-answer pseudo-action surfaced as a tool call name.
fn is_final_pseudo_action(name: &str) -> bool {
    let lower = name.trim().to_lowercase();
    lower == "final"
        || lower.starts_with("final(")
        || lower == "final_var"
        || lower.starts_with("final_var(")
}

/// Best-effort extraction of the intended final answer from a bogus call.
fn final_answer_of(call: &ActionCall) -> Option<String> {
    final_answer_from_parameters(&call.parameters)
        .or_else(|| final_answer_from_name(&call.action_name))
}

/// Read a human-readable answer out of common parameter shapes.
fn final_answer_from_parameters(params: &serde_json::Value) -> Option<String> {
    match params {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Object(map) => {
            for key in [
                "answer",
                "final_answer",
                "text",
                "content",
                "result",
                "response",
                "message",
                "output",
            ] {
                if let Some(value) = map.get(key).and_then(|v| v.as_str())
                    && !value.is_empty()
                {
                    return Some(value.to_string());
                }
            }
            None
        }
        _ => None,
    }
}

/// Recover an answer embedded in a malformed name such as
/// `FINAL("All steps completed")</arg_value>`.
fn final_answer_from_name(name: &str) -> Option<String> {
    let open = name.find('(')?;
    let inner = &name[open + 1..];
    let end = inner.find("</").unwrap_or(inner.len());
    let inner = inner[..end].trim();
    let inner = inner.strip_suffix(')').unwrap_or(inner).trim();
    let unquoted = inner
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| inner.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(inner)
        .trim();
    (!unquoted.is_empty()).then(|| unquoted.to_string())
}

/// Render an answer as a `FINAL(...)` call the orchestrator can parse.
fn format_final_call(answer: &str) -> String {
    if answer.contains("\"\"\"") {
        format!("FINAL({answer})")
    } else {
        format!("FINAL(\"\"\"{answer}\"\"\")")
    }
}

/// A request from the LLM to execute a capability action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionCall {
    /// Unique call identifier (echoed in the result).
    pub id: String,
    /// Action name (e.g. "web_fetch", "create_issue").
    pub action_name: String,
    /// Action parameters as JSON.
    pub parameters: serde_json::Value,
}

/// Result of executing a capability action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResult {
    /// The call ID this result corresponds to.
    pub call_id: String,
    /// The action that was executed.
    pub action_name: String,
    /// Output value.
    pub output: serde_json::Value,
    /// Whether this result represents an error.
    pub is_error: bool,
    /// How long the action took.
    #[serde(with = "duration_millis")]
    pub duration: Duration,
    /// Images attached by multimodal tools. Runtime-only: never serialized
    /// into sealed step records or checkpoints; the executor stashes them on
    /// the thread for provider delivery instead.
    #[serde(skip)]
    pub transient_images: Vec<crate::types::message::TransientContentPart>,
}

/// Token usage for a single LLM call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// USD cost for this call (populated by LlmBackend if cost data is available).
    pub cost_usd: f64,
}

impl TokenUsage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

/// Serde helper for Duration as milliseconds.
mod duration_millis {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_millis() as u64)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let millis = u64::deserialize(d)?;
        Ok(Duration::from_millis(millis))
    }
}

#[cfg(test)]
mod tests {
    use super::{ActionCall, LlmResponse};

    fn call(name: &str, parameters: serde_json::Value) -> ActionCall {
        ActionCall {
            id: "call-1".to_string(),
            action_name: name.to_string(),
            parameters,
        }
    }

    fn extract_final_answer(text: &str) -> Option<String> {
        // Mirror extract_final() in orchestrator/default.py: FINAL("""...""").
        let idx = text.find("FINAL(")?;
        let after = &text[idx + 6..];
        let after = after.strip_prefix("\"\"\"")?;
        let end = after.find("\"\"\"")?;
        Some(after[..end].to_string())
    }

    #[test]
    fn from_action_calls_passes_real_calls_through() {
        let response = LlmResponse::from_action_calls(
            vec![call("memory_write", serde_json::json!({"path": "a.md"}))],
            Some("reasoning".to_string()),
        );
        let LlmResponse::ActionCalls { calls, content } = response else {
            panic!("real calls must stay structured");
        };
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].action_name, "memory_write");
        assert_eq!(content.as_deref(), Some("reasoning"));
    }

    #[test]
    fn from_action_calls_consumes_bare_final_with_answer_params() {
        let response = LlmResponse::from_action_calls(
            vec![call(
                "FINAL",
                serde_json::json!({"answer": "done: 24 bytes"}),
            )],
            None,
        );
        let LlmResponse::Text(text) = response else {
            panic!("FINAL-only batch must become text");
        };
        assert_eq!(
            extract_final_answer(&text).as_deref(),
            Some("done: 24 bytes")
        );
    }

    #[test]
    fn from_action_calls_consumes_final_junk_with_xml_suffix() {
        // Observed live: name `FINAL()</arg_value>` carrying the answer only in
        // the provider's argument payload.
        let response = LlmResponse::from_action_calls(
            vec![call(
                "FINAL()</arg_value>",
                serde_json::json!({"answer": "All four steps completed"}),
            )],
            Some("report".to_string()),
        );
        let LlmResponse::Text(text) = response else {
            panic!("FINAL junk call must become text");
        };
        assert!(text.starts_with("report\n"));
        assert_eq!(
            extract_final_answer(&text).as_deref(),
            Some("All four steps completed")
        );
    }

    #[test]
    fn from_action_calls_recovers_answer_embedded_in_malformed_name() {
        // Observed live: `FINAL(answer...</arg_value>` with the answer inside
        // the fabricated tool name itself.
        let response = LlmResponse::from_action_calls(
            vec![call(
                "FINAL(All four steps completed with actual tool results)</arg_value>",
                serde_json::json!({}),
            )],
            None,
        );
        let LlmResponse::Text(text) = response else {
            panic!("FINAL-only batch must become text");
        };
        assert_eq!(
            extract_final_answer(&text).as_deref(),
            Some("All four steps completed with actual tool results")
        );
    }

    #[test]
    fn from_action_calls_consumes_final_var() {
        let response = LlmResponse::from_action_calls(
            vec![call(
                "FINAL_VAR",
                serde_json::json!({"result": "summary text"}),
            )],
            None,
        );
        assert!(matches!(response, LlmResponse::Text(_)));
    }

    #[test]
    fn from_action_calls_drops_final_mixed_with_real_calls() {
        let response = LlmResponse::from_action_calls(
            vec![
                call("memory_write", serde_json::json!({"path": "a.md"})),
                call("FINAL", serde_json::json!({"answer": "junk"})),
            ],
            None,
        );
        let LlmResponse::ActionCalls { calls, .. } = response else {
            panic!("mixed batch must keep the real call structured");
        };
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].action_name, "memory_write");
    }

    #[test]
    fn from_action_calls_falls_back_to_content() {
        let response = LlmResponse::from_action_calls(
            vec![call("finAL(", serde_json::json!({}))],
            Some("the claimed answer".to_string()),
        );
        let LlmResponse::Text(text) = response else {
            panic!("FINAL-only batch must become text");
        };
        assert_eq!(
            extract_final_answer(&text).as_deref(),
            Some("the claimed answer")
        );
    }

    #[test]
    fn from_action_calls_empty_final_does_not_invent_success() {
        let response =
            LlmResponse::from_action_calls(vec![call("FINAL", serde_json::json!({}))], None);
        assert!(matches!(response, LlmResponse::Text(text) if text.is_empty()));
    }

    #[test]
    fn from_action_calls_empty_batch_becomes_text() {
        let response = LlmResponse::from_action_calls(vec![], Some("plain".to_string()));
        assert!(matches!(response, LlmResponse::Text(text) if text == "plain"));
    }

    #[test]
    fn from_action_calls_final_answer_survives_triple_quotes() {
        let response = LlmResponse::from_action_calls(
            vec![call(
                "FINAL",
                serde_json::json!({"answer": "quoted \"\"\" inside"}),
            )],
            None,
        );
        // Unquoted fallback keeps FINAL(...) shape for balanced-paren parsing.
        let LlmResponse::Text(text) = response else {
            panic!("FINAL-only batch must become text");
        };
        assert!(text.ends_with(")"));
        assert!(text.contains("quoted \"\"\" inside"));
    }

    fn extracted_code(text: &str) -> Option<String> {
        match LlmResponse::from_text(text.to_string()) {
            LlmResponse::Code { code, content } => {
                assert_eq!(content.as_deref(), Some(text));
                Some(code)
            }
            LlmResponse::Text(_) => None,
            LlmResponse::ActionCalls { .. } => {
                panic!("text classification cannot create actions")
            }
        }
    }

    #[test]
    fn from_text_preserves_plain_text() {
        let response = LlmResponse::from_text("plain response".to_string());
        assert!(matches!(
            response,
            LlmResponse::Text(text) if text == "plain response"
        ));
    }

    #[test]
    fn from_text_extracts_supported_python_fences() {
        let cases = [
            ("```repl\nx = 1\n```", "x = 1"),
            ("```python\nprint('hello')\n```", "print('hello')"),
            ("```py\nvalue = 42\n```", "value = 42"),
            ("```\nFINAL('done')\n```", "FINAL('done')"),
        ];

        for (text, expected) in cases {
            assert_eq!(extracted_code(text).as_deref(), Some(expected));
        }
    }

    #[test]
    fn from_text_ignores_invalid_fences() {
        for text in [
            "```json\n{\"key\": \"value\"}\n```",
            "```python\n\n```",
            "```python\nprint('unclosed')",
        ] {
            assert!(matches!(
                LlmResponse::from_text(text.to_string()),
                LlmResponse::Text(content) if content == text
            ));
        }
    }

    #[test]
    fn from_text_concatenates_multiple_specific_blocks() {
        let text = "```repl\nfirst = 1\n```\ntext\n```repl\nFINAL(first)\n```";
        assert_eq!(
            extracted_code(text).as_deref(),
            Some("first = 1\n\nFINAL(first)")
        );
    }

    #[test]
    fn from_text_prefers_specific_marker_over_bare_block() {
        let text = "```\nignored\n```\n```repl\nused = True\n```";
        assert_eq!(extracted_code(text).as_deref(), Some("used = True"));
    }
}
