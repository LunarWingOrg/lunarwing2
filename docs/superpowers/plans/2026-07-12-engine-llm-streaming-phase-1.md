# Engine LLM Streaming Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add native Rig 0.40 streaming for plain and tool-capable requests and preserve those deltas through LunarWing's complete production provider/decorator chain without making streaming user-visible yet.

**Architecture:** Extend the host contract with a tool-capable streaming fallback, map Rig's typed stream into LunarWing's provider-neutral chunks, and give every production decorator an explicit streaming policy. Retry and failover may switch attempts only before the first successful chunk; cache and recording tee completed streams; SmartRouting buffers only the moderate cascade candidate; the existing timeout remains an absolute total turn budget.

**Tech Stack:** Rust 2024, `async-trait`, `futures 0.3` `BoxStream`/`unfold`/`StreamExt`, `rig-core 0.40.0`, Tokio tests, existing LunarWing provider decorators.

---

## Resume Checkpoint - 2026-07-12

Session stopped at the user's request before completing Task 1 or running the
first Task 2 test.

Current branch and committed baseline:

- Branch: `feat/wire-engine-v2`
- Phase 0 commit: `1c844d8`
- Channel-neutral design commit: `7f2f603`
- This Phase 1 plan is currently untracked and not committed.

Current uncommitted implementation changes:

- `ic/src/llm/provider.rs`
  - Added test-only `ToolFallbackProvider`.
  - Added `non_streaming_provider_falls_back_for_tool_stream`.
  - Added the default `LlmProvider::complete_with_tools_stream` blocking
    fallback.
- `ic/src/llm/rig_adapter.rs`
  - Added test-only `TestStreamingResponse` and `ScriptedCompletionModel`.
  - Added `complete_stream_emits_text_deltas_and_terminal_usage`.
  - This Rig test has not been compiled or run yet.

Verification completed:

```text
taskset -c 0-5 cargo test -j6 --lib \
  llm::provider::tests::non_streaming_provider_falls_back_for_tool_stream \
  -- --exact --nocapture

result: 1 passed; 0 failed
```

The test was first observed failing with `E0599` because
`complete_with_tools_stream` did not exist, then passed after the default method
was added.

Incomplete work and exact resume point:

1. Run the new Rig test from `ic/`:

   ```bash
   taskset -c 0-5 cargo test -j6 --lib \
     llm::rig_adapter::tests::complete_stream_emits_text_deltas_and_terminal_usage \
     -- --exact --nocapture
   ```

2. The expected RED result is that `RigAdapter` still uses the blocking
   fallback and the scripted model returns `blocking completion is not
   configured`.
3. Add the missing EOF-without-Final test before production Rig stream code.
4. Implement native Rig mapping only after both tests fail for the intended
   reasons.
5. Do not launch duplicate Cargo commands; a duplicate invocation in the prior
   session waited on the target-directory lock and made the session appear
   stalled.

No changes were made to decorators, engine code, gateway code, WASM channels,
configuration, or feature parity.

## Scope Amendment - Rig 0.40 Upgrade

The user approved upgrading `rig-core` from 0.30.0 to 0.40.0 before native
stream mapping. Keep the migration isolated to the dependency declaration,
lockfile, and the two modules that directly use Rig (`llm/mod.rs` and
`llm/rig_adapter.rs`). Restore existing blocking behavior and tests before
implementing Task 2.

Rig 0.40 changes relevant to this plan:

- replace the removed `reqwest-rustls` feature with explicit `reqwest` and
  `rustls` features;
- `GetTokenUsage::token_usage()` returns zero-sentinel `Usage`, not
  `Option<Usage>`;
- `StreamedAssistantContent` includes `Unknown(serde_json::Value)`;
- raw provider streams also contain internal metadata events, but those are
  consumed by Rig and are not exposed as typed assistant content.

## Production Provider Contract - TensorZero 2026.3.2

The deployed production provider is TensorZero Gateway 2026.3.2 through its
OpenAI-compatible `/openai/v1/chat/completions` endpoint. Task 2 must cover its
actual wire contract in addition to synthetic Rig events:

- JSON SSE data frames terminate with `data: [DONE]`;
- tool calls retain one numeric index while the ID is present only in the first
  chunk and name/argument fragments arrive incrementally;
- usage is present only when `stream_options.include_usage=true`; Rig 0.40's
  OpenAI streaming client adds that option;
- mid-stream failures are OpenAI-shaped JSON SSE events with a non-empty
  `error` object; Rig 0.40 recognizes them and terminates the stream instead of
  silently skipping them;
- `delta.tensorzero_extra_content` carries thought and unknown blocks that
  Rig's generic OpenAI parser does not expose. Phase 1 continues to omit
  reasoning, so preserving those blocks remains a later TensorZero-specific
  adapter decision.

## Scope And Locked Decisions

This plan is the provider-layer Phase 1 milestone. It does not implement the
engine consumer, `ResponseDelta`, gateway SSE consumption, interrupts, or WASM
channel routing. Those remain separate Phase 2 and channel-neutral delivery
plans.

The implementation must preserve these decisions:

- Add `complete_with_tools_stream()` now. Engine V2 normally supplies actions,
  so a plain-only host stream would leave CodeAct calls blocking. Partial tool
  deltas may be accumulated internally; user-visible tool-argument rendering
  remains deferred.
- Rig 0.40's generic final event exposes usage but no portable finish reason.
  Infer `tool_calls` after any tool event and otherwise report `stop`.
- Key Rig tool calls by `internal_call_id`, assigning stable LunarWing numeric
  indexes in first-seen order. Do not key by the provider ID, which may be empty
  on early deltas.
- Require Rig's `Final` event. EOF before `Final` is `InvalidResponse`; do not
  silently accept a truncated stream.
- Skip Rig reasoning events because `LlmStreamChunk` has no reasoning variant.
  Do not run full-response cleanup per delta.
- Retry and failover consume setup or first-item errors before exposing output.
  After the first successful chunk, later errors pass through without replay.
- SmartRouting's moderate cascade consumes the cheap stream to `Done`, checks
  uncertainty, then replays either the cheap chunks or a native primary stream.
  It never exposes cheap chunks before deciding whether to escalate.
- `TimeoutProvider` keeps its existing absolute turn-budget guarantee across
  stream acquisition and consumption. A separate first-token or idle timeout
  can be designed later; this task must not weaken the hard-kill safety margin.
