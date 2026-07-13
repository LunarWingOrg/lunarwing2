# Engine LLM Streaming Phase 0 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add object-safe streaming contracts with blocking fallbacks to the host LLM provider and engine backend without changing runtime behavior.

**Architecture:** Define a typed `LlmStreamChunk` and boxed stream alias at each trait boundary. Each default method awaits the existing blocking completion, converts its result into a finite stream, and leaves current providers, decorators, bridge code, and consumers unchanged until later rollout phases override the method.

**Tech Stack:** Rust 2024, `async-trait`, `futures 0.3` boxed streams, existing LunarWing and `lunarwing_engine` unit-test conventions.

---

### Task 1: Host provider streaming primitive

**Files:**
- Modify: `ic/src/llm/provider.rs`
- Modify: `ic/src/llm/mod.rs`
- Modify: `ic/src/llm/reasoning.rs`
- Test: `ic/src/llm/provider.rs` unit tests

- [ ] **Step 1: Write the failing fallback test**

Add a `#[tokio::test]` using the existing `crate::testing::StubLlm`, call `complete_stream` through `Arc<dyn LlmProvider>`, collect the stream, and assert the exact sequence is `Ok(TextDelta("hello"))` followed by `Ok(Done { usage: Some(...), finish_reason: "stop" })` with the same text and token counts as `complete`.

- [ ] **Step 2: Run the focused test and verify the expected failure**

Run from `ic/`:

```bash
taskset -c 0-5 cargo test -j6 --lib llm::provider::tests::non_streaming_provider_falls_back_to_single_text_delta -- --exact --nocapture
```

Expected: compilation fails because `LlmProvider::complete_stream` and `LlmStreamChunk` do not exist yet.

- [ ] **Step 3: Implement the host stream type and default method**

Add the proposed `LlmStreamChunk` variants and `BoxStream` alias, move the existing host `TokenUsage` definition into the provider module while preserving the `crate::llm::TokenUsage` re-export, and add `LlmProvider::complete_stream` with a default implementation that calls `complete`, emits one text delta, then a terminal `Done` containing all completion token counts and `FinishReason::as_str()`.

- [ ] **Step 4: Run the focused test and verify it passes**

Run the same command; expected: the new test passes and the provider library target compiles.

- [ ] **Step 5: Run host LLM unit coverage**

```bash
taskset -c 0-5 cargo test -j6 --lib llm:: -- --nocapture
```

Expected: all existing host LLM tests and the new fallback test pass.

### Task 2: Engine backend streaming primitive

**Files:**
- Modify: `ic/crates/lunarwing_engine/Cargo.toml`
- Modify: `ic/crates/lunarwing_engine/src/traits/llm.rs`
- Modify: `ic/crates/lunarwing_engine/src/lib.rs`
- Test: `ic/crates/lunarwing_engine/src/traits/llm.rs` unit tests

- [ ] **Step 1: Write the failing engine fallback test**

Add a test-only backend implementing only `complete` and `model_name`, call `complete_stream` through `Arc<dyn LlmBackend>`, collect the stream, and assert a text output produces one `TextDelta` and one `Done` with the same engine `TokenUsage` and a `"stop"` finish reason.

- [ ] **Step 2: Run the focused test and verify the expected failure**

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine traits::llm::tests::non_streaming_backend_falls_back_to_single_text_delta -- --exact --nocapture
```

Expected: compilation fails because the engine stream type and trait method do not exist yet.

- [ ] **Step 3: Implement the engine stream type and fallback**

Add the engine-local `LlmStreamChunk` and boxed stream alias, add `LlmBackend::complete_stream` with an explicit lifetime tied to `&self`, call `complete`, convert text/code/action-call outputs into the proposed chunk sequence, and re-export the new public types from `lunarwing_engine`.

- [ ] **Step 4: Run the focused engine test and verify it passes**

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine traits::llm::tests::non_streaming_backend_falls_back_to_single_text_delta -- --exact --nocapture
```

Expected: the test passes and the engine crate compiles with all existing backend implementations inheriting the default.

- [ ] **Step 5: Run engine unit tests and formatting**

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine -- --test-threads=6
taskset -c 0-5 cargo fmt --all -- --check
```

Expected: all engine tests pass and rustfmt reports no changes.

### Task 3: Phase 0 integration verification

**Files:**
- Verify only: `ic/src/bridge/llm_adapter.rs`, all `ic/src/llm/*` decorators, `ic/FEATURE_PARITY.md`

- [ ] **Step 1: Confirm no adapter or decorator edits are required**

Compile the workspace and inspect the diff to ensure the bridge and decorator implementations still call their existing blocking methods and inherit the default stream fallback; do not add native forwarding semantics until Phase 1.

- [ ] **Step 2: Run the workspace compile check**

```bash
taskset -c 0-5 cargo check -j6
```

Expected: exit code 0 with no new diagnostics.

- [ ] **Step 3: Run the narrow diff and parity checks**

```bash
git diff --check
rg -n "complete_stream|LlmStreamChunk" ic/src/llm/provider.rs ic/crates/lunarwing_engine/src/traits/llm.rs
```

Expected: only the two trait boundaries and their exports contain the new Phase 0 API; `FEATURE_PARITY.md` remains unchanged because no user-visible streaming behavior is enabled.

