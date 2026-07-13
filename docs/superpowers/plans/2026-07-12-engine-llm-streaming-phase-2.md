# Engine LLM Streaming Phase 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the real Engine V2 orchestrator consume LunarWing's native plain and tool-capable LLM streams, reconstruct existing engine outputs strictly, and emit transient provider-neutral response deltas without changing channel behavior.

**Architecture:** `LlmBridgeAdapter` maps host requests and chunks without buffering. A focused engine collector validates and reconstructs `LlmOutput`, while the primary orchestrator supplies a callback that broadcasts transient `ResponseDelta` events. All channel, SSE, frontend, interrupt, and non-primary engine LLM paths remain unchanged.

**Tech Stack:** Rust 2024, MSRV 1.92, `async-trait`, `futures 0.3` boxed streams, Tokio broadcast tests, Serde event round trips, Monty-backed Engine V2 integration tests.

---

## Scope And Constraints

- Implement the approved design in
  `docs/superpowers/specs/2026-07-12-engine-llm-streaming-phase-2-design.md`.
- Work from `ic/` for every Cargo command.
- Prefix every Cargo command with `taskset -c 0-5`; use `-j6` where the Cargo
  subcommand accepts it.
- Never run `cargo build`. Use `cargo check` for compile verification.
- Run commands expected to exceed five minutes in tmux and write their output
  to `/tmp`.
- Keep Engine V2 gateway-only. Do not edit channel, web, frontend, WIT,
  configuration, or routing-policy code.
- Keep compaction, scripting, mission, and other auxiliary engine LLM calls on
  `LlmBackend::complete()`.
- Do not persist `ResponseDelta` in `thread.events`.
- Do not update `ic/FEATURE_PARITY.md`; Phase 2 remains non-user-visible.

## File Map

- Modify `ic/crates/lunarwing_engine/src/types/step.rs`: centralize text versus
  CodeAct response classification in `LlmResponse::from_text`.
- Modify `ic/crates/lunarwing_engine/src/types/event.rs`: add and serialize the
  transient `ResponseDelta` event kind.
- Create `ic/crates/lunarwing_engine/src/executor/llm_stream.rs`: strict engine
  stream accumulation and reconstruction.
- Modify `ic/crates/lunarwing_engine/src/executor/mod.rs`: register the focused
  collector module without exporting it outside the executor.
- Modify `ic/src/bridge/llm_adapter.rs`: share request construction between
  blocking/streaming methods and map native host chunks to engine chunks.
- Modify `ic/src/llm/mod.rs`: expose Phase 1 scripted stream fixtures to sibling
  modules under `#[cfg(test)]` only.
- Modify `ic/crates/lunarwing_engine/src/executor/orchestrator.rs`: consume the
  engine stream and broadcast deltas in the primary LLM host call.
- Modify `ic/crates/lunarwing_engine/src/executor/loop_engine.rs`: exercise the
  real orchestrator with native text, code, tool, error, and no-receiver streams.
- Modify `docs/proposals/ENGINE_LLM_STREAMING.md`: record verified Phase 2 status
  and make Phase 3 the next delivery milestone.

### Task 1: Centralize Engine Text And Code Response Classification

**Files:**
- Modify: `ic/crates/lunarwing_engine/src/types/step.rs:86-103,160-end`
- Modify: `ic/src/bridge/llm_adapter.rs:77-84,128-137,204-361`
- Test: `ic/crates/lunarwing_engine/src/types/step.rs`
- Test: `ic/src/bridge/llm_adapter.rs`

- [ ] **Step 1: Add failing `LlmResponse::from_text` classification tests**

Append a test module to `types/step.rs`. Use table-driven cases so the current
bridge coverage is preserved without duplicating the parser:

```rust
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
            LlmResponse::ActionCalls { .. } => panic!("text classification cannot create actions"),
        }
    }

    #[test]
    fn from_text_preserves_plain_text() {
        let response = LlmResponse::from_text("plain response".to_string());
        assert!(matches!(response, LlmResponse::Text(text) if text == "plain response"));
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
```

- [ ] **Step 2: Run the classification tests to verify RED**