- Phase 1 remains non-user-visible. `ic/FEATURE_PARITY.md` stays unchanged.

## File Map

- `ic/src/llm/provider.rs`: host stream contracts and blocking fallbacks.
- `ic/src/llm/streaming.rs`: shared request, first-item replay, and stream
  collection helpers.
- `ic/src/llm/streaming_test_support.rs`: deterministic scripted provider used
  only by unit tests.
- `ic/src/llm/rig_adapter.rs`: native Rig stream request construction and event
  mapping.
- `ic/src/llm/retry.rs`: pre-first-chunk retry policy.
- `ic/src/llm/failover.rs`: pre-first-chunk provider selection and cooldown.
- `ic/src/llm/circuit_breaker.rs`: terminal success and streamed failure
  accounting.
- `ic/src/llm/timeout.rs`: absolute deadline across the returned stream.
- `ic/src/llm/response_cache.rs`: cache-hit replay and cache-miss tee.
- `ic/src/llm/smart_routing.rs`: direct routes plus buffered moderate cascade.
- `ic/src/llm/recording.rs`: stream reassembly and terminal trace recording.
- `ic/src/llm/mod.rs`: module wiring and full-chain composition test.
- `docs/proposals/ENGINE_LLM_STREAMING.md`: Phase 0 status and corrected Phase 1
  semantics.

### Task 0: Upgrade Rig to 0.40

**Files:**
- Modify: `ic/Cargo.toml`
- Modify: `ic/Cargo.lock`
- Modify: `ic/src/llm/mod.rs`
- Modify: `ic/src/llm/rig_adapter.rs`
- Test: `ic/src/llm/rig_adapter.rs`

- [x] **Step 1: Update the dependency and TLS features**

Pin `rig-core` to `0.40` with default features disabled and explicit `reqwest`
and `rustls` features.

- [x] **Step 2: Run a compile check and capture migration failures**

```bash
taskset -c 0-5 cargo check -j6 --lib
```

Expected: compilation identifies any Rig 0.40 API changes in the two direct
consumer modules.

- [x] **Step 3: Restore blocking adapter and provider construction behavior**

Make the smallest API adaptations required by Rig 0.40. Do not add native
stream mapping in this task.

- [x] **Step 4: Verify existing Rig coverage**

```bash
taskset -c 0-5 cargo test -j6 --lib llm::rig_adapter::tests:: \
  -- --skip complete_stream_emits_text_deltas_and_terminal_usage --nocapture
```

Expected: all pre-streaming adapter tests pass. The saved native-stream test
remains RED until Task 2.

- [x] **Step 5: Commit the dependency migration**

```bash
git add ic/Cargo.toml ic/Cargo.lock ic/src/llm/mod.rs ic/src/llm/rig_adapter.rs \
  docs/superpowers/plans/2026-07-12-engine-llm-streaming-phase-1.md
git commit -m "chore: upgrade rig core to 0.40"
```

### Task 1: Complete the host streaming contract

**Files:**
- Modify: `ic/src/llm/provider.rs:212-232, 415-455`
- Create: `ic/src/llm/streaming.rs`
- Create: `ic/src/llm/streaming_test_support.rs`
- Modify: `ic/src/llm/mod.rs:8-30`
- Test: `ic/src/llm/provider.rs`

- [x] **Step 1: Write the failing tool-stream fallback test**

Add `non_streaming_provider_falls_back_for_tool_stream` beside the existing
plain fallback test. It must call `complete_with_tools_stream`, collect every
item, and assert text, full tool calls, usage, and terminal reason:

```rust
#[tokio::test]
async fn non_streaming_provider_falls_back_for_tool_stream() {
    let provider: Arc<dyn LlmProvider> = Arc::new(ToolFallbackProvider);
    let request = ToolCompletionRequest::new(
        vec![ChatMessage::user("search")],
        vec![ToolDefinition {
            name: "search".into(),
            description: "Search".into(),
            parameters: serde_json::json!({"type": "object"}),
        }],
    );

    let chunks = provider
        .complete_with_tools_stream(request)
        .await
        .expect("tool stream should open")
        .collect::<Vec<_>>()
        .await;

    assert!(matches!(
        &chunks[0],
        Ok(LlmStreamChunk::TextDelta(text)) if text == "I will search."
    ));
    assert!(matches!(
        &chunks[1],
        Ok(LlmStreamChunk::ToolCallDelta {
            index: 0,
            id: Some(id),
            name: Some(name),
            args_delta,
        }) if id == "call_1" && name == "search" && args_delta == "{\"q\":\"rust\"}"
    ));
    assert!(matches!(
        &chunks[2],
        Ok(LlmStreamChunk::Done {
            usage: Some(TokenUsage { input_tokens: 8, output_tokens: 4, .. }),
            finish_reason,
        }) if finish_reason == "tool_calls"
    ));
}
```

- [x] **Step 2: Run the focused test and verify the expected failure**

Run from `ic/`:

```bash
taskset -c 0-5 cargo test -j6 --lib llm::provider::tests::non_streaming_provider_falls_back_for_tool_stream -- --exact --nocapture
```

Expected: compilation fails because `LlmProvider::complete_with_tools_stream`
does not exist.

- [x] **Step 3: Add the tool-capable default method**

Refactor the existing plain fallback into private chunk builders and add this
object-safe trait method:

```rust
fn tool_fallback_chunks(
    response: ToolCompletionResponse,
) -> Vec<Result<LlmStreamChunk, LlmError>> {
    let mut chunks = Vec::new();
    if let Some(content) = response.content.filter(|content| !content.is_empty()) {
        chunks.push(Ok(LlmStreamChunk::TextDelta(content)));
    }
    for (index, call) in response.tool_calls.into_iter().enumerate() {
        chunks.push(Ok(LlmStreamChunk::ToolCallDelta {
            index,
            id: Some(call.id),
            name: Some(call.name),
            args_delta: call.arguments.to_string(),
        }));
    }
    chunks.push(Ok(LlmStreamChunk::Done {
        usage: Some(TokenUsage {
            input_tokens: response.input_tokens,
            output_tokens: response.output_tokens,
            cache_read_input_tokens: response.cache_read_input_tokens,
            cache_creation_input_tokens: response.cache_creation_input_tokens,
        }),
        finish_reason: response.finish_reason.as_str().to_string(),
    }));
    chunks
}

async fn complete_with_tools_stream(
    &self,
    request: ToolCompletionRequest,
) -> Result<LlmStream<'_>, LlmError> {
    let response = self.complete_with_tools(request).await?;
    Ok(stream::iter(tool_fallback_chunks(response)).boxed())
}
```

