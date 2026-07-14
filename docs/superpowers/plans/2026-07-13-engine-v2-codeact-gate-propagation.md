# Engine V2 CodeAct Gate Propagation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every execute-time Engine V2 tool gate suspend CodeAct as a
structured `ThreadOutcome::GatePaused` instead of exposing the gate as a
catchable Monty `RuntimeError`.

**Architecture:** Keep policy and effect-adapter gate decisions unchanged.
Add a private typed boundary between completed tool futures and the Monty VM;
ordinary results and errors still resume Monty, while a gate is retained as
executor control flow. Resolve all already-spawned siblings, then return the
first gate without resuming Python so the bridge remains the sole owner of
pending-gate persistence and delivery.

**Tech Stack:** Rust 2024, Tokio tasks, Monty CodeAct executor, LunarWing
`EngineError`, `ThreadOutcome`, `LeaseManager`, and existing engine unit tests.

---

## File Map

- Modify `ic/crates/lunarwing_engine/src/executor/scripting.rs`: add focused
  CodeAct tests, introduce the private tool-future result enum, and propagate
  execute-time gates out of `ResolveFutures`.
- Check `ic/FEATURE_PARITY.md`: no status change is expected because this fixes
  an existing approval contract rather than adding a new feature.
- Modify `docs/superpowers/plans/2026-07-13-engine-llm-streaming-phase-5.md`
  only after deployment and live acceptance, recording the new commit and
  evidence without marking deferred DarkIRC or WeeChat gates complete.

### Task 1: Capture The Execute-Time Gate Regression

**Files:**
- Modify: `ic/crates/lunarwing_engine/src/executor/scripting.rs`
- Test: `ic/crates/lunarwing_engine/src/executor/scripting.rs`

- [x] **Step 1: Add a test helper that accepts an observable lease manager**

Extract the common executor call from `run_code` so accounting tests can inspect
the same `LeaseManager` after code execution:

```rust
async fn run_code_with_leases(
    code: &str,
    effects: Arc<dyn EffectExecutor>,
    thread: &Thread,
    leases: &LeaseManager,
) -> Result<CodeExecutionResult, EngineError> {
    let policy = PolicyEngine::new();
    let ctx = make_exec_context(thread);

    execute_code(
        code,
        thread,
        &(Arc::new(StubLlm) as Arc<dyn crate::traits::llm::LlmBackend>),
        &effects,
        leases,
        &policy,
        &ctx,
        &[],
        &serde_json::json!({}),
    )
    .await
}
```

Keep `run_code` as the wildcard-lease convenience wrapper and delegate its final
call to `run_code_with_leases`.

- [x] **Step 2: Add the primary execute-time approval regression test**

Add `execute_time_gate_returns_structured_pause`. Its mock action definition
must retain `requires_approval: false`, while `execute_action` returns:

```rust
EngineError::GatePaused {
    gate_name: "approval".into(),
    action_name: "http".into(),
    call_id: "effect_gate_call".into(),
    parameters: Box::new(serde_json::json!({
        "method": "POST",
        "url": "https://httpbin.org/anything",
        "body": {"probe": "PHASE5_HTTP_APPROVAL_OK"}
    })),
    resume_kind: Box::new(crate::gate::ResumeKind::Approval {
        allow_always: true,
    }),
    resume_output: None,
}
```

Run code that attempts to catch the tool error:

```python
try:
    await http(
        method="POST",
        url="https://httpbin.org/anything",
        body={"probe": "PHASE5_HTTP_APPROVAL_OK"},
    )
    FINAL("tool completed")
except Exception as error:
    print("caught: " + str(error))
    FINAL("caught gate")
```

Assert all of the following in the same regression:

```rust
assert!(result.final_answer.is_none());
assert!(!result.had_error, "gate control flow is not a script error");
assert!(!result.stdout.contains("execution paused by gate"));

match result.need_approval.as_ref() {
    Some(crate::runtime::messaging::ThreadOutcome::GatePaused {
        gate_name,
        action_name,
        call_id,
        parameters,
        resume_kind,
        resume_output,
    }) => {
        assert_eq!(gate_name, "approval");
        assert_eq!(action_name, "http");
        assert_eq!(call_id, "effect_gate_call");
        assert_eq!(parameters["method"], "POST");
        assert!(matches!(
            resume_kind,
            crate::gate::ResumeKind::Approval { allow_always: true }
        ));
        assert!(resume_output.is_none());
    }
    other => panic!("expected structured GatePaused, got {other:?}"),
}

assert!(result.events.iter().any(|event| matches!(
    event,
    EventKind::ApprovalRequested {
        action_name,
        call_id,
        allow_always: Some(true),
        gate_name: Some(gate_name),
        ..
    } if action_name == "http"
        && call_id == "effect_gate_call"
        && gate_name == "approval"
)));
```

