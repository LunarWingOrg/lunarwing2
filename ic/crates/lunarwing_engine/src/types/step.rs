//! Step — the unit of execution within a thread.
//!
//! Each step corresponds to one LLM call plus its subsequent action
//! executions. This replaces the implicit "iteration" counter in the
//! existing `run_agentic_loop`.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
        match extract_code_block(&text) {
            Some(code) => Self::Code {
                code,
                content: Some(text),
            },
            None => Self::Text(text),
        }
    }
}

/// Extract Python code from fenced code blocks in an LLM response.
fn extract_code_block(text: &str) -> Option<String> {
    let mut all_code = Vec::new();

    for marker in ["```repl", "```python", "```py", "```"] {
        let mut search_from = 0;
        while let Some(start) = text[search_from..].find(marker) {
            let abs_start = search_from + start;
            let after_marker = abs_start + marker.len();

            if marker == "```" && text[after_marker..].starts_with(|c: char| c.is_alphabetic()) {
                let lang: String = text[after_marker..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                    .collect();
                if !["repl", "python", "py"].contains(&lang.as_str()) {
                    search_from = after_marker;
                    continue;
                }
            }

            let code_start = text[after_marker..]
                .find('\n')
                .map(|offset| after_marker + offset + 1)
                .unwrap_or(after_marker);

            if let Some(end) = text[code_start..].find("```") {
                let code = text[code_start..code_start + end].trim();
                if !code.is_empty() {
                    all_code.push(code.to_string());
                }
                search_from = code_start + end + 3;
            } else {
                break;
            }
        }

        if !all_code.is_empty() {
            break;
        }
    }

    (!all_code.is_empty()).then(|| all_code.join("\n\n"))
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
    use super::LlmResponse;

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