- [x] **Step 4: Add shared first-item and request helpers**

Create `ic/src/llm/streaming.rs` with exact typed request dispatch and replay
semantics:

```rust
use futures::StreamExt;
use futures::stream;

use crate::llm::error::LlmError;
use crate::llm::provider::{
    CompletionRequest, LlmProvider, LlmStream, LlmStreamChunk,
    ToolCompletionRequest,
};

#[derive(Clone)]
pub(crate) enum ProviderStreamRequest {
    Plain(CompletionRequest),
    Tools(ToolCompletionRequest),
}

impl ProviderStreamRequest {
    pub(crate) async fn open<'a>(
        &self,
        provider: &'a dyn LlmProvider,
    ) -> Result<LlmStream<'a>, LlmError> {
        match self {
            Self::Plain(request) => provider.complete_stream(request.clone()).await,
            Self::Tools(request) => provider.complete_with_tools_stream(request.clone()).await,
        }
    }
}

pub(crate) enum FirstStreamItem<'a> {
    Empty,
    Error(LlmError),
    Chunk {
        first: LlmStreamChunk,
        rest: LlmStream<'a>,
    },
}

pub(crate) async fn take_first(
    mut stream: LlmStream<'_>,
) -> FirstStreamItem<'_> {
    match stream.next().await {
        None => FirstStreamItem::Empty,
        Some(Err(error)) => FirstStreamItem::Error(error),
        Some(Ok(first)) => FirstStreamItem::Chunk {
            first,
            rest: stream,
        },
    }
}

pub(crate) fn replay_first(
    first: LlmStreamChunk,
    rest: LlmStream<'_>,
) -> LlmStream<'_> {
    stream::iter([Ok(first)]).chain(rest).boxed()
}
```

Use explicit named lifetimes if the inferred `'_` return lifetimes do not
compile; the input stream and returned `FirstStreamItem`/`LlmStream` must share
one lifetime.

- [x] **Step 5: Add deterministic stream test support**

Create `ic/src/llm/streaming_test_support.rs` behind `#[cfg(test)]` in
`llm/mod.rs`. Define `StreamScript::{SetupError, Items, DelayedItems}` and a
`ScriptedStreamingProvider` with separate plain/tool queues and atomic call
counts. Its stream conversion must be deterministic:

```rust
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
```

The provider's blocking methods return a fixed successful response; its two
stream methods pop from the corresponding queue. Expose `plain_calls()` and
`tool_calls()` so decorator tests can prove that retries/failovers did or did
not occur.

- [x] **Step 6: Run the contract tests**

```bash
taskset -c 0-5 cargo test -j6 --lib llm::provider::tests:: -- --nocapture
```

Expected: all provider fallback tests pass.

- [x] **Step 7: Commit the contract correction**

```bash
git add ic/src/llm/provider.rs ic/src/llm/streaming.rs ic/src/llm/streaming_test_support.rs ic/src/llm/mod.rs
git commit -m "feat: add tool-capable llm stream contract"
```

### Task 2: Implement native Rig 0.40 stream mapping

**Files:**
- Modify: `ic/src/llm/rig_adapter.rs:1-35, 474-552, 601-760`
- Test: `ic/src/llm/rig_adapter.rs`

- [x] **Step 1: Write failing native text and truncation tests**

Add a private `ScriptedCompletionModel` implementing Rig's `CompletionModel`
with a raw final response that implements `GetTokenUsage`. Add these tests:

```rust
#[tokio::test]
async fn complete_stream_emits_text_deltas_and_terminal_usage() {
    let model = ScriptedCompletionModel::plain(vec![
        Ok(RawStreamingChoice::Message("hel".into())),
        Ok(RawStreamingChoice::Message("lo".into())),
        Ok(RawStreamingChoice::FinalResponse(TestFinal::usage(7, 3, 2))),
    ]);
    let adapter = RigAdapter::new(model, "test-model");

    let chunks = adapter
        .complete_stream(CompletionRequest::new(vec![ChatMessage::user("hi")]))
        .await
        .expect("stream should open")
        .collect::<Vec<_>>()
        .await;

    assert_eq!(chunks[0], Ok(LlmStreamChunk::TextDelta("hel".into())));
    assert_eq!(chunks[1], Ok(LlmStreamChunk::TextDelta("lo".into())));
    assert!(matches!(
        &chunks[2],
        Ok(LlmStreamChunk::Done {
            usage: Some(TokenUsage {
                input_tokens: 7,
                output_tokens: 3,
                cache_read_input_tokens: 2,
                ..
            }),
            finish_reason,
        }) if finish_reason == "stop"
    ));
}

#[tokio::test]
async fn complete_stream_rejects_eof_without_final_event() {
    let model = ScriptedCompletionModel::plain(vec![
        Ok(RawStreamingChoice::Message("partial".into())),
    ]);
    let adapter = RigAdapter::new(model, "test-model");
    let chunks = adapter
        .complete_stream(CompletionRequest::new(vec![ChatMessage::user("hi")]))
        .await
        .expect("stream should open")
        .collect::<Vec<_>>()
        .await;

    assert!(matches!(chunks.last(), Some(Err(LlmError::InvalidResponse { .. }))));
}
```

- [x] **Step 2: Run the text test and verify it fails**

```bash
taskset -c 0-5 cargo test -j6 --lib llm::rig_adapter::tests::complete_stream_emits_text_deltas_and_terminal_usage -- --exact --nocapture
```

Expected: failure because `RigAdapter` still inherits the blocking fallback.

- [x] **Step 3: Add the Rig stream state and mapper**

Import `GetTokenUsage`, `StreamedAssistantContent`, and
`ToolCallDeltaContent`. Add an owned state used by `futures::stream::unfold`:

```rust
struct RigStreamState<R>
where
    R: Clone + Unpin + GetTokenUsage,
{
    upstream: rig_core::streaming::StreamingCompletionResponse<R>,
    provider: String,
    indexes: std::collections::HashMap<String, usize>,
    seen_deltas: HashSet<String>,
    next_index: usize,
    saw_tool: bool,
    terminal: bool,
}

fn rig_stream_index<R>(state: &mut RigStreamState<R>, internal_call_id: &str) -> usize
where
    R: Clone + Unpin + GetTokenUsage,
{
    if let Some(index) = state.indexes.get(internal_call_id) {
        return *index;
    }
    let index = state.next_index;
    state.next_index += 1;
    state.indexes.insert(internal_call_id.to_string(), index);
    index
}
```