Run:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine types::step::tests:: -- --nocapture
```

Expected: compilation fails with `E0599` because `LlmResponse::from_text` does
not exist.

- [ ] **Step 3: Add the shared constructor and move the existing parser**

Add this constructor beside `LlmResponse`, then move the current
`extract_code_block` body from `src/bridge/llm_adapter.rs` into `types/step.rs`
as the private helper shown below:

```rust
impl LlmResponse {
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
```

- [ ] **Step 4: Make blocking bridge conversion use the shared constructor**

Replace both text/code matches in `LlmBridgeAdapter::complete()` with:

```rust
let llm_response = LlmResponse::from_text(response.content);
```

and:

```rust
let llm_response = if !response.tool_calls.is_empty() {
    LlmResponse::ActionCalls {
        calls: response
            .tool_calls
            .iter()
            .map(|call| lunarwing_engine::ActionCall {
                id: call.id.clone(),
                action_name: call.name.clone(),
                parameters: call.arguments.clone(),
            })
            .collect(),
        content: response.content.clone(),
    }
} else {
    LlmResponse::from_text(response.content.unwrap_or_default())
};
```

Delete the bridge-local `extract_code_block` function and its now-relocated
unit tests.

- [ ] **Step 5: Run focused classification and bridge tests**

Run:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine types::step::tests:: -- --nocapture
taskset -c 0-5 cargo test -j6 --lib bridge::llm_adapter::tests:: -- --nocapture
```

Expected: all classification tests pass; the bridge test target compiles and
passes with no local parser remaining.

- [ ] **Step 6: Commit the behavior-preserving classification refactor**

```bash
git add ic/crates/lunarwing_engine/src/types/step.rs ic/src/bridge/llm_adapter.rs
git commit -m "refactor: centralize engine llm response classification"
```

### Task 2: Add The Provider-Neutral Response Delta Event

**Files:**
- Modify: `ic/crates/lunarwing_engine/src/types/event.rs:114-213`
- Test: `ic/crates/lunarwing_engine/src/types/event.rs`

- [ ] **Step 1: Add the failing Serde round-trip test**

Append this test module to `types/event.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::{EventKind, ThreadEvent};
    use crate::types::thread::ThreadId;

    #[test]
    fn response_delta_round_trips_through_serde() {
        let event = ThreadEvent::new(
            ThreadId::new(),
            EventKind::ResponseDelta {
                content: "partial response".to_string(),
            },
        );

        let json = serde_json::to_value(&event).expect("response delta should serialize");
        let decoded: ThreadEvent =
            serde_json::from_value(json).expect("response delta should deserialize");

        assert_eq!(decoded.thread_id, event.thread_id);
        assert!(matches!(
            decoded.kind,
            EventKind::ResponseDelta { content } if content == "partial response"
        ));
    }
}
```

- [ ] **Step 2: Run the event test to verify RED**

Run:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  types::event::tests::response_delta_round_trips_through_serde \
  -- --exact --nocapture
```

Expected: compilation fails because `EventKind::ResponseDelta` is absent.

- [ ] **Step 3: Add the transient engine event variant**

Add the variant in the messages section immediately after `MessageAdded`:

```rust
    /// A live, provider-neutral assistant text fragment.
    ///
    /// This event is broadcast for delivery but is not persisted in the
    /// thread's event history.
    ResponseDelta {
        content: String,
    },
```

- [ ] **Step 4: Run the event test to verify GREEN**

Run:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  types::event::tests::response_delta_round_trips_through_serde \
  -- --exact --nocapture
```

Expected: one test passes.

- [ ] **Step 5: Commit the event contract**

```bash
git add ic/crates/lunarwing_engine/src/types/event.rs
git commit -m "feat: add engine response delta events"
```

### Task 3: Build The Strict Engine Stream Collector

**Files:**
- Create: `ic/crates/lunarwing_engine/src/executor/llm_stream.rs`
- Modify: `ic/crates/lunarwing_engine/src/executor/mod.rs:1-18`
- Test: `ic/crates/lunarwing_engine/src/executor/llm_stream.rs`

- [ ] **Step 1: Register the collector module and add failing text/terminal tests**

Add this private module declaration to `executor/mod.rs`:

```rust
mod llm_stream;
```

Create `executor/llm_stream.rs` with imports plus a test module. The first RED
set must cover ordered callbacks, empty callback suppression, code
classification, terminal usage, missing usage, missing `Done`, post-`Done`
chunks, duplicate `Done`, and a mid-stream error:

```rust
use std::collections::BTreeMap;

use futures::StreamExt;

use crate::traits::llm::{LlmOutput, LlmStream, LlmStreamChunk};
use crate::types::error::EngineError;
use crate::types::step::{ActionCall, LlmResponse, TokenUsage};

#[cfg(test)]
mod tests {
    use futures::stream;

    use super::*;

    fn done(usage: Option<TokenUsage>) -> Result<LlmStreamChunk, EngineError> {
        Ok(LlmStreamChunk::Done {
            usage,
            finish_reason: "stop".to_string(),
        })
    }

    async fn collect(
        items: Vec<Result<LlmStreamChunk, EngineError>>,
    ) -> (Result<LlmOutput, EngineError>, Vec<String>) {
        let stream = stream::iter(items).boxed();
        let mut deltas = Vec::new();
        let result = collect_llm_stream(stream, |delta| deltas.push(delta.to_string())).await;
        (result, deltas)
    }

    #[tokio::test]
    async fn collects_text_deltas_and_terminal_usage() {
        let usage = TokenUsage {
            input_tokens: 8,
            output_tokens: 3,
            ..TokenUsage::default()
        };
        let (result, deltas) = collect(vec![
            Ok(LlmStreamChunk::TextDelta("hel".to_string())),
            Ok(LlmStreamChunk::TextDelta(String::new())),
            Ok(LlmStreamChunk::TextDelta("lo".to_string())),
            done(Some(usage)),
        ])
        .await;

        let output = result.expect("valid text stream should collect");
        assert_eq!(deltas, vec!["hel", "lo"]);
        assert_eq!(output.usage, usage);
        assert!(matches!(output.response, LlmResponse::Text(text) if text == "hello"));
    }

    #[tokio::test]
    async fn classifies_collected_code() {
        let (result, _) = collect(vec![
            Ok(LlmStreamChunk::TextDelta("```repl\nFINAL('done')\n```".to_string())),
            done(None),
        ])
        .await;

        assert!(matches!(
            result.expect("code stream should collect").response,
            LlmResponse::Code { code, .. } if code == "FINAL('done')"
        ));
    }

    #[tokio::test]
    async fn missing_usage_defaults_to_zero() {
        let (result, _) = collect(vec![done(None)]).await;
        assert_eq!(
            result.expect("usage-less terminal should collect").usage,
            TokenUsage::default()
        );
    }

    #[tokio::test]
    async fn rejects_eof_before_done() {
        let (result, _) = collect(vec![Ok(LlmStreamChunk::TextDelta("partial".into()))]).await;
        assert!(matches!(result, Err(EngineError::Llm { reason }) if reason.contains("before Done")));
    }

    #[tokio::test]
    async fn rejects_chunk_after_done() {
        let (result, _) = collect(vec![
            done(None),
            Ok(LlmStreamChunk::TextDelta("late".into())),
        ])
        .await;
        assert!(matches!(result, Err(EngineError::Llm { reason }) if reason.contains("after Done")));
    }

    #[tokio::test]
    async fn rejects_tool_chunk_after_done() {
        let (result, _) = collect(vec![
            done(None),
            Ok(LlmStreamChunk::ToolCallDelta {
                index: 0,
                id: Some("call-1".to_string()),
                name: Some("search".to_string()),
                args_delta: "{}".to_string(),
            }),
        ])
        .await;
        assert!(matches!(result, Err(EngineError::Llm { reason }) if reason.contains("after Done")));
    }

    #[tokio::test]
    async fn rejects_second_done() {
        let (result, _) = collect(vec![done(None), done(None)]).await;
        assert!(matches!(result, Err(EngineError::Llm { reason }) if reason.contains("after Done")));
    }

    #[tokio::test]
    async fn propagates_mid_stream_error_after_callback() {
        let (result, deltas) = collect(vec![
            Ok(LlmStreamChunk::TextDelta("visible".into())),
            Err(EngineError::Llm {
                reason: "stream failed".into(),
            }),
        ])
        .await;
        assert_eq!(deltas, vec!["visible"]);
        assert!(matches!(result, Err(EngineError::Llm { reason }) if reason == "stream failed"));
    }
}
```

- [ ] **Step 2: Run the collector tests to verify RED**

Run:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine executor::llm_stream::tests:: -- --nocapture
```

Expected: compilation fails because `collect_llm_stream` is absent.

- [ ] **Step 3: Add failing fragmented-tool and identity-validation tests**

Extend the same test module with these helpers and cases before writing the
collector implementation:

```rust
    fn tool(
        index: usize,
        id: Option<&str>,
        name: Option<&str>,
        args_delta: &str,
    ) -> Result<LlmStreamChunk, EngineError> {
        Ok(LlmStreamChunk::ToolCallDelta {
            index,
            id: id.map(str::to_string),
            name: name.map(str::to_string),
            args_delta: args_delta.to_string(),
        })
    }

    #[tokio::test]
    async fn reconstructs_fragmented_interleaved_tool_calls_in_index_order() {
        let (result, deltas) = collect(vec![
            Ok(LlmStreamChunk::TextDelta("I will run tools.".into())),
            tool(1, Some("call-2"), Some("second"), "{\"b\":"),
            tool(0, Some("call-1"), Some("first"), "{\"a\":"),
            tool(1, None, None, "2}"),
            tool(0, Some("call-1"), Some("first"), "1}"),
            done(Some(TokenUsage::default())),
        ])
        .await;

        let output = result.expect("fragmented tool stream should collect");
        assert_eq!(deltas, vec!["I will run tools."]);
        let LlmResponse::ActionCalls { calls, content } = output.response else {
            panic!("expected action calls");
        };
        assert_eq!(content.as_deref(), Some("I will run tools."));
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call-1");
        assert_eq!(calls[0].action_name, "first");
        assert_eq!(calls[0].parameters, serde_json::json!({"a": 1}));
        assert_eq!(calls[1].id, "call-2");
        assert_eq!(calls[1].parameters, serde_json::json!({"b": 2}));
    }

    #[tokio::test]
    async fn tool_stream_without_text_has_no_content() {
        let (result, deltas) = collect(vec![
            tool(0, Some("call-1"), Some("search"), "{}"),
            done(None),
        ])
        .await;

        assert!(deltas.is_empty());
        assert!(matches!(
            result.expect("tool stream should collect").response,
            LlmResponse::ActionCalls { content: None, .. }
        ));
    }

    #[tokio::test]
    async fn rejects_conflicting_tool_identity() {
        for items in [
            vec![
                tool(0, Some("call-1"), Some("search"), "{}"),
                tool(0, Some("call-2"), None, ""),
                done(None),
            ],
            vec![
                tool(0, Some("call-1"), Some("search"), "{}"),
                tool(0, None, Some("fetch"), ""),
                done(None),
            ],
        ] {
            let (result, _) = collect(items).await;
            assert!(matches!(result, Err(EngineError::Llm { reason }) if reason.contains("conflicting")));
        }
    }