Grant the test lease with `max_uses: Some(2)` and assert its remaining uses are
`Some(2)` after the pre-execution gate.

- [x] **Step 3: Add post-execution and sibling regressions before production code**

Add `post_execution_gate_preserves_resume_output_without_refund`, returning an
authentication gate with:

```rust
resume_kind: Box::new(crate::gate::ResumeKind::Authentication {
    credential_name: "github_token".into(),
    instructions: "Authorize to continue".into(),
    auth_url: None,
}),
resume_output: Some(Box::new(serde_json::json!({"already": "executed"}))),
```

Assert the complete `ThreadOutcome::GatePaused` retains that output and the
lease decreases from two uses to one.

Add `execute_time_gate_preserves_successful_sibling_result` using:

```python
import asyncio
await asyncio.gather(approval_tool(), echo())
FINAL("should not reach")
```

Return one `EngineError::GatePaused` and one successful `ActionResult` with
output `"sibling complete"`. Assert `need_approval.is_some()`,
`final_answer.is_none()`, and that `action_results` plus `ActionExecuted` retain
the successful sibling.

Strengthen the existing `gather_with_error_propagates` characterization with:

```rust
assert!(result.need_approval.is_none());
assert!(result.stdout.contains("tool exploded"));
```

This proves ordinary effect errors remain script-visible and are not promoted
to gates.

- [x] **Step 4: Run the primary regression and confirm RED**

From `ic/`:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  executor::scripting::tests::execute_time_gate_returns_structured_pause \
  -- --exact --nocapture
```

Expected: FAIL because `need_approval` is `None` and the script can receive the
`RuntimeError` text. Compilation errors in the test are not an acceptable RED;
fix the test until it fails on the behavioral assertion.

- [x] **Step 5: Run the other new tests against the old implementation**

Run each exact test with the same command shape. The post-execution and sibling
tests must fail because the gate is not structured. The ordinary-error
characterization must pass.

### Task 2: Propagate Gates As Executor Control Flow

**Files:**
- Modify: `ic/crates/lunarwing_engine/src/executor/scripting.rs`
- Test: `ic/crates/lunarwing_engine/src/executor/scripting.rs`

- [x] **Step 1: Add a private typed tool-future result**

Place this beside `PendingFuture`:

```rust
enum ToolFutureResolution {
    Monty(ExtFunctionResult),
    GatePaused(crate::runtime::messaging::ThreadOutcome),
}
```

- [x] **Step 2: Return the typed result from `resolve_tool_future`**

Change its return type to `ToolFutureResolution`. Wrap successful calls,
ordinary `EngineError` values, and task panics in the corresponding explicit
variant without changing their current events or Monty exception strings:

```rust
ToolFutureResolution::Monty(ExtFunctionResult::Return(monty_value))
ToolFutureResolution::Monty(ExtFunctionResult::Error(MontyException::new(
    ExcType::RuntimeError,
    Some(error_message),
)))
```

For `EngineError::GatePaused`, move the boxed payload into owned values, refund
only when `resume_output.is_none()`, emit `ApprovalRequested`, and return:

```rust
ToolFutureResolution::GatePaused(
    crate::runtime::messaging::ThreadOutcome::GatePaused {
        gate_name,
        action_name,
        call_id,
        parameters,
        resume_kind,
        resume_output,
    },
)
```

The event must clone from those owned values before the outcome consumes them.
Its `allow_always` value comes only from `ResumeKind::Approval`; authentication
continues to use `None`. Never construct a Monty exception for a gate.

- [x] **Step 3: Stop before `resolve.resume` when any tool gated**

In `RunProgress::ResolveFutures`, add:

```rust
let mut first_gate = None;
```

For a tool future, classify the typed result:

```rust
match resolve_tool_future(
    handle,
    &action_name,
    &call_id,
    lease_id,
    parameters,
    params_summary,
    leases,
    context,
    &mut action_results,
    &mut events,
)
.await
{
    ToolFutureResolution::Monty(result) => Some(result),
    ToolFutureResolution::GatePaused(outcome) => {
        if first_gate.is_none() {
            first_gate = Some(outcome);
        }
        None
    }
}
```

Keep resolving every pending ID, collecting all `Some` Monty results. Before
calling `resolve.resume`, return:

```rust
if let Some(outcome) = first_gate {
    return Ok(CodeExecutionResult {
        return_value: serde_json::Value::Null,
        stdout,
        action_results,
        events,
        need_approval: Some(outcome),
        recursive_tokens,
        final_answer: None,
        had_error,
    });
}
```

This is the control-flow boundary that prevents Python and the model from
catching or narrating the gate.

- [x] **Step 4: Run the four focused tests and confirm GREEN**

Run each exact test from Task 1. Expected: all pass. Then run the complete
scripting module:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine \
  executor::scripting::tests -- --nocapture
```