The `unfold` match must handle every Rig 0.40 variant, including explicitly
skipping `StreamedAssistantContent::Unknown(_)`:

```rust
match state.upstream.next().await {
    Some(Ok(StreamedAssistantContent::Text(text))) if !text.text.is_empty() => {
        Some((Ok(LlmStreamChunk::TextDelta(text.text)), state))
    }
    Some(Ok(StreamedAssistantContent::Text(_)))
    | Some(Ok(StreamedAssistantContent::Reasoning(_)))
    | Some(Ok(StreamedAssistantContent::ReasoningDelta { .. }))
    | Some(Ok(StreamedAssistantContent::Unknown(_))) => {
        Some((Ok(LlmStreamChunk::TextDelta(String::new())), state))
    }
    Some(Ok(StreamedAssistantContent::ToolCallDelta {
        id,
        internal_call_id,
        content,
    })) => {
        let index = rig_stream_index(&mut state, &internal_call_id);
        state.saw_tool = true;
        state.seen_deltas.insert(internal_call_id);
        let (name, args_delta) = match content {
            ToolCallDeltaContent::Name(name) => (Some(name), String::new()),
            ToolCallDeltaContent::Delta(delta) => (None, delta),
        };
        Some((Ok(LlmStreamChunk::ToolCallDelta {
            index,
            id: (!id.is_empty()).then_some(id),
            name,
            args_delta,
        }), state))
    }
    Some(Ok(StreamedAssistantContent::ToolCall {
        tool_call,
        internal_call_id,
    })) => {
        state.saw_tool = true;
        let index = rig_stream_index(&mut state, &internal_call_id);
        if state.seen_deltas.contains(&internal_call_id) {
            return Some((Ok(LlmStreamChunk::ToolCallDelta {
                index,
                id: (!tool_call.id.is_empty()).then_some(tool_call.id),
                name: None,
                args_delta: String::new(),
            }), state));
        }
        Some((Ok(LlmStreamChunk::ToolCallDelta {
            index,
            id: (!tool_call.id.is_empty()).then_some(tool_call.id),
            name: Some(tool_call.function.name),
            args_delta: tool_call.function.arguments.to_string(),
        }), state))
    }
    Some(Ok(StreamedAssistantContent::Final(raw))) => {
        state.terminal = true;
        let raw_usage = raw.token_usage();
        let usage = raw_usage.has_values().then(|| TokenUsage {
            input_tokens: saturate_u32(raw_usage.input_tokens),
            output_tokens: saturate_u32(raw_usage.output_tokens),
            cache_read_input_tokens: saturate_u32(raw_usage.cached_input_tokens),
            cache_creation_input_tokens: saturate_u32(
                raw_usage.cache_creation_input_tokens,
            ),
        });
        let finish_reason = if state.saw_tool { "tool_calls" } else { "stop" };
        Some((Ok(LlmStreamChunk::Done {
            usage,
            finish_reason: finish_reason.to_string(),
        }), state))
    }
    Some(Err(error)) => {
        state.terminal = true;
        Some((Err(LlmError::RequestFailed {
            provider: state.provider.clone(),
            reason: error.to_string(),
        }), state))
    }
    None if !state.terminal => {
        state.terminal = true;
        Some((Err(LlmError::InvalidResponse {
            provider: state.provider.clone(),
            reason: "stream ended before Rig emitted a final response".into(),
        }), state))
    }
    None => None,
}
```

Do not actually emit empty text for skipped reasoning/empty events. Structure
the `unfold` helper to loop internally until it has a real LunarWing item or
reaches terminal state.

- [x] **Step 4: Implement both native request methods**

Extract request preparation helpers so blocking and streaming paths share
model-override warnings, unsupported-parameter stripping, message sanitation,
tool conversion, and cache settings. Then override both methods:

```rust
async fn complete_stream(
    &self,
    request: CompletionRequest,
) -> Result<LlmStream<'_>, LlmError> {
    let rig_request = self.build_plain_request(request)?;
    let upstream = self.model.stream(rig_request).await.map_err(|error| {
        LlmError::RequestFailed {
            provider: self.model_name.clone(),
            reason: error.to_string(),
        }
    })?;
    Ok(map_rig_stream(upstream, self.model_name.clone()))
}

async fn complete_with_tools_stream(
    &self,
    request: ToolCompletionRequest,
) -> Result<LlmStream<'_>, LlmError> {
    let (rig_request, _known_tool_names) = self.build_tool_request(request)?;
    let upstream = self.model.stream(rig_request).await.map_err(|error| {
        LlmError::RequestFailed {
            provider: self.model_name.clone(),
            reason: error.to_string(),
        }
    })?;
    Ok(map_rig_stream(upstream, self.model_name.clone()))
}
```

Add the explicit associated response bounds required for boxing the owned Rig
stream. Keep `M::Response` and `M::StreamingResponse` bounds separate.

- [x] **Step 5: Add tool, duplicate, and mid-stream error tests**

Add these exact tests:

- `complete_with_tools_stream_assigns_stable_indexes`
- `complete_with_tools_stream_suppresses_duplicate_full_arguments`
- `complete_with_tools_stream_synthesizes_full_tool_call`
- `complete_stream_surfaces_mid_stream_error_and_stops`
- `tensorzero_2026_3_2_stream_maps_text_usage_and_done`
- `tensorzero_2026_3_2_tool_stream_preserves_fragment_order`
- `tensorzero_2026_3_2_midstream_error_is_not_silently_dropped`

The duplicate test must emit Rig name/argument deltas followed by Rig's full
`ToolCall` and assert argument fragments appear once. The full-call test must
emit only `ToolCall` and assert LunarWing receives one complete delta. The
TensorZero tests must use a local mock HTTP/SSE endpoint with captured
2026.3.2-compatible frames and exercise Rig's OpenAI completions client through
`RigAdapter`; they must not depend on a live TensorZero service or the external
TensorZero checkout.

- [x] **Step 6: Run Rig adapter coverage**

```bash
taskset -c 0-5 cargo test -j6 --lib llm::rig_adapter::tests:: -- --nocapture
```

Expected: all adapter conversion and native stream tests pass without network
access.