    #[tokio::test]
    async fn rejects_incomplete_or_malformed_tool_calls() {
        let cases = [
            vec![tool(0, None, Some("search"), "{}"), done(None)],
            vec![tool(0, Some("call-1"), None, "{}"), done(None)],
            vec![
                tool(0, Some("call-1"), Some("search"), "not-json"),
                done(None),
            ],
        ];

        for items in cases {
            let (result, _) = collect(items).await;
            assert!(matches!(result, Err(EngineError::Llm { .. })));
        }
    }
```

Expected: the tests remain RED because the collector is still absent.

- [ ] **Step 4: Implement the strict accumulator and collector**

Add the production code above the test module:

```rust
#[derive(Default)]
struct StreamAccumulator {
    text: String,
    tool_calls: BTreeMap<usize, ToolCallAccumulator>,
    usage: TokenUsage,
    saw_done: bool,
}

#[derive(Default)]
struct ToolCallAccumulator {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

pub(super) async fn collect_llm_stream(
    mut stream: LlmStream<'_>,
    mut on_text_delta: impl FnMut(&str),
) -> Result<LlmOutput, EngineError> {
    let mut accumulator = StreamAccumulator::default();
    while let Some(item) = stream.next().await {
        accumulator.push(item?, &mut on_text_delta)?;
    }
    accumulator.finish()
}

impl StreamAccumulator {
    fn push(
        &mut self,
        chunk: LlmStreamChunk,
        on_text_delta: &mut impl FnMut(&str),
    ) -> Result<(), EngineError> {
        if self.saw_done {
            return Err(llm_error("LLM stream emitted a chunk after Done"));
        }

        match chunk {
            LlmStreamChunk::TextDelta(delta) => {
                if !delta.is_empty() {
                    on_text_delta(&delta);
                }
                self.text.push_str(&delta);
            }
            LlmStreamChunk::ToolCallDelta {
                index,
                id,
                name,
                args_delta,
            } => {
                let call = self.tool_calls.entry(index).or_default();
                merge_identity(&mut call.id, id, "id", index)?;
                merge_identity(&mut call.name, name, "name", index)?;
                call.arguments.push_str(&args_delta);
            }
            LlmStreamChunk::Done {
                usage,
                finish_reason: _,
            } => {
                self.usage = usage.unwrap_or_default();
                self.saw_done = true;
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<LlmOutput, EngineError> {
        if !self.saw_done {
            return Err(llm_error("LLM stream ended before Done"));
        }

        let response = if self.tool_calls.is_empty() {
            LlmResponse::from_text(self.text)
        } else {
            let mut calls = Vec::with_capacity(self.tool_calls.len());
            for (index, call) in self.tool_calls {
                calls.push(call.finish(index)?);
            }
            LlmResponse::ActionCalls {
                calls,
                content: (!self.text.is_empty()).then_some(self.text),
            }
        };

        Ok(LlmOutput {
            response,
            usage: self.usage,
        })
    }
}

impl ToolCallAccumulator {
    fn finish(self, index: usize) -> Result<ActionCall, EngineError> {
        let id = self
            .id
            .ok_or_else(|| llm_error(format!("tool call at index {index} is missing an id")))?;
        let action_name = self
            .name
            .ok_or_else(|| llm_error(format!("tool call at index {index} is missing a name")))?;
        let parameters = serde_json::from_str(&self.arguments).map_err(|error| {
            llm_error(format!(
                "tool call at index {index} has invalid JSON arguments: {error}"
            ))
        })?;
        Ok(ActionCall {
            id,
            action_name,
            parameters,
        })
    }
}

fn merge_identity(
    current: &mut Option<String>,
    incoming: Option<String>,
    field: &str,
    index: usize,
) -> Result<(), EngineError> {
    let Some(value) = incoming.filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    match current {
        None => {
            *current = Some(value);
            Ok(())
        }
        Some(existing) if existing == &value => Ok(()),
        Some(existing) => Err(llm_error(format!(
            "conflicting tool call {field} at index {index}: '{existing}' versus '{value}'"
        ))),
    }
}

fn llm_error(reason: impl Into<String>) -> EngineError {
    EngineError::Llm {
        reason: reason.into(),
    }
}
```

- [ ] **Step 5: Run all collector tests to verify GREEN**

Run:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine executor::llm_stream::tests:: -- --nocapture
```

Expected: every collector test passes, including strict terminal and tool
validation cases.

- [ ] **Step 6: Verify the Phase 0 blocking fallback through the collector**

Add this complete-only backend and collector test to the test module:

```rust
    use crate::traits::llm::{LlmBackend, LlmCallConfig};

    struct CompleteOnlyBackend;

    #[async_trait::async_trait]
    impl LlmBackend for CompleteOnlyBackend {
        async fn complete(
            &self,
            _messages: &[crate::types::message::ThreadMessage],
            _actions: &[crate::types::capability::ActionDef],
            _config: &LlmCallConfig,
        ) -> Result<LlmOutput, EngineError> {
            Ok(LlmOutput {
                response: LlmResponse::Text("fallback text".to_string()),
                usage: TokenUsage {
                    input_tokens: 5,
                    output_tokens: 2,
                    ..TokenUsage::default()
                },
            })
        }

        fn model_name(&self) -> &str {
            "complete-only"
        }
    }

    #[tokio::test]
    async fn collects_complete_only_backend_fallback() {
        let backend = CompleteOnlyBackend;
        let stream = backend
            .complete_stream(&[], &[], &LlmCallConfig::default())
            .await
            .expect("fallback stream should open");
        let mut deltas = Vec::new();
        let output = collect_llm_stream(stream, |delta| deltas.push(delta.to_string()))
            .await
            .expect("fallback stream should collect");

        assert_eq!(deltas, vec!["fallback text"]);
        assert_eq!(output.usage.input_tokens, 5);
        assert_eq!(output.usage.output_tokens, 2);
        assert!(matches!(
            output.response,
            LlmResponse::Text(text) if text == "fallback text"
        ));
    }
```

Run:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  executor::llm_stream::tests::collects_complete_only_backend_fallback \
  -- --exact --nocapture
```

Expected: one test passes.

- [ ] **Step 7: Commit the collector**

```bash
git add ic/crates/lunarwing_engine/src/executor/mod.rs \
  ic/crates/lunarwing_engine/src/executor/llm_stream.rs
git commit -m "feat: collect engine llm streams"
```

### Task 4: Map Native Host Streams Through `LlmBridgeAdapter`

**Files:**
- Modify: `ic/src/llm/mod.rs:20-28`
- Modify: `ic/src/bridge/llm_adapter.rs:3-155,263-end`
- Test: `ic/src/bridge/llm_adapter.rs`

- [ ] **Step 1: Make the scripted Phase 1 provider fixture crate-visible in tests**

Change only the test module declaration in `src/llm/mod.rs`:

```rust
#[cfg(test)]
pub(crate) mod streaming_test_support;
```

- [ ] **Step 2: Add failing plain, tool, depth, and error mapping tests**

In `llm_adapter.rs` tests, import `futures::StreamExt`, the scripted provider,
and host/engine chunk aliases. Add helpers for one action and terminal usage,
then add these tests:

```rust
use futures::StreamExt;
use rust_decimal::Decimal;

use crate::llm::streaming_test_support::{ScriptedStreamingProvider, StreamScript};
use crate::llm::{LlmError, LlmStreamChunk as HostChunk, TokenUsage as HostUsage};
use lunarwing_engine::LlmStreamChunk as EngineChunk;

fn search_action() -> ActionDef {
    ActionDef {
        name: "search".to_string(),
        description: "Search".to_string(),
        parameters_schema: serde_json::json!({"type": "object"}),
        effects: Vec::new(),
        requires_approval: false,
    }
}

#[tokio::test]
async fn complete_stream_maps_plain_host_chunks_and_usage() {
    let provider = Arc::new(ScriptedStreamingProvider::new(
        "primary",
        vec![StreamScript::Items(vec![
            Ok(HostChunk::TextDelta("hel".into())),
            Ok(HostChunk::TextDelta("lo".into())),
            Ok(HostChunk::Done {
                usage: Some(HostUsage {
                    input_tokens: 8,
                    output_tokens: 3,
                    cache_read_input_tokens: 2,
                    cache_creation_input_tokens: 1,
                }),
                finish_reason: "stop".into(),
            }),
        ])],
        vec![],
    ));
    let adapter = LlmBridgeAdapter::new(provider.clone(), None);

    let chunks = adapter
        .complete_stream(&[], &[], &LlmCallConfig::default())
        .await
        .expect("plain stream should open")
        .collect::<Vec<_>>()
        .await;

    assert_eq!(provider.plain_calls(), 1);
    assert!(matches!(&chunks[0], Ok(EngineChunk::TextDelta(text)) if text == "hel"));
    assert!(matches!(&chunks[1], Ok(EngineChunk::TextDelta(text)) if text == "lo"));
    match &chunks[2] {
        Ok(EngineChunk::Done {
            usage: Some(usage),
            finish_reason,
        }) => {
            assert_eq!(usage.input_tokens, 8);
            assert_eq!(usage.output_tokens, 3);
            assert_eq!(usage.cache_read_tokens, 2);
            assert_eq!(usage.cache_write_tokens, 1);
            assert_eq!(usage.cost_usd, 0.0);
            assert_eq!(finish_reason, "stop");
        }
        other => panic!("expected mapped terminal usage, got {other:?}"),
    }
}

#[tokio::test]
async fn complete_stream_maps_fragmented_tool_chunks() {
    let provider = Arc::new(ScriptedStreamingProvider::new(
        "primary",
        vec![],
        vec![StreamScript::Items(vec![
            Ok(HostChunk::ToolCallDelta {
                index: 0,
                id: Some("call-1".into()),
                name: Some("search".into()),
                args_delta: "{\"q\":\"".into(),
            }),
            Ok(HostChunk::ToolCallDelta {
                index: 0,
                id: None,
                name: None,
                args_delta: "rust\"}".into(),
            }),
            Ok(HostChunk::Done {
                usage: None,
                finish_reason: "tool_calls".into(),
            }),
        ])],
    ));
    let adapter = LlmBridgeAdapter::new(provider.clone(), None);

    let chunks = adapter
        .complete_stream(&[], &[search_action()], &LlmCallConfig::default())
        .await
        .expect("tool stream should open")
        .collect::<Vec<_>>()
        .await;

    assert_eq!(provider.tool_calls(), 1);
    assert!(matches!(
        &chunks[0],
        Ok(EngineChunk::ToolCallDelta {
            index: 0,
            id: Some(id),
            name: Some(name),
            args_delta,
        }) if id == "call-1" && name == "search" && args_delta == "{\"q\":\""
    ));
    assert!(matches!(
        &chunks[1],
        Ok(EngineChunk::ToolCallDelta { args_delta, .. }) if args_delta == "rust\"}"
    ));
}

#[tokio::test]
async fn complete_stream_uses_cheap_provider_at_nonzero_depth() {
    let primary = Arc::new(ScriptedStreamingProvider::new("primary", vec![], vec![]));
    let cheap = Arc::new(ScriptedStreamingProvider::new(
        "cheap",
        vec![StreamScript::Items(vec![
            Ok(HostChunk::TextDelta("cheap".into())),
            Ok(HostChunk::Done {
                usage: None,
                finish_reason: "stop".into(),
            }),
        ])],
        vec![],
    ));
    let adapter = LlmBridgeAdapter::new(primary.clone(), Some(cheap.clone()));
    let config = LlmCallConfig {
        depth: 1,
        ..LlmCallConfig::default()
    };

    let chunks = adapter
        .complete_stream(&[], &[], &config)
        .await
        .expect("cheap stream should open")
        .collect::<Vec<_>>()
        .await;

    assert_eq!(primary.plain_calls(), 0);
    assert_eq!(cheap.plain_calls(), 1);
    assert!(matches!(&chunks[0], Ok(EngineChunk::TextDelta(text)) if text == "cheap"));
}

#[tokio::test]
async fn complete_stream_maps_setup_error() {
    let provider = Arc::new(ScriptedStreamingProvider::new(
        "primary",
        vec![StreamScript::SetupError(LlmError::RequestFailed {
            provider: "primary".into(),
            reason: "setup failed".into(),
        })],
        vec![],
    ));
    let adapter = LlmBridgeAdapter::new(provider, None);

    let error = match adapter
        .complete_stream(&[], &[], &LlmCallConfig::default())
        .await
    {
        Ok(_) => panic!("setup error should cross the bridge"),
        Err(error) => error,
    };

    assert!(matches!(error, EngineError::Llm { reason } if reason.contains("setup failed")));
}

#[tokio::test]
async fn complete_stream_maps_item_error() {
    let provider = Arc::new(ScriptedStreamingProvider::new(
        "primary",
        vec![StreamScript::Items(vec![Err(LlmError::RequestFailed {
            provider: "primary".into(),
            reason: "item failed".into(),
        })])],
        vec![],
    ));
    let adapter = LlmBridgeAdapter::new(provider, None);
    let items = adapter
        .complete_stream(&[], &[], &LlmCallConfig::default())
        .await
        .expect("stream should open")
        .collect::<Vec<_>>()
        .await;

    assert!(matches!(
        items.first(),
        Some(Err(EngineError::Llm { reason })) if reason.contains("item failed")
    ));
}
```

- [ ] **Step 3: Run bridge stream tests to verify RED**

Run:

```bash
taskset -c 0-5 cargo test -j6 --lib bridge::llm_adapter::tests::complete_stream_ -- --nocapture
```

Expected: tests fail because `LlmBridgeAdapter` inherits the engine's blocking
fallback instead of opening the scripted native host streams.

- [ ] **Step 4: Add request-parity characterization tests**

Add this test-local provider. The inherited host stream fallbacks call its
blocking methods, which records the exact request built by the bridge:

```rust
use std::sync::Mutex;

use crate::llm::{
    CompletionRequest, CompletionResponse, FinishReason, ToolCompletionResponse,
};

#[derive(Default)]
struct CapturingProvider {
    plain_requests: Mutex<Vec<CompletionRequest>>,
    tool_requests: Mutex<Vec<ToolCompletionRequest>>,
}

#[async_trait::async_trait]
impl LlmProvider for CapturingProvider {
    fn model_name(&self) -> &str {
        "capturing"
    }

    fn cost_per_token(&self) -> (Decimal, Decimal) {
        (Decimal::ZERO, Decimal::ZERO)
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, LlmError> {
        self.plain_requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(request);
        Ok(CompletionResponse {
            content: "plain".to_string(),
            input_tokens: 1,
            output_tokens: 1,
            finish_reason: FinishReason::Stop,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        })
    }

    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        self.tool_requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(request);
        Ok(ToolCompletionResponse {
            content: Some("tools".to_string()),
            tool_calls: Vec::new(),
            input_tokens: 1,
            output_tokens: 1,
            finish_reason: FinishReason::Stop,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        })
    }
}

#[tokio::test]
async fn stream_request_preserves_plain_config_and_metadata() {
    let provider = Arc::new(CapturingProvider::default());
    let adapter = LlmBridgeAdapter::new(provider.clone(), None);
    let config = LlmCallConfig {
        max_tokens: Some(1234),
        temperature: Some(0.25),
        force_text: true,
        depth: 0,
        metadata: [("thread_id".to_string(), "thread-1".to_string())]
            .into_iter()
            .collect(),
    };

    adapter
        .complete_stream(&[ThreadMessage::user("hello")], &[search_action()], &config)
        .await
        .expect("plain stream should open")
        .collect::<Vec<_>>()
        .await;

    let requests = provider.plain_requests.lock().expect("plain request lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].max_tokens, Some(1234));
    assert_eq!(requests[0].temperature, Some(0.25));
    assert_eq!(requests[0].metadata.get("thread_id").map(String::as_str), Some("thread-1"));
    assert!(provider.tool_requests.lock().expect("tool request lock").is_empty());
}

#[tokio::test]
async fn stream_request_preserves_tool_config_and_choice() {
    let provider = Arc::new(CapturingProvider::default());
    let adapter = LlmBridgeAdapter::new(provider.clone(), None);
    let config = LlmCallConfig {
        max_tokens: Some(2048),
        temperature: Some(0.5),
        metadata: [("trace".to_string(), "abc".to_string())]
            .into_iter()
            .collect(),
        ..LlmCallConfig::default()
    };

    adapter
        .complete_stream(&[ThreadMessage::user("search")], &[search_action()], &config)
        .await
        .expect("tool stream should open")
        .collect::<Vec<_>>()
        .await;

    let requests = provider.tool_requests.lock().expect("tool request lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].max_tokens, Some(2048));
    assert_eq!(requests[0].temperature, Some(0.5));
    assert_eq!(requests[0].tool_choice.as_deref(), Some("auto"));
    assert_eq!(requests[0].metadata.get("trace").map(String::as_str), Some("abc"));
}

#[tokio::test]
async fn stream_request_uses_blocking_defaults() {
    let provider = Arc::new(CapturingProvider::default());
    let adapter = LlmBridgeAdapter::new(provider.clone(), None);

    adapter
        .complete_stream(&[ThreadMessage::user("hello")], &[], &LlmCallConfig::default())
        .await
        .expect("default plain stream should open")
        .collect::<Vec<_>>()
        .await;

    let requests = provider.plain_requests.lock().expect("plain request lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].max_tokens, Some(4096));
    assert_eq!(requests[0].temperature, Some(0.7));
}
```

Run the complete bridge test module after adding these cases. Expected: the
request characterization tests pass through the inherited engine fallback;
the native mapping tests from Step 2 remain RED. These passing tests lock the
blocking defaults before request construction is shared.

- [ ] **Step 5: Share request construction and implement one-for-one chunk mapping**

Add a private request enum and helpers:

```rust
enum BridgeRequest {
    Plain(crate::llm::CompletionRequest),
    Tools(ToolCompletionRequest),
}

fn build_request(
    messages: &[ThreadMessage],
    actions: &[ActionDef],
    config: &LlmCallConfig,
) -> BridgeRequest {
    let chat_messages = messages.iter().map(thread_msg_to_chat).collect();
    let tools: Vec<ToolDefinition> = if config.force_text {
        Vec::new()
    } else {
        actions.iter().map(action_def_to_tool_def).collect()
    };
    let max_tokens = config.max_tokens.unwrap_or(4096);
    let temperature = config.temperature.unwrap_or(0.7);

    if tools.is_empty() {
        let mut request = crate::llm::CompletionRequest::new(chat_messages)
            .with_max_tokens(max_tokens)
            .with_temperature(temperature);
        request.metadata = config.metadata.clone();
        BridgeRequest::Plain(request)
    } else {
        let mut request = ToolCompletionRequest::new(chat_messages, tools)
            .with_max_tokens(max_tokens)
            .with_temperature(temperature)
            .with_tool_choice("auto");
        request.metadata = config.metadata.clone();
        BridgeRequest::Tools(request)
    }
}

fn map_provider_error(error: crate::llm::LlmError) -> EngineError {
    EngineError::Llm {
        reason: error.to_string(),
    }
}

fn map_usage(usage: crate::llm::TokenUsage) -> TokenUsage {
    map_usage_fields(
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_read_input_tokens,
        usage.cache_creation_input_tokens,
    )
}

fn map_usage_fields(
    input_tokens: u32,
    output_tokens: u32,
    cache_read_tokens: u32,
    cache_write_tokens: u32,
) -> TokenUsage {
    TokenUsage {
        input_tokens: u64::from(input_tokens),
        output_tokens: u64::from(output_tokens),
        cache_read_tokens: u64::from(cache_read_tokens),
        cache_write_tokens: u64::from(cache_write_tokens),
        cost_usd: 0.0,
    }
}

fn map_stream_chunk(chunk: crate::llm::LlmStreamChunk) -> lunarwing_engine::LlmStreamChunk {
    match chunk {
        crate::llm::LlmStreamChunk::TextDelta(text) => {
            lunarwing_engine::LlmStreamChunk::TextDelta(text)
        }
        crate::llm::LlmStreamChunk::ToolCallDelta {
            index,
            id,
            name,
            args_delta,
        } => lunarwing_engine::LlmStreamChunk::ToolCallDelta {
            index,
            id,
            name,
            args_delta,
        },
        crate::llm::LlmStreamChunk::Done {
            usage,
            finish_reason,
        } => lunarwing_engine::LlmStreamChunk::Done {
            usage: usage.map(map_usage),
            finish_reason,
        },
    }
}

fn map_completion_response(response: crate::llm::CompletionResponse) -> LlmOutput {
    let usage = map_usage_fields(
        response.input_tokens,
        response.output_tokens,
        response.cache_read_input_tokens,
        response.cache_creation_input_tokens,
    );
    LlmOutput {
        response: LlmResponse::from_text(response.content),
        usage,
    }
}

fn map_tool_response(response: crate::llm::ToolCompletionResponse) -> LlmOutput {
    let usage = map_usage_fields(
        response.input_tokens,
        response.output_tokens,
        response.cache_read_input_tokens,
        response.cache_creation_input_tokens,
    );
    let llm_response = if response.tool_calls.is_empty() {
        LlmResponse::from_text(response.content.unwrap_or_default())
    } else {
        LlmResponse::ActionCalls {
            calls: response
                .tool_calls
                .into_iter()
                .map(|call| lunarwing_engine::ActionCall {
                    id: call.id,
                    action_name: call.name,
                    parameters: call.arguments,
                })
                .collect(),
            content: response.content,
        }
    };
    LlmOutput {
        response: llm_response,
        usage,
    }
}
```

Replace `complete()` with the shared request construction and conversion:

```rust
async fn complete(
    &self,
    messages: &[ThreadMessage],
    actions: &[ActionDef],
    config: &LlmCallConfig,
) -> Result<LlmOutput, EngineError> {
    let provider = self.provider_for_depth(config.depth);
    match build_request(messages, actions, config) {
        BridgeRequest::Plain(request) => provider
            .complete(request)
            .await
            .map(map_completion_response)
            .map_err(map_provider_error),
        BridgeRequest::Tools(request) => provider
            .complete_with_tools(request)
            .await
            .map(map_tool_response)
            .map_err(map_provider_error),
    }
}
```

Add the native trait override:

```rust
async fn complete_stream<'a>(
    &'a self,
    messages: &[ThreadMessage],
    actions: &[ActionDef],
    config: &LlmCallConfig,
) -> Result<lunarwing_engine::LlmStream<'a>, EngineError> {
    let provider = self.provider_for_depth(config.depth);
    let stream = match build_request(messages, actions, config) {
        BridgeRequest::Plain(request) => provider.complete_stream(request).await,
        BridgeRequest::Tools(request) => provider.complete_with_tools_stream(request).await,
    }
    .map_err(map_provider_error)?;

    Ok(stream
        .map(|item| item.map(map_stream_chunk).map_err(map_provider_error))
        .boxed())
}
```

Import `futures::StreamExt`. The response helpers and terminal chunk mapper now
share `map_usage_fields`, so counter widening has one implementation.

- [ ] **Step 6: Run the complete bridge test module**

Run:

```bash
taskset -c 0-5 cargo test -j6 --lib bridge::llm_adapter::tests:: -- --nocapture
```

Expected: native text/tool mapping, request parity, depth selection, setup
errors, item errors, and existing response-classification tests all pass.

- [ ] **Step 7: Commit native bridge mapping**

```bash
git add ic/src/llm/mod.rs ic/src/bridge/llm_adapter.rs
git commit -m "feat: bridge native llm streams into engine"
```

### Task 5: Consume Streams In The Real Engine V2 Orchestrator

**Files:**
- Modify: `ic/crates/lunarwing_engine/src/executor/orchestrator.rs:16-28,391-403,536-625`
- Modify: `ic/crates/lunarwing_engine/src/executor/loop_engine.rs:379-end`
- Test: `ic/crates/lunarwing_engine/src/executor/loop_engine.rs`

- [ ] **Step 1: Add a native-stream-only engine test backend**

Inside the existing `loop_engine.rs` test module, add imports for `VecDeque`,
atomics, `futures::StreamExt`, `LlmStream`, and `LlmStreamChunk`. Add this
backend beside `MockLlm`:

```rust
struct StreamingMockLlm {
    streams: Mutex<VecDeque<Vec<Result<LlmStreamChunk, EngineError>>>>,
    blocking_calls: std::sync::atomic::AtomicUsize,
    streaming_calls: std::sync::atomic::AtomicUsize,
}

impl StreamingMockLlm {
    fn new(streams: Vec<Vec<Result<LlmStreamChunk, EngineError>>>) -> Self {
        Self {
            streams: Mutex::new(streams.into()),
            blocking_calls: std::sync::atomic::AtomicUsize::new(0),
            streaming_calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl LlmBackend for StreamingMockLlm {
    async fn complete(
        &self,
        _messages: &[ThreadMessage],
        _actions: &[ActionDef],
        _config: &LlmCallConfig,
    ) -> Result<LlmOutput, EngineError> {
        self.blocking_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Err(EngineError::Llm {
            reason: "blocking completion must not be called".to_string(),
        })
    }

    async fn complete_stream<'a>(
        &'a self,
        _messages: &[ThreadMessage],
        _actions: &[ActionDef],
        _config: &LlmCallConfig,
    ) -> Result<LlmStream<'a>, EngineError> {
        self.streaming_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let items = self
            .streams
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop_front()
            .ok_or_else(|| EngineError::Llm {
                reason: "no scripted engine stream remains".to_string(),
            })?;
        Ok(futures::stream::iter(items).boxed())
    }

    fn model_name(&self) -> &str {
        "streaming-mock"
    }
}
```

Refactor the existing `make_loop()` helper through this shared constructor so
native and complete-only backends use identical engine dependencies:

```rust
async fn make_loop_with_llm(
    llm: Arc<dyn LlmBackend>,
    effect_results: Vec<Result<ActionResult, EngineError>>,
    config: ThreadConfig,
) -> (ExecutionLoop, crate::runtime::messaging::SignalSender) {
    let project_id = ProjectId::new();
    let thread = Thread::new(
        "test goal",
        ThreadType::Foreground,
        project_id,
        "test-user",
        config,
    );
    let thread_id = thread.id;
    let effects = Arc::new(MockEffects::new(vec![test_action()], effect_results));
    let leases = Arc::new(LeaseManager::new());
    let policy = Arc::new(PolicyEngine::new());

    leases
        .grant(thread_id, "test_cap", GrantedActions::All, None, None)
        .await
        .expect("default test lease should be granted");
    let (signal_tx, signal_rx) = crate::runtime::messaging::signal_channel(16);
    let execution = ExecutionLoop::new(
        thread,
        llm,
        effects,
        leases,
        policy,
        signal_rx,
        "test-user".to_string(),
    );
    (execution, signal_tx)
}

async fn make_loop(
    llm_responses: Vec<LlmOutput>,
    effect_results: Vec<Result<ActionResult, EngineError>>,
    config: ThreadConfig,
) -> (ExecutionLoop, crate::runtime::messaging::SignalSender) {
    make_loop_with_llm(
        Arc::new(MockLlm::new(llm_responses)),
        effect_results,
        config,
    )
    .await
}
```

- [ ] **Step 2: Add the failing real-orchestrator text/event test**

```rust
#[tokio::test]
async fn orchestrator_uses_native_text_stream_and_broadcasts_deltas() {
    let usage = TokenUsage {
        input_tokens: 8,
        output_tokens: 3,
        ..TokenUsage::default()
    };
    let llm = Arc::new(StreamingMockLlm::new(vec![vec![
        Ok(LlmStreamChunk::TextDelta("hel".to_string())),
        Ok(LlmStreamChunk::TextDelta("lo".to_string())),
        Ok(LlmStreamChunk::Done {
            usage: Some(usage),
            finish_reason: "stop".to_string(),
        }),
    ]]));
    let (exec, _signal_tx) = make_loop_with_llm(
        llm.clone(),
        Vec::new(),
        ThreadConfig::default(),
    )
    .await;
    let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(32);
    let mut exec = exec.with_event_tx(event_tx);

    let outcome = exec.run().await.expect("streamed orchestrator should run");

    assert!(matches!(
        outcome,
        ThreadOutcome::Completed { response: Some(response) } if response == "hello"
    ));
    assert_eq!(
        llm.blocking_calls.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    assert_eq!(
        llm.streaming_calls.load(std::sync::atomic::Ordering::Relaxed),
        1
    );

    let mut deltas = Vec::new();
    while let Ok(event) = event_rx.try_recv() {
        if let EventKind::ResponseDelta { content } = event.kind {
            deltas.push(content);
        }
    }
    assert_eq!(deltas, vec!["hel", "lo"]);
    assert!(!exec
        .thread
        .events
        .iter()
        .any(|event| matches!(&event.kind, EventKind::ResponseDelta { .. })));
    assert_eq!(exec.thread.total_tokens_used, usage.total());
}
```

- [ ] **Step 3: Run the text/event test to verify RED**

Run:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  executor::loop_engine::tests::orchestrator_uses_native_text_stream_and_broadcasts_deltas \
  -- --exact --nocapture
```

Expected: the test fails because `handle_llm_complete()` calls `complete()` and
the streaming mock rejects that call.

- [ ] **Step 4: Add failing native code, tool, error, and no-receiver tests**

Add these four full-loop tests. They use exact native chunks rather than the
complete-only `LlmOutput` fixtures:

```rust
#[tokio::test]
async fn orchestrator_reconstructs_native_code_stream() {
    let code_text = "```repl\nFINAL('streamed code')\n```";
    let llm = Arc::new(StreamingMockLlm::new(vec![vec![
        Ok(LlmStreamChunk::TextDelta(code_text.to_string())),
        Ok(LlmStreamChunk::Done {
            usage: Some(TokenUsage::default()),
            finish_reason: "stop".to_string(),
        }),
    ]]));
    let (execution, _signal_tx) = make_loop_with_llm(
        llm,
        Vec::new(),
        ThreadConfig::default(),
    )
    .await;
    let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(32);
    let mut execution = execution.with_event_tx(event_tx);

    let outcome = execution
        .run()
        .await
        .expect("native code stream should run");

    assert!(matches!(
        outcome,
        ThreadOutcome::Completed { response: Some(response) } if response == "streamed code"
    ));
    let mut code_deltas = Vec::new();
    while let Ok(event) = event_rx.try_recv() {
        if let EventKind::ResponseDelta { content } = event.kind {
            code_deltas.push(content);
        }
    }
    assert_eq!(code_deltas, vec![code_text.to_string()]);
}

#[tokio::test]
async fn orchestrator_reconstructs_native_tool_stream() {
    let llm = Arc::new(StreamingMockLlm::new(vec![
        vec![
            Ok(LlmStreamChunk::TextDelta("I will run it.".to_string())),
            Ok(LlmStreamChunk::ToolCallDelta {
                index: 0,
                id: Some("call-1".to_string()),
                name: Some("test_tool".to_string()),
                args_delta: "{".to_string(),
            }),
            Ok(LlmStreamChunk::ToolCallDelta {
                index: 0,
                id: None,
                name: None,
                args_delta: "}".to_string(),
            }),
            Ok(LlmStreamChunk::Done {
                usage: Some(TokenUsage {
                    input_tokens: 4,
                    output_tokens: 2,
                    ..TokenUsage::default()
                }),
                finish_reason: "tool_calls".to_string(),
            }),
        ],
        vec![
            Ok(LlmStreamChunk::TextDelta("Done!".to_string())),
            Ok(LlmStreamChunk::Done {
                usage: Some(TokenUsage {
                    input_tokens: 5,
                    output_tokens: 1,
                    ..TokenUsage::default()
                }),
                finish_reason: "stop".to_string(),
            }),
        ],
    ]));
    let effect = ActionResult {
        call_id: "call-1".to_string(),
        action_name: "test_tool".to_string(),
        output: serde_json::json!({"ok": true}),
        is_error: false,
        duration: Duration::from_millis(1),
    };
    let (mut execution, _signal_tx) = make_loop_with_llm(
        llm.clone(),
        vec![Ok(effect)],
        ThreadConfig::default(),
    )
    .await;

    let outcome = execution
        .run()
        .await
        .expect("native tool stream should run");

    assert!(matches!(
        outcome,
        ThreadOutcome::Completed { response: Some(response) } if response == "Done!"
    ));
    assert_eq!(
        llm.streaming_calls.load(std::sync::atomic::Ordering::Relaxed),
        2
    );
    assert!(execution.thread.internal_messages.iter().any(|message| {
        message.role == crate::types::message::MessageRole::ActionResult
            && message.action_call_id.as_deref() == Some("call-1")
    }));
}

#[tokio::test]
async fn orchestrator_stream_failure_does_not_commit_usage() {
    let llm = Arc::new(StreamingMockLlm::new(vec![vec![
        Ok(LlmStreamChunk::TextDelta("partial".to_string())),
        Err(EngineError::Llm {
            reason: "mid-stream failure".to_string(),
        }),
    ]]));
    let (execution, _signal_tx) = make_loop_with_llm(
        llm,
        Vec::new(),
        ThreadConfig::default(),
    )
    .await;
    let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(32);
    let mut execution = execution.with_event_tx(event_tx);

    let outcome = execution
        .run()
        .await
        .expect("engine loop should convert orchestrator error to failed outcome");

    assert!(matches!(outcome, ThreadOutcome::Failed { .. }));
    assert_eq!(execution.thread.total_tokens_used, 0);
    let mut deltas = Vec::new();
    while let Ok(event) = event_rx.try_recv() {
        if let EventKind::ResponseDelta { content } = event.kind {
            deltas.push(content);
        }
    }
    assert_eq!(deltas, vec!["partial"]);
}

#[tokio::test]
async fn orchestrator_streams_without_event_sender() {
    let llm = Arc::new(StreamingMockLlm::new(vec![vec![
        Ok(LlmStreamChunk::TextDelta("no receiver".to_string())),
        Ok(LlmStreamChunk::Done {
            usage: None,
            finish_reason: "stop".to_string(),
        }),
    ]]));
    let (mut execution, _signal_tx) = make_loop_with_llm(
        llm.clone(),
        Vec::new(),
        ThreadConfig::default(),
    )
    .await;

    let outcome = execution
        .run()
        .await
        .expect("stream should not require an event sender");

    assert!(matches!(
        outcome,
        ThreadOutcome::Completed { response: Some(response) } if response == "no receiver"
    ));
    assert_eq!(
        llm.streaming_calls.load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}
```

- [ ] **Step 5: Switch only the primary host call to strict stream collection**

Import the collector and move the response/output types into the module-level
imports:

```rust
use super::llm_stream::collect_llm_stream;
use crate::traits::llm::{LlmBackend, LlmCallConfig, LlmOutput};
use crate::types::step::{LlmResponse, StepId, TokenUsage};
```

Remove the unused `_kwargs` argument from `handle_llm_complete()` so adding the
event sender keeps the function at seven arguments. Update the dispatch call to
pass `event_tx`:

```rust
handle_llm_complete(
    args,
    thread,
    llm,
    effects,
    leases,
    &mut total_tokens,
    event_tx,
)
.await
```

Use this handler signature:

```rust
async fn handle_llm_complete(
    args: &[MontyObject],
    thread: &mut Thread,
    llm: &Arc<dyn LlmBackend>,
    effects: &Arc<dyn EffectExecutor>,
    leases: &Arc<LeaseManager>,
    total_tokens: &mut TokenUsage,
    event_tx: Option<&tokio::sync::broadcast::Sender<ThreadEvent>>,
) -> ExtFunctionResult {
```

Open and collect the stream before the existing output-to-Monty conversion:

```rust
let stream = match llm.complete_stream(&messages, &actions, &config).await {
    Ok(stream) => stream,
    Err(error) => return llm_error_result(error),
};
let thread_id = thread.id;
let output = collect_llm_stream(stream, |content| {
    let Some(tx) = event_tx else {
        return;
    };
    let event = ThreadEvent::new(
        thread_id,
        EventKind::ResponseDelta {
            content: content.to_string(),
        },
    );
    let _ = tx.send(event);
})
.await;

match output {
    Ok(output) => llm_output_result(output, total_tokens),
    Err(error) => llm_error_result(error),
}
```

Close the handler after the result match. Extract the existing successful
conversion without changing its JSON shapes or usage accounting:

```rust
fn llm_output_result(
    output: LlmOutput,
    total_tokens: &mut TokenUsage,
) -> ExtFunctionResult {
    total_tokens.input_tokens += output.usage.input_tokens;
    total_tokens.output_tokens += output.usage.output_tokens;
    total_tokens.cost_usd += output.usage.cost_usd;

    let usage = serde_json::json!({
        "input_tokens": output.usage.input_tokens,
        "output_tokens": output.usage.output_tokens,
        "cost_usd": output.usage.cost_usd,
    });
    let result = match output.response {
        LlmResponse::Text(text) => {
            serde_json::json!({"type": "text", "content": text, "usage": usage})
        }
        LlmResponse::Code { code, .. } => {
            serde_json::json!({"type": "code", "code": code, "usage": usage})
        }
        LlmResponse::ActionCalls { calls, content } => {
            let calls: Vec<serde_json::Value> = calls
                .into_iter()
                .map(|call| {
                    serde_json::json!({
                        "name": call.action_name,
                        "call_id": call.id,
                        "params": call.parameters,
                    })
                })
                .collect();
            serde_json::json!({
                "type": "actions",
                "content": content,
                "calls": calls,
                "usage": usage,
            })
        }
    };
    ExtFunctionResult::Return(json_to_monty(&result))
}
```

Extract the existing error branch into:

```rust
fn llm_error_result(error: EngineError) -> ExtFunctionResult {
    ExtFunctionResult::Error(monty::MontyException::new(
        monty::ExcType::RuntimeError,
        Some(format!("LLM call failed: {error}")),
    ))
}
```

Do not append `ResponseDelta` to `thread.events` and do not change any other
call to `llm.complete()`.

- [ ] **Step 6: Run all new orchestrator integration tests**

Run:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  executor::loop_engine::tests::orchestrator_ -- --nocapture
```

Expected: native text, code, tool, failure, and no-receiver tests all pass.

- [ ] **Step 7: Run blocking-fallback regression tests through the real loop**

Run:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  executor::loop_engine::tests::text_response_completes -- --exact --nocapture
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  executor::loop_engine::tests::action_then_text -- --exact --nocapture
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  executor::loop_engine::tests::codeact_simple_final -- --exact --nocapture
```

Expected: all three existing complete-only `MockLlm` tests pass through the
Phase 0 stream fallback, proving text/action/code Monty dictionary parity.

- [ ] **Step 8: Confirm auxiliary engine calls remain blocking**

Run:

```bash
rg -n "llm\.complete\(|complete_stream\(" \
  crates/lunarwing_engine/src/executor/compaction.rs \
  crates/lunarwing_engine/src/executor/scripting.rs \
  crates/lunarwing_engine/src/runtime/mission.rs \
  crates/lunarwing_engine/src/executor/orchestrator.rs
```

Expected: `complete_stream()` appears only in the primary orchestrator handler;
compaction, scripting, and mission calls still use `complete()`.

- [ ] **Step 9: Commit the engine consumer and integration coverage**

```bash
git add ic/crates/lunarwing_engine/src/executor/orchestrator.rs \
  ic/crates/lunarwing_engine/src/executor/loop_engine.rs
git commit -m "feat: stream engine orchestrator completions"
```

### Task 6: Update Status Documentation And Run Full Verification

**Files:**
- Modify: `docs/proposals/ENGINE_LLM_STREAMING.md`
- Check unchanged: `ic/FEATURE_PARITY.md`

- [ ] **Step 1: Update the streaming proposal after implementation passes**

Change the status section to say Phase 2 is complete on the implementation
branch. Replace the first three items under `Remaining delivery path` with a
completed-engine summary:

```markdown
The bridge and primary Engine V2 executor now consume native provider streams,
strictly reconstruct the existing text/code/action result, and broadcast
transient provider-neutral `ResponseDelta` thread events. Provider streaming is
still not user-visible because the bridge/router does not translate those
events into channel statuses yet.
```

Update the rollout entry to:

```markdown
- **Phase 2 - implemented:** the host bridge maps native streams, the primary
  Engine V2 orchestrator consumes them strictly, and engine text deltas are
  broadcast as transient provider-neutral events.
```

Add this paragraph to the Validation section. Keep Phase 3 as the next
milestone and preserve the gateway-only/WASM safety language.

```markdown
Phase 2 coverage adds host-to-engine plain/tool chunk mapping, request/default
parity, depth routing, setup and item error mapping, strict engine terminal and
tool reconstruction, complete-only fallback compatibility, response-delta
Serde coverage, and real orchestrator text/code/tool/error/no-receiver streams.
The integration tests also prove ordered live delta broadcast, non-persistence
of provider chunks, and terminal-only usage commitment.
```

- [ ] **Step 2: Confirm no feature parity status change is warranted**

Run:

```bash
rg -n "Engine V2|stream|streaming" FEATURE_PARITY.md
git diff -- FEATURE_PARITY.md
```

Expected: the inspection shows Phase 2 remains non-user-visible and the diff is
empty.

- [ ] **Step 3: Format and check whitespace**

Run:

```bash
taskset -c 0-5 cargo fmt --all -- --check
git diff --check
```

Expected: both commands exit successfully with no output.

- [ ] **Step 4: Run all engine tests in tmux**

From `ic/`, start:

```bash
tmux new-session -d -s phase2-engine-tests \
  "bash -o pipefail -c 'taskset -c 0-5 cargo test -j6 -p lunarwing_engine -- --nocapture 2>&1 | tee /tmp/phase2-engine-tests.log'"
tmux set-option -t phase2-engine-tests remain-on-exit on
```

Monitor with:

```bash
tmux capture-pane -pt phase2-engine-tests -S -80
```

Expected: the session exits and the log ends with all engine tests passing.
After recording the final status, close the completed session with
`tmux kill-session -t phase2-engine-tests`.

- [ ] **Step 5: Run host bridge tests and compile checks**

Run:

```bash
taskset -c 0-5 cargo test -j6 --lib bridge::llm_adapter::tests:: -- --nocapture
taskset -c 0-5 cargo check -j6 -p lunarwing_engine
taskset -c 0-5 cargo check -j6 --lib
```

Expected: bridge tests pass and both compile checks finish successfully.

- [ ] **Step 6: Run Clippy with warnings denied**

Run:

```bash
taskset -c 0-5 cargo clippy -j6 -p lunarwing_engine --all-targets -- -D warnings
taskset -c 0-5 cargo clippy -j6 --lib -- -D warnings
```

Expected: both commands finish with zero warnings.

- [ ] **Step 7: Confirm scope and channel neutrality**

Run:

```bash
git diff --name-only 867e3f1..HEAD
rg -n "StatusUpdate|AppEvent|SseEvent|StreamChunk" \
  crates/lunarwing_engine/src/executor/llm_stream.rs \
  crates/lunarwing_engine/src/executor/orchestrator.rs \
  src/bridge/llm_adapter.rs
```

Expected: changed implementation files match the plan; the channel-type search
returns no new delivery dependency in the Phase 2 path. Documentation-only
commits made concurrently may also appear and must remain untouched.

- [ ] **Step 8: Commit the verified Phase 2 status update**

```bash
git add docs/proposals/ENGINE_LLM_STREAMING.md
git commit -m "docs: complete engine llm streaming phase 2"
```

## Completion Checklist

- [ ] Native host plain streams reach `LlmBridgeAdapter::complete_stream()`.
- [ ] Native host tool streams reach `LlmBridgeAdapter::complete_stream()`.
- [ ] Bridge request defaults, metadata, `force_text`, tool choice, and depth
  routing match blocking behavior.
- [ ] The strict collector rejects every incomplete or ambiguous stream listed
  in the design.
- [ ] Text, code, action, usage, and complete-only fallback parity are tested.
- [ ] The real Engine V2 orchestrator broadcasts ordered text deltas.
- [ ] Delta events never enter `thread.events`.
- [ ] Errors never commit terminal usage or synthesize completion.
- [ ] Auxiliary engine calls remain blocking.
- [ ] No channel, gateway, SSE, frontend, WIT, routing, configuration, database,
  or feature parity behavior changes.
- [ ] Formatting, tests, checks, Clippy, and `git diff --check` pass under the
  six-thread constraint without a debug build.