- [x] **Step 5: Format and inspect the focused diff**

```bash
taskset -c 0-5 cargo fmt --all
git diff --check
git diff -- ic/crates/lunarwing_engine/src/executor/scripting.rs
```

Expected: formatting and whitespace checks pass; no policy, bridge, UI, or
unrelated executor files change.

### Task 3: Verify, Publish, And Deploy To Brightdawn

**Files:**
- Check: `ic/FEATURE_PARITY.md`
- Modify after live acceptance:
  `docs/superpowers/plans/2026-07-13-engine-llm-streaming-phase-5.md`

- [x] **Step 1: Run engine compile, tests, and lint**

From `ic/`, with one Cargo process at a time:

```bash
taskset -c 0-5 cargo check -j6 -p lunarwing_engine
taskset -c 0-5 cargo test -j6 -p lunarwing_engine --lib -- --test-threads=6
taskset -c 0-5 cargo clippy -j6 -p lunarwing_engine --all-targets -- -D warnings
taskset -c 0-5 cargo fmt --all -- --check
```

Expected: every command exits zero with no warnings. Use tmux for any command
that becomes long-running; never use a debug `cargo build`.

- [x] **Step 2: Confirm parity and documentation scope**

Search `ic/FEATURE_PARITY.md` for Engine V2 approvals. Do not change a status
marker unless the fix alters the documented feature status. The committed design
spec already documents the behavior; defer the Phase 5 progress edit until live
evidence exists.

- [ ] **Step 3: Commit and push the scoped implementation**

```bash
git add \
  docs/superpowers/plans/2026-07-13-engine-v2-codeact-gate-propagation.md \
  ic/crates/lunarwing_engine/src/executor/scripting.rs
git commit -m "fix(engine): propagate CodeAct tool gates"
git push
```

- [ ] **Step 4: Fast-forward and release-build Brightdawn**

Fast-forward `/home/brightdawn/lunarwing` to the pushed integration branch as
the tenant user. Preserve `env/`, `state/`, generated lockfiles, and installed
WASM artifacts. In a tmux session, run the release-only lifecycle build:

```bash
sudo -n ic/scripts/lunarwing-mt-admin.sh build-tenant brightdawn
```

Then use the lifecycle owner for restart and status:

```bash
sudo -n ic/scripts/lunarwing-mt-admin.sh restart-tenant brightdawn
sudo -n ic/scripts/lunarwing-mt-admin.sh status brightdawn
```

- [ ] **Step 5: Repeat the live gateway HTTP approval acceptance**

Send one gateway prompt that requests an HTTP POST to
`https://httpbin.org/anything` with body
`{"probe":"PHASE5_HTTP_APPROVAL_OK"}`. Verify before approval that the thread is
`Waiting`, exactly one pending gate exists, and the UI shows one approval card.
Approve once; verify one network action, one result card, one terminal response,
and durable card contents after refresh and thread switching.

- [ ] **Step 6: Record the live checkpoint without broadening rollout**

Update the Phase 5 progress checkpoint with the implementation commit, automated
test counts, Brightdawn build/status evidence, and live HTTP result. Keep Phase 5
open and keep DarkIRC and WeeChat outside the allowlist. Commit and push the
documentation update only after the evidence is collected.

## Rollback And Safety Notes

- Rollback is a normal commit revert plus a Brightdawn release rebuild; there is
  no schema, configuration, permission-policy, or WASM artifact change.
- Dynamic HTTP method policy and all other effect-adapter gate decisions remain
  authoritative. This fix changes only how an already-returned gate crosses the
  CodeAct executor boundary.
- Do not clean up stale pending gates or alter the serialized dispatcher in this
  change.
- Never print tenant secrets while checking service state or pending gates.