- [x] **Step 7: Commit native Rig streaming**

```bash
git add ic/src/llm/rig_adapter.rs
git commit -m "feat: stream native rig completions"
```

### Task 3: Preserve streams through RetryProvider

**Files:**
- Modify: `ic/src/llm/retry.rs:140-228`
- Test: `ic/src/llm/retry.rs`

- [x] **Step 1: Write first-item retry tests**

Use `ScriptedStreamingProvider` to add:

- `stream_retries_setup_error_before_first_chunk`
- `stream_retries_first_item_error_before_first_chunk`
- `stream_does_not_retry_after_first_chunk`
- `tool_stream_uses_same_retry_policy`
- `retry_after_is_capped_in_retry_loop`

Use `RateLimited { retry_after: Some(Duration::ZERO) }` for retry tests so they
do not sleep for the jittered one-second backoff.

- [x] **Step 2: Run one focused test and verify failure**

```bash
taskset -c 0-5 cargo test -j6 --lib llm::retry::tests::stream_does_not_retry_after_first_chunk -- --exact --nocapture
```

Expected: the inherited fallback calls blocking `complete()` and does not
preserve the scripted stream.

- [x] **Step 3: Add a concrete borrowed-stream retry loop**

Do not reuse generic `retry_loop<T>` with `T = LlmStream`; the returned stream
borrows the provider. Add this explicit helper:

```rust
async fn retry_stream<'a>(
    &'a self,
    request: ProviderStreamRequest,
    label: &str,
) -> Result<LlmStream<'a>, LlmError> {
    for attempt in 0..=self.config.max_retries {
        let opened = request.open(self.inner.as_ref()).await;
        let first = match opened {
            Ok(stream) => take_first(stream).await,
            Err(error) => FirstStreamItem::Error(error),
        };

        match first {
            FirstStreamItem::Empty => return Ok(futures::stream::empty().boxed()),
            FirstStreamItem::Chunk { first, rest } => {
                return Ok(replay_first(first, rest));
            }
            FirstStreamItem::Error(error) => {
                if !is_retryable(&error) || attempt == self.config.max_retries {
                    return Err(error);
                }
                let delay = retry_delay(&error, attempt);
                tracing::warn!(
                    provider = %self.inner.model_name(),
                    attempt = attempt + 1,
                    max_retries = self.config.max_retries,
                    delay_ms = delay.as_millis() as u64,
                    error = %error,
                    "Retrying stream before first chunk{label}"
                );
                tokio::time::sleep(delay).await;
            }
        }
    }

    Err(LlmError::RequestFailed {
        provider: self.inner.model_name().to_string(),
        reason: "stream retry loop exhausted without a result".to_string(),
    })
}
```

Extract `retry_delay()` and use `cap_retry_after()` for both blocking and stream
loops.

- [x] **Step 4: Override both stream methods**

```rust
async fn complete_stream(
    &self,
    request: CompletionRequest,
) -> Result<LlmStream<'_>, LlmError> {
    self.retry_stream(ProviderStreamRequest::Plain(request), "").await
}

async fn complete_with_tools_stream(
    &self,
    request: ToolCompletionRequest,
) -> Result<LlmStream<'_>, LlmError> {
    self.retry_stream(ProviderStreamRequest::Tools(request), " (tools)").await
}
```

- [x] **Step 5: Run RetryProvider tests**

```bash
taskset -c 0-5 cargo test -j6 --lib llm::retry::tests:: -- --nocapture
```

Expected: blocking and streaming retry tests pass.

- [x] **Step 6: Commit retry streaming**

```bash
git add ic/src/llm/retry.rs
git commit -m "feat: retry llm streams before first chunk"
```

### Task 4: Preserve streams through FailoverProvider

**Files:**
- Modify: `ic/src/llm/failover.rs:108-381`
- Test: `ic/src/llm/failover.rs`

- [ ] **Step 1: Write failover boundary tests**

Add:

- `stream_fails_before_first_chunk_then_uses_fallback`
- `stream_midstream_error_does_not_fail_over`
- `stream_nonretryable_first_error_does_not_fail_over`
- `stream_success_binds_effective_model_before_consumption`
- `tool_stream_uses_same_failover_policy`

Assert call counts and exact delta order. The mid-stream test must receive the
primary text and then its error, with zero fallback calls.

- [ ] **Step 2: Run the fallback test and verify failure**

```bash
taskset -c 0-5 cargo test -j6 --lib llm::failover::tests::stream_fails_before_first_chunk_then_uses_fallback -- --exact --nocapture
```

Expected: failure because the decorator uses blocking `complete()`.

- [ ] **Step 3: Add concrete stream provider selection**

Add `try_stream_providers<'a>` instead of passing streams through the existing
generic closure, because each stream borrows a provider stored in `self`:

```rust
async fn try_stream_providers<'a>(
    &'a self,
    request: ProviderStreamRequest,
) -> Result<(usize, LlmStream<'a>), LlmError> {
    let available = self.available_provider_indexes()?;
    let mut last_error = None;

    for (position, index) in available.iter().copied().enumerate() {
        let provider = self.providers[index].as_ref();
        let first = match request.open(provider).await {
            Ok(stream) => take_first(stream).await,
            Err(error) => FirstStreamItem::Error(error),
        };

        match first {
            FirstStreamItem::Empty => return Ok((index, futures::stream::empty().boxed())),
            FirstStreamItem::Chunk { first, rest } => {
                self.last_used.store(index, Ordering::Relaxed);
                self.cooldowns[index].reset();
                return Ok((index, replay_first(first, rest)));
            }
            FirstStreamItem::Error(error) if !is_retryable(&error) => return Err(error),
            FirstStreamItem::Error(error) => {
                self.record_provider_failure(index);
                self.log_next_stream_provider(position, &available, index, &error);
                last_error = Some(error);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| LlmError::RequestFailed {
        provider: "failover".to_string(),
        reason: "all available providers failed before the first stream chunk".to_string(),
    }))
}
```

Extract `available_provider_indexes`, `record_provider_failure`, and the logging
helper from the current blocking loop so both paths use identical cooldown
selection and threshold behavior.

- [ ] **Step 4: Override plain and tool streams**

Each method calls `try_stream_providers`, binds the selected index to the
current Tokio task before returning, and returns the selected stream unchanged:

```rust
let (provider_index, stream) = self.try_stream_providers(request).await?;
self.bind_provider_to_current_task(provider_index);
Ok(stream)
```

- [ ] **Step 5: Run failover tests**

```bash
taskset -c 0-5 cargo test -j6 --lib llm::failover::tests:: -- --nocapture
```

Expected: cooldown, concurrent model attribution, blocking failover, and new
stream tests all pass.

- [ ] **Step 6: Commit failover streaming**

```bash
git add ic/src/llm/failover.rs
git commit -m "feat: fail over llm streams before first chunk"
```

### Task 5: Account for stream outcomes in CircuitBreakerProvider

**Files:**
- Modify: `ic/src/llm/circuit_breaker.rs:245-318`
- Test: `ic/src/llm/circuit_breaker.rs`

- [ ] **Step 1: Write circuit stream tests**

Add:

- `stream_setup_failure_counts_toward_breaker`
- `stream_midstream_failure_counts_toward_breaker`
- `stream_done_marks_success`
- `stream_drop_before_done_does_not_mark_success`
- `tool_stream_uses_same_breaker_accounting`

- [ ] **Step 2: Run the terminal success test and verify failure**

```bash
taskset -c 0-5 cargo test -j6 --lib llm::circuit_breaker::tests::stream_done_marks_success -- --exact --nocapture
```

Expected: the inherited fallback records success around blocking completion,
not at the streamed terminal chunk.

- [ ] **Step 3: Add the async outcome wrapper**

Keep setup checks method-level, then use `StreamExt::then` for asynchronous
breaker bookkeeping:

```rust
fn account_stream<'a>(&'a self, stream: LlmStream<'a>) -> LlmStream<'a> {
    stream
        .then(move |item| async move {
            match &item {
                Ok(LlmStreamChunk::Done { .. }) => self.record_success().await,
                Err(error) => self.record_failure(error).await,
                Ok(LlmStreamChunk::TextDelta(_))
                | Ok(LlmStreamChunk::ToolCallDelta { .. }) => {}
            }
            item
        })
        .boxed()
}
```

If opening the inner stream returns an error, call `record_failure` before
returning it. Dropping a stream before `Done` leaves breaker state unchanged.

- [ ] **Step 4: Override both stream methods**

Both methods must call `check_allowed()` before opening the inner stream and
must return `account_stream(stream)` only after setup succeeds.

- [ ] **Step 5: Run circuit breaker tests**

```bash
taskset -c 0-5 cargo test -j6 --lib llm::circuit_breaker::tests:: -- --nocapture
```

Expected: all state-machine and stream-accounting tests pass.

- [ ] **Step 6: Commit circuit accounting**

```bash
git add ic/src/llm/circuit_breaker.rs
git commit -m "feat: account for streamed circuit outcomes"
```

### Task 6: Enforce the total turn budget across streams

**Files:**
- Modify: `ic/src/llm/timeout.rs:1-102`
- Test: `ic/src/llm/timeout.rs`

- [ ] **Step 1: Write total-deadline tests**

Add:

- `complete_stream_times_out_before_first_chunk`
- `complete_stream_times_out_after_visible_chunk`
- `complete_stream_finishes_within_total_budget`
- `tool_stream_uses_same_total_budget`

Use a scripted delayed stream and assert a visible first chunk can be followed
by one `RequestFailed` timeout item, never a synthetic `Done`.

- [ ] **Step 2: Run the mid-stream timeout test and verify failure**

```bash
taskset -c 0-5 cargo test -j6 --lib llm::timeout::tests::complete_stream_times_out_after_visible_chunk -- --exact --nocapture
```

Expected: the inherited fallback either blocks before returning or ignores the
scripted stream timing.

- [ ] **Step 3: Add an absolute-deadline stream wrapper**

After bounding stream acquisition, wrap every poll against one deadline:

```rust
fn deadline_stream<'a>(
    &'a self,
    inner: LlmStream<'a>,
    deadline: tokio::time::Instant,
) -> LlmStream<'a> {
    futures::stream::unfold(
        (inner, false),
        move |(mut inner, finished)| async move {
            if finished {
                return None;
            }
            match tokio::time::timeout_at(deadline, inner.next()).await {
                Ok(Some(item)) => {
                    let terminal = matches!(item, Ok(LlmStreamChunk::Done { .. }) | Err(_));
                    Some((item, (inner, terminal)))
                }
                Ok(None) => None,
                Err(_) => Some((Err(self.elapsed_error()), (inner, true))),
            }
        },
    )
    .boxed()
}
```

Compute the deadline before awaiting the inner stream so setup time, retries,
failover, and consumption share one total budget.

- [ ] **Step 4: Override both stream methods**

Use `timeout_at(deadline, inner.complete_stream(...))` for acquisition. On
success, return `deadline_stream`; on timeout, return the existing retryable
turn-budget error.

- [ ] **Step 5: Run timeout tests**

```bash
taskset -c 0-5 cargo test -j6 --lib llm::timeout::tests:: -- --nocapture
```

Expected: blocking and native streams obey the configured total budget.

- [ ] **Step 6: Commit stream timeout support**

```bash
git add ic/src/llm/timeout.rs
git commit -m "feat: enforce turn budget on llm streams"
```

### Task 7: Tee plain streams through CachedProvider

**Files:**
- Modify: `ic/src/llm/response_cache.rs:55-317`
- Test: `ic/src/llm/response_cache.rs`

- [ ] **Step 1: Write cache stream tests**

Add:

- `stream_miss_forwards_chunks_and_populates_cache`
- `stream_hit_emits_cached_text_and_done`
- `stream_error_does_not_populate_cache`
- `stream_eof_without_done_does_not_populate_cache`
- `blocking_completion_and_stream_share_cache_entry`
- `tool_stream_bypasses_cache`

- [ ] **Step 2: Run the cache miss test and verify failure**

```bash
taskset -c 0-5 cargo test -j6 --lib llm::response_cache::tests::stream_miss_forwards_chunks_and_populates_cache -- --exact --nocapture
```

Expected: the inherited fallback returns one buffered delta and does not tee
the scripted stream.

- [ ] **Step 3: Extract cache lookup and insertion helpers**

Move the existing lock-scoped hit and insertion logic into helpers shared by
blocking and stream paths:

```rust
fn lookup(&self, key: &str, now: Instant, request_number: u64) -> Option<CompletionResponse>;

fn insert(
    &self,
    key: String,
    response: CompletionResponse,
    now: Instant,
    request_number: u64,
);
```

The bodies must preserve TTL checks, total hit counting, LRU eviction, and
statistics logging from the current `complete()` implementation.

- [ ] **Step 4: Add cached replay and miss tee**

Cache hits return:

```rust
fn cached_stream(response: CompletionResponse) -> LlmStream<'static> {
    let usage = TokenUsage {
        input_tokens: response.input_tokens,
        output_tokens: response.output_tokens,
        cache_read_input_tokens: response.cache_read_input_tokens,
        cache_creation_input_tokens: response.cache_creation_input_tokens,
    };
    futures::stream::iter([
        Ok(LlmStreamChunk::TextDelta(response.content)),
        Ok(LlmStreamChunk::Done {
            usage: Some(usage),
            finish_reason: response.finish_reason.as_str().to_string(),
        }),
    ])
    .boxed()
}
```

On a miss, use `unfold` state holding the inner stream, cache key, accumulated
text, terminal usage, and `&CachedProvider`. Forward every item unchanged. On
`Done`, reconstruct `CompletionResponse` and insert it before yielding the
terminal item. Never insert on `Err`, EOF before `Done`, or consumer drop.

- [ ] **Step 5: Delegate tool streams without caching**

`complete_with_tools_stream` must call the inner provider directly, matching
the existing tool-call side-effect policy.

- [ ] **Step 6: Run cache tests**

```bash
taskset -c 0-5 cargo test -j6 --lib llm::response_cache::tests:: -- --nocapture
```

Expected: blocking cache, eviction, statistics, stream interop, and bypass tests
pass.

- [ ] **Step 7: Commit cache stream support**

```bash
git add ic/src/llm/response_cache.rs
git commit -m "feat: cache completed llm streams"
```

### Task 8: Preserve SmartRouting semantics without duplicate output

**Files:**
- Modify: `ic/src/llm/smart_routing.rs:696-947`
- Test: `ic/src/llm/smart_routing.rs`

- [ ] **Step 1: Write route and cascade stream tests**

Add:

- `simple_stream_routes_to_cheap_provider`
- `complex_stream_routes_to_primary_provider`
- `moderate_cascade_replays_confident_cheap_stream`
- `moderate_cascade_discards_uncertain_cheap_before_primary_stream`
- `tool_stream_always_routes_to_primary_provider`

The escalation test must assert no cheap delta appears in the returned stream.

- [ ] **Step 2: Run the cascade test and verify failure**

```bash
taskset -c 0-5 cargo test -j6 --lib llm::smart_routing::tests::moderate_cascade_discards_uncertain_cheap_before_primary_stream -- --exact --nocapture
```

Expected: the decorator inherits its blocking fallback and cannot expose the
selected native stream.

- [ ] **Step 3: Add strict plain stream collection**

Add a private collector that retains chunks and reconstructs the cheap response
only after `Done`:

```rust
struct BufferedCompletion {
    chunks: Vec<LlmStreamChunk>,
    response: CompletionResponse,
}

async fn buffer_completion_stream(
    provider_name: &str,
    mut stream: LlmStream<'_>,
) -> Result<BufferedCompletion, LlmError> {
    let mut chunks = Vec::new();
    let mut content = String::new();
    while let Some(item) = stream.next().await {
        let chunk = item?;
        match &chunk {
            LlmStreamChunk::TextDelta(delta) => content.push_str(delta),
            LlmStreamChunk::ToolCallDelta { .. } => {
                return Err(LlmError::InvalidResponse {
                    provider: provider_name.to_string(),
                    reason: "plain smart-routing stream produced a tool call".to_string(),
                });
            }
            LlmStreamChunk::Done { usage, finish_reason } => {
                let usage = usage.unwrap_or_default();
                let response = CompletionResponse {
                    content,
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    finish_reason: FinishReason::from_stream_reason(finish_reason),
                    cache_read_input_tokens: usage.cache_read_input_tokens,
                    cache_creation_input_tokens: usage.cache_creation_input_tokens,
                };
                chunks.push(chunk);
                return Ok(BufferedCompletion { chunks, response });
            }
        }
        chunks.push(chunk);
    }
    Err(LlmError::InvalidResponse {
        provider: provider_name.to_string(),
        reason: "smart-routing stream ended before Done".to_string(),
    })
}
```

Add `FinishReason::from_stream_reason(&str)` in `provider.rs` with exhaustive
mapping for `stop`, `length`, `tool_calls`, `content_filter`, and unknown text.

- [ ] **Step 4: Override routing streams**

Simple and Complex directly call the selected provider's `complete_stream`.
Moderate without cascade directly calls cheap. Moderate with cascade buffers
cheap, calls `response_is_uncertain`, then either replays `chunks` with
`stream::iter(chunks.into_iter().map(Ok)).boxed()` or returns the primary native
stream. Tool streams always call `primary.complete_with_tools_stream`.

Update routing counters exactly where the blocking path updates them.

- [ ] **Step 5: Run smart-routing tests**

```bash
taskset -c 0-5 cargo test -j6 --lib llm::smart_routing::tests:: -- --nocapture
```

Expected: scoring, blocking routing, statistics, native routes, and buffered
cascade tests pass.

- [ ] **Step 6: Commit SmartRouting stream support**

```bash
git add ic/src/llm/provider.rs ic/src/llm/smart_routing.rs
git commit -m "feat: preserve smart routing for llm streams"
```

### Task 9: Reassemble completed streams in RecordingLlm

**Files:**
- Modify: `ic/src/llm/recording.rs:589-762`
- Test: `ic/src/llm/recording.rs`

- [ ] **Step 1: Write recording stream tests**

Add:

- `stream_records_reassembled_text_and_usage`
- `tool_stream_records_reassembled_tool_calls`
- `stream_error_does_not_record_partial_response`
- `stream_eof_without_done_does_not_record_response`

The error tests may still record a new `UserInput` marker through
`capture_new_messages`, but must not append a Text or ToolCalls response step.

- [ ] **Step 2: Run the text recording test and verify failure**

```bash
taskset -c 0-5 cargo test -j6 --lib llm::recording::tests::stream_records_reassembled_text_and_usage -- --exact --nocapture
```

Expected: the inherited fallback records the blocking result rather than the
scripted stream sequence.

- [ ] **Step 3: Add a stream trace accumulator**

Define an accumulator keyed by tool index:

```rust
#[derive(Default)]
struct StreamTraceAccumulator {
    content: String,
    tool_calls: std::collections::BTreeMap<usize, StreamTraceToolCall>,
}

#[derive(Default)]
struct StreamTraceToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}
```

Text deltas append content. Tool deltas update non-empty ID/name and append
arguments. At `Done`, convert to one `TraceStep`: ToolCalls if the map is
non-empty, otherwise Text. Parse complete argument strings as JSON; preserve a
malformed value as `serde_json::Value::String`.

- [ ] **Step 4: Tee both stream methods**

Call `capture_new_messages` before opening the inner stream. Wrap the stream
with `unfold`, forward every chunk unchanged, and append one response step only
when `Done` arrives. Do not append on error, premature EOF, or consumer drop.

Both plain and tool stream methods use the same tee helper, but plain streams
must reject unexpected tool deltas as `InvalidResponse` before recording.

- [ ] **Step 5: Run recording tests**

```bash
taskset -c 0-5 cargo test -j6 --lib llm::recording::tests:: -- --nocapture
```

Expected: existing trace fixtures and new terminal-only stream recording tests
pass.

- [ ] **Step 6: Commit recording support**

```bash
git add ic/src/llm/recording.rs
git commit -m "feat: record completed llm streams"
```

### Task 10: Verify the full decorator chain and update the proposal

**Files:**
- Modify: `ic/src/llm/mod.rs:305-476`
- Modify: `docs/proposals/ENGINE_LLM_STREAMING.md`
- Verify: `ic/FEATURE_PARITY.md`
- Test: `ic/src/llm/mod.rs`

- [ ] **Step 1: Write the failing full-chain composition test**

Manually compose a scripted native provider through Retry, SmartRouting,
Failover, CircuitBreaker, CachedProvider, TimeoutProvider, and RecordingLlm.
Use a Simple request so SmartRouting selects the scripted cheap provider. Assert
the exact output is two text deltas followed by one Done, with no duplicated
first item:

```rust
assert_eq!(
    chunks,
    vec![
        Ok(LlmStreamChunk::TextDelta("hel".into())),
        Ok(LlmStreamChunk::TextDelta("lo".into())),
        Ok(LlmStreamChunk::Done {
            usage: Some(TokenUsage {
                input_tokens: 3,
                output_tokens: 2,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            }),
            finish_reason: "stop".into(),
        }),
    ]
);
```

- [ ] **Step 2: Run the composition test and verify failure before all wrappers are complete**

```bash
taskset -c 0-5 cargo test -j6 --lib llm::tests::native_stream_survives_full_decorator_chain -- --exact --nocapture
```

Expected before Tasks 3-9: one or more wrappers collapse the stream to a
blocking fallback. Expected after Tasks 3-9: PASS.

- [ ] **Step 3: Add a tool-stream chain composition test**

Compose the same chain with `complete_with_tools_stream` and assert a text
delta, one tool delta, and `Done { finish_reason: "tool_calls" }` survive.
Confirm CachedProvider bypasses tool caching and SmartRouting chooses primary.

- [ ] **Step 4: Update the streaming proposal**

Update `docs/proposals/ENGINE_LLM_STREAMING.md` to record:

- Phase 0 complete in commit `1c844d8`.
- Host Phase 1 includes `complete_with_tools_stream` because engine action calls
  require it; user-visible partial tool rendering remains deferred.
- Rig 0.40 finish-reason limitation and LunarWing's inference policy.
- TensorZero Gateway 2026.3.2 SSE compatibility and its deferred
  `tensorzero_extra_content` limitation.
- Strict EOF-before-final behavior.
- Retry/failover first-successful-chunk commit point.
- SmartRouting's buffered moderate cascade.
- `TimeoutProvider` remains a total turn budget; the original TTFC-only note is
  replaced because weakening the existing watchdog would regress queue safety.
- Gateway and WASM delivery follows the approved channel-neutral design in
  `docs/superpowers/specs/2026-07-12-engine-v2-channel-neutral-delivery-design.md`.

Do not update `ic/FEATURE_PARITY.md`: no engine or user surface consumes native
provider deltas in Phase 1.

- [ ] **Step 5: Run focused provider coverage**

```bash
taskset -c 0-5 cargo test -j6 --lib llm::provider::tests:: -- --nocapture
taskset -c 0-5 cargo test -j6 --lib llm::rig_adapter::tests:: -- --nocapture
taskset -c 0-5 cargo test -j6 --lib llm::retry::tests:: -- --nocapture
taskset -c 0-5 cargo test -j6 --lib llm::failover::tests:: -- --nocapture
taskset -c 0-5 cargo test -j6 --lib llm::circuit_breaker::tests:: -- --nocapture
taskset -c 0-5 cargo test -j6 --lib llm::timeout::tests:: -- --nocapture
taskset -c 0-5 cargo test -j6 --lib llm::response_cache::tests:: -- --nocapture
taskset -c 0-5 cargo test -j6 --lib llm::smart_routing::tests:: -- --nocapture
taskset -c 0-5 cargo test -j6 --lib llm::recording::tests:: -- --nocapture
```

Expected: all focused provider and decorator tests pass.

- [ ] **Step 6: Run workspace verification**

```bash
taskset -c 0-5 cargo fmt --all -- --check
taskset -c 0-5 cargo check -j6
taskset -c 0-5 cargo clippy -j6 --all --benches --tests --examples -- -D warnings
git diff --check
```

Expected: formatting, compile check, Clippy, and whitespace checks pass. If root
Clippy still reports the previously observed unrelated `collapsible_if` in
`ic/src/agent/agent_loop.rs`, verify the changed LLM targets separately and
record that pre-existing failure without editing unrelated agent code.

- [ ] **Step 7: Commit Phase 1 integration**

```bash
git add ic/src/llm docs/proposals/ENGINE_LLM_STREAMING.md
git commit -m "feat: preserve native streams through llm decorators"
```

## Phase 1 Exit Criteria

- Both plain and tool-capable host requests have blocking fallbacks and native
  Rig overrides.
- A native Rig stream produces multiple LunarWing chunks before terminal Done.
- Truncated Rig streams fail rather than silently finalizing partial text.
- Retry and failover never replay after a successful chunk has escaped.
- Circuit breaker, timeout, cache, SmartRouting, and recording retain their
  documented behavior under streaming.
- The full production decorator shape preserves exact chunk order without
  duplication.
- No engine, gateway, XMPP, DarkIRC, or WeeChat behavior changes in Phase 1.
