# Engine V2 Tool Panel Persistence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Populate Engine V2 gateway tool cards during live execution and preserve bounded tool summaries across refreshes, thread switches, and approval resumes.

**Architecture:** Add a backward-compatible result preview to durable engine action events, deliver it through the existing Phase 5 channel status path, and reconstruct the v1-compatible `tool_calls` row from the terminal saved thread. Record bridge-executed approved actions through a narrow ThreadManager event method so live and durable paths share one event source.

**Tech Stack:** Rust 2024, Tokio broadcast channels, Serde/serde_json, LunarWing Engine V2, `ChannelManager::send_status`, PostgreSQL/libSQL-compatible conversation history.

**Design:** `docs/superpowers/specs/2026-07-13-engine-v2-tool-panel-persistence-design.md`

**Source Reference:** Commit `6aed96e` on `fix/ui/toolpanel-gui-fix-2`; manually port intent only. Do not cherry-pick it.

---

## File Map

- Modify `ic/crates/lunarwing_engine/src/types/event.rs`: define the preview contract and event field.
- Modify `ic/crates/lunarwing_engine/src/executor/{structured,scripting,orchestrator,trace,loop_engine}.rs`: populate and test previews.
- Modify `ic/crates/lunarwing_engine/src/runtime/manager.rs`: persist and broadcast a resolved waiting-thread event.
- Modify `ic/src/bridge/router.rs`: emit live `ToolResult`, record approved action results, and reconstruct durable history.
- Do not modify `ic/src/channels/web/static/app.js`: it already consumes both required contracts.

## Command Rules

Run every Cargo command from `ic/`, prefix it with `taskset -c 0-5`, and pass `-j6`. Do not run `cargo build` in debug mode. Keep one Cargo process active at a time.

### Task 1: Carry Bounded Result Previews In Engine Events

**Files:**
- Modify: `ic/crates/lunarwing_engine/src/types/event.rs`
- Modify: `ic/crates/lunarwing_engine/src/executor/structured.rs`
- Modify: `ic/crates/lunarwing_engine/src/executor/scripting.rs`
- Modify: `ic/crates/lunarwing_engine/src/executor/orchestrator.rs`
- Modify: `ic/crates/lunarwing_engine/src/executor/trace.rs`
- Modify: `ic/crates/lunarwing_engine/src/executor/loop_engine.rs`

- [ ] **Step 1: Write failing preview and compatibility tests**

In `types/event.rs`, import `preview_from_output` and add:

```rust
#[test]
fn action_result_preview_handles_strings_json_and_empty_values() {
    assert_eq!(preview_from_output(&serde_json::Value::Null), None);
    assert_eq!(preview_from_output(&serde_json::json!("")), None);
    assert_eq!(
        preview_from_output(&serde_json::json!("hello")).as_deref(),
        Some("hello")
    );
    assert_eq!(
        preview_from_output(&serde_json::json!({"count": 2})).as_deref(),
        Some("{\"count\":2}")
    );
}

#[test]
fn action_result_preview_truncates_at_utf8_boundary() {
    let preview = preview_from_output(&serde_json::json!("你".repeat(400)))
        .expect("non-empty output should produce a preview");
    assert!(preview.len() <= 1003);
    assert!(preview.ends_with("..."));
}
```

Update `structured.rs::call_id_preserved_on_successful_execution` to destructure `result_preview` and assert:

```rust
assert_eq!(result_preview.as_deref(), Some("{\"results\":[]}"));
```

In `scripting.rs::single_await_tool_call`, add:

```rust
let preview = result.events.iter().find_map(|event| match event {
    EventKind::ActionExecuted { result_preview, .. } => result_preview.as_deref(),
    _ => None,
});
assert_eq!(preview, Some("hello world"));
```

In `loop_engine.rs::action_executed_events_carry_call_id`, use output `{"data":"result"}` and assert the event preview is `Some("{\"data\":\"result\"}")`.

- [ ] **Step 2: Run the new tests and verify RED**

Run:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine action_result_preview -- --test-threads=6
```

Expected: compilation fails because `preview_from_output` and `ActionExecuted::result_preview` do not exist.

- [ ] **Step 3: Implement the bounded helper and event field**

Add after the existing `truncate` helper:

```rust
pub const ACTION_RESULT_PREVIEW_MAX_BYTES: usize = 1000;

pub fn preview_from_output(output: &serde_json::Value) -> Option<String> {
    let raw = match output {
        serde_json::Value::Null => return None,
        serde_json::Value::String(value) => value.clone(),
        value => value.to_string(),
    };
    if raw.is_empty() {
        None
    } else {
        Some(truncate(&raw, ACTION_RESULT_PREVIEW_MAX_BYTES))
    }
}
```

Add to `EventKind::ActionExecuted`:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
result_preview: Option<String>,
```

Add a serde test that constructs an `ActionExecuted` with `result_preview: None`, confirms serialization omits the field, then deserializes it and matches `result_preview: None`.

- [ ] **Step 4: Populate every constructor**

Use `preview_from_output` at real successful execution sites:

```rust
result_preview: crate::types::event::preview_from_output(&action_result.output),
```

Use the local variable `result.output` or `r.output` in `scripting.rs` and both orchestrator sites. Set `result_preview: None` in `orchestrator.rs::handle_emit_event` and the synthetic trace test constructor.

- [ ] **Step 5: Run the engine suite and verify GREEN**

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine --lib -- --test-threads=6
```

Expected: all engine library tests pass.

- [ ] **Step 6: Format and commit**

```bash
taskset -c 0-5 cargo fmt --all
git add crates/lunarwing_engine/src/types/event.rs crates/lunarwing_engine/src/executor/structured.rs crates/lunarwing_engine/src/executor/scripting.rs crates/lunarwing_engine/src/executor/orchestrator.rs crates/lunarwing_engine/src/executor/trace.rs crates/lunarwing_engine/src/executor/loop_engine.rs
git commit -m "feat(engine): retain bounded tool result previews"
```

Expected: one focused engine event commit.

### Task 2: Record Approved Actions On Waiting Threads

**Files:**
- Modify: `ic/crates/lunarwing_engine/src/runtime/manager.rs`

- [ ] **Step 1: Write a failing persistence-and-broadcast test**

Store a waiting thread in the existing `MockStore`, subscribe before recording, and verify the event is saved and broadcast once:

```rust
#[tokio::test]
async fn record_waiting_event_persists_and_broadcasts_once() {
    let store = Arc::new(MockStore::new());
    let mgr = make_manager_with_store(MockLlm::text("done"), Arc::clone(&store));
    let mut thread = Thread::new(
        "approved action",
        ThreadType::Foreground,
        ProjectId::new(),
        "alice",
        ThreadConfig::default(),
    );
    thread.state = ThreadState::Waiting;
    let thread_id = thread.id;
    store.save_thread(&thread).await.unwrap();

    let mut receiver = mgr.subscribe_events();
    mgr.record_waiting_event(
        thread_id,
        "alice",
        EventKind::ActionExecuted {
            step_id: StepId::new(),
            action_name: "ssh".into(),
            call_id: "call-approved".into(),
            duration_ms: 4,
            params_summary: None,
            result_preview: Some("ok".into()),
        },
    )
    .await
    .unwrap();

    let delivered = receiver.recv().await.unwrap();
    let saved = store.load_thread(thread_id).await.unwrap().unwrap();
    assert_eq!(saved.events.iter().filter(|event| event.id == delivered.id).count(), 1);
}
```

- [ ] **Step 2: Run the test and verify RED**

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine runtime::manager::tests::record_waiting_event_persists_and_broadcasts_once -- --exact --test-threads=6
```

Expected: compilation fails because `record_waiting_event` does not exist.

- [ ] **Step 3: Implement the narrow manager API**

Import `EventKind` and `ThreadEvent`, then add beside `subscribe_events`:

```rust
pub async fn record_waiting_event(
    &self,
    thread_id: ThreadId,
    user_id: &str,
    kind: EventKind,
) -> Result<(), EngineError> {
    let mut thread = self
        .store
        .load_thread(thread_id)
        .await?
        .ok_or(EngineError::ThreadNotFound(thread_id))?;
    if !thread.is_owned_by(user_id) {
        return Err(EngineError::AccessDenied {
            user_id: user_id.to_string(),
            entity: format!("thread {thread_id}"),
        });
    }
    if thread.state != ThreadState::Waiting {
        return Err(EngineError::Store {
            reason: format!("thread {thread_id} is not waiting"),
        });
    }

    let event = ThreadEvent::new(thread_id, kind);
    thread.events.push(event.clone());
    thread.updated_at = chrono::Utc::now();
    self.store.save_thread(&thread).await?;
    let _ = self.event_tx.send(event);
    Ok(())
}
```

The normal resumed run later appends its deduplicated complete event log; this method only needs to save the authoritative thread snapshot before broadcasting.

- [ ] **Step 4: Test ownership and state rejection**

Add one test that records as the wrong owner and on a non-waiting thread. Assert `AccessDenied` and `Store` respectively, and assert a 50 ms receive timeout for each rejected call.

- [ ] **Step 5: Run manager tests and commit**

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine runtime::manager::tests -- --test-threads=6
taskset -c 0-5 cargo fmt --all
git add crates/lunarwing_engine/src/runtime/manager.rs
git commit -m "feat(engine): record resolved waiting-thread actions"
```

Expected: all manager tests pass in one focused commit.

### Task 3: Deliver Live Tool Results Through Phase 5 Status Routing

**Files:**
- Modify: `ic/src/bridge/router.rs`

- [ ] **Step 1: Write a failing channel-neutral delivery test**

Model the test on `response_delta_reaches_gateway_and_other_channels_via_status`. Register gateway and XMPP stubs, forward this event to each, and expect three ordered statuses:

```rust
let event = lunarwing_engine::ThreadEvent::new(
    lunarwing_engine::ThreadId::new(),
    lunarwing_engine::EventKind::ActionExecuted {
        step_id: lunarwing_engine::StepId::new(),
        action_name: "shell".into(),
        call_id: "call-1".into(),
        duration_ms: 12,
        params_summary: Some("ls".into()),
        result_preview: Some("file.txt".into()),
    },
);
```

Assertions:

```rust
assert!(matches!(captured[0], StatusUpdate::ToolStarted { .. }));
assert!(matches!(
    &captured[1],
    StatusUpdate::ToolResult { preview, .. } if preview == "file.txt"
));
assert!(matches!(captured[2], StatusUpdate::ToolCompleted { success: true, .. }));
assert!(thread_event_to_app_events(&event, "thread-1").is_empty());
```

Add a second case with `result_preview: None` and expect only `ToolStarted -> ToolCompleted`.

- [ ] **Step 2: Run the test and verify RED**

```bash
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests::action_result_reaches_gateway_and_other_channels_via_status -- --exact --test-threads=6
```

Expected: the assertion fails because only two statuses are emitted.

- [ ] **Step 3: Add ToolResult to the status translator**

Destructure `result_preview` in `forward_event_to_channel` and insert between `ToolStarted` and `ToolCompleted`:

```rust
if let Some(preview) = result_preview.as_deref().filter(|value| !value.is_empty()) {
    let _ = channels
        .send_status(
            channel_name,
            StatusUpdate::ToolResult {
                name: display_name.clone(),
                preview: preview.to_string(),
            },
            metadata,
        )
        .await;
}
```

Do not add action mappings to `thread_event_to_app_events`; gateway statuses already become SSE events.

- [ ] **Step 4: Verify and commit live delivery**

```bash
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests::action_result_reaches_gateway_and_other_channels_via_status -- --exact --test-threads=6
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests::response_delta_reaches_gateway_and_other_channels_via_status -- --exact --test-threads=6
taskset -c 0-5 cargo fmt --all
git add src/bridge/router.rs
git commit -m "fix(engine): deliver live tool result previews"
```

Expected: both current Phase 5 and new tool status tests pass.

### Task 4: Reconstruct Tool History From The Saved Thread

**Files:**
- Modify: `ic/src/bridge/router.rs`

- [ ] **Step 1: Write failing durable-summary tests**

Create events containing a successful action, `ApprovalReceived`, and a failed action. Call the wished-for `tool_call_summaries` helper and assert call order, IDs, preview, and error:

```rust
let summaries = tool_call_summaries(&events);
assert_eq!(summaries.len(), 2);
assert_eq!(summaries[0]["call_id"], "call-before");
assert_eq!(summaries[0]["result_preview"], "output text");
assert_eq!(summaries[1]["call_id"], "call-after");
assert_eq!(summaries[1]["error"], "timeout");
```

Serialize the summaries between test `user` and `assistant` messages, pass them to `build_turns_from_db_messages`, and assert the same two tools render with the preview and error.

- [ ] **Step 2: Run the helper test and verify RED**

```bash
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests::saved_thread_events_reconstruct_tool_history_in_order -- --exact --test-threads=6
```

Expected: compilation fails because `tool_call_summaries` does not exist.

- [ ] **Step 3: Implement pure summary extraction**

Add near `drain_pending_thread_events`:

```rust
fn tool_call_summaries(events: &[lunarwing_engine::ThreadEvent]) -> Vec<serde_json::Value> {
    use crate::channels::web::util::truncate_preview;
    use lunarwing_engine::EventKind;

    let mut summaries = Vec::new();
    for event in events {
        match &event.kind {
            EventKind::ActionExecuted {
                action_name,
                call_id,
                result_preview,
                ..
            } => {
                let mut summary = serde_json::json!({"name": action_name});
                if !call_id.is_empty() {
                    summary["call_id"] = serde_json::Value::String(call_id.clone());
                }
                if let Some(preview) = result_preview.as_deref().filter(|value| !value.is_empty()) {
                    summary["result_preview"] =
                        serde_json::Value::String(truncate_preview(preview, 500));
                }
                summaries.push(summary);
            }
            EventKind::ActionFailed {
                action_name,
                call_id,
                error,
                ..
            } => {
                let mut summary = serde_json::json!({
                    "name": action_name,
                    "error": truncate_preview(error, 200),
                });
                if !call_id.is_empty() {
                    summary["call_id"] = serde_json::Value::String(call_id.clone());
                }
                summaries.push(summary);
            }
            _ => {}
        }
    }
    summaries
}
```

- [ ] **Step 4: Load summaries from the terminal saved thread**

Add:

```rust
async fn load_tool_calls_json(
    store: &Arc<dyn lunarwing_engine::Store>,
    thread_id: lunarwing_engine::ThreadId,
) -> Option<String> {
    let thread = match store.load_thread(thread_id).await {
        Ok(Some(thread)) => thread,
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(%thread_id, %error, "failed to load tool history");
            return None;
        }
    };
    let summaries = tool_call_summaries(&thread.events);
    if summaries.is_empty() {
        return None;
    }
    serde_json::to_string(&summaries)
        .map_err(|error| tracing::warn!(%thread_id, %error, "failed to serialize tool history"))
        .ok()
}
```

After `join_thread` and `record_thread_outcome`, load this JSON for terminal outcomes other than `GatePaused` and `Stopped`.

- [ ] **Step 5: Persist tool_calls immediately before assistant**

Capture `tool_calls_json` in `write_v1_response` and write:

```rust
if let Some(ref json) = tool_calls_json
    && let Err(error) = db.add_conversation_message(cid, "tool_calls", json).await
{
    tracing::warn!(%error, "failed to persist Engine V2 tool history");
}
if let Err(error) = db.add_conversation_message(cid, "assistant", &text).await {
    tracing::warn!(%error, "failed to persist Engine V2 assistant history");
}
```

Retain the existing non-empty terminal-text gate. Gate pauses and stopped turns therefore cannot leave partial tool rows.

- [ ] **Step 6: Record a directly approved action before resume**

In `execute_pending_gate_action`, subscribe before calling `execute_resolved_pending_action`. On success, record this event before `resume_thread`:

```rust
let event = lunarwing_engine::EventKind::ActionExecuted {
    step_id: exec_ctx.step_id,
    action_name: pending.action_name.clone(),
    call_id: pending.call_id.clone(),
    duration_ms: result.duration.as_millis() as u64,
    params_summary: lunarwing_engine::types::event::summarize_params(
        &pending.action_name,
        &pending.parameters,
    ),
    result_preview: lunarwing_engine::types::event::preview_from_output(&result.output),
};
state
    .thread_manager
    .record_waiting_event(pending.thread_id, &message.user_id, event)
    .await
    .map_err(|error| engine_err("record approved action", error))?;
```

Remove the old subscription created after execution. Keep approval context, injected result, call ID, and terminal delivery unchanged.

- [ ] **Step 7: Run router and manager regressions**

```bash
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests -- --test-threads=6
taskset -c 0-5 cargo test -j6 -p lunarwing_engine runtime::manager::tests -- --test-threads=6
```

Expected: all router, history, scoped-control, one-shot approval, and manager tests pass.

- [ ] **Step 8: Format and commit durable history wiring**

```bash
taskset -c 0-5 cargo fmt --all
git add src/bridge/router.rs
git commit -m "fix(engine): persist tool panels across reloads"
```

Expected: one router/history commit with no frontend changes.

### Task 5: Verify The Integrated Change

**Files:**
- Check: `ic/FEATURE_PARITY.md`
- Check: all files modified in Tasks 1-4

- [ ] **Step 1: Check whether parity documentation tracks this behavior**

```bash
rg -n -i 'tool card|tool panel|tool result|engine v2|gateway' FEATURE_PARITY.md
```

Expected: no parity row needs a status change. If one explicitly tracks persistent Engine V2 tool panels, update only that row and commit `docs: update engine tool panel parity`.

- [ ] **Step 2: Run formatting and whitespace gates**

```bash
taskset -c 0-5 cargo fmt --all -- --check
git diff --check
```

Expected: both exit zero without output.

- [ ] **Step 3: Run default and all-feature compile checks**

```bash
taskset -c 0-5 cargo check -j6
taskset -c 0-5 cargo check -j6 --all-features
```

Expected: both checks succeed.

- [ ] **Step 4: Run focused full library suites**

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine --lib -- --test-threads=6
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests -- --test-threads=6
taskset -c 0-5 cargo test -j6 --lib channels::web::util::tests -- --test-threads=6
```

Expected: all engine, router, and history reconstruction tests pass.

- [ ] **Step 5: Run default and all-feature Clippy**

```bash
taskset -c 0-5 cargo clippy -j6 --all --benches --tests --examples -- -D warnings
taskset -c 0-5 cargo clippy -j6 --all --benches --tests --examples --all-features -- -D warnings
```

Expected: both exit zero with warnings denied.

- [ ] **Step 6: Review final scope**

```bash
git status --short --branch
git diff HEAD~4..HEAD --stat
git log --oneline --decorate -8
```

Expected: only the approved spec/plan, engine event/manager files, and router tests changed. Gateway static files, tenant env/state, WASM artifacts, and lockfiles remain untouched.

### Task 6: Deploy And Validate On Brightdawn

**Preserve:**
- `/home/brightdawn/lunarwing/env/`
- `/home/brightdawn/lunarwing/state/`
- Installed WASM artifacts and tenant-specific generated lockfiles

- [ ] **Step 1: Push the verified branch**

```bash
git push origin integration/enginep4p5-uifix-skillsb2b3/v2.0.0.0
git status --short --branch
```

Expected: the clean local branch matches origin.

- [ ] **Step 2: Fast-forward Brightdawn's checkout**

```bash
sudo -n -u brightdawn git -C /home/brightdawn/lunarwing fetch origin integration/enginep4p5-uifix-skillsb2b3/v2.0.0.0
sudo -n -u brightdawn git -C /home/brightdawn/lunarwing switch integration/enginep4p5-uifix-skillsb2b3/v2.0.0.0
sudo -n -u brightdawn git -C /home/brightdawn/lunarwing merge --ff-only origin/integration/enginep4p5-uifix-skillsb2b3/v2.0.0.0
```

Expected: Brightdawn is at the exact pushed commit without a merge commit.

- [ ] **Step 3: Build only the release daemon in tmux**

```bash
tmux new-session -d -s toolpanel-brightdawn-build "cd /home/brightdawn/lunarwing/ic && taskset -c 0-5 cargo build --release -j6 --bin lunarwing 2>&1 | tee /tmp/toolpanel-brightdawn-build.log"
tmux set-option -t toolpanel-brightdawn-build remain-on-exit on
tmux capture-pane -p -t toolpanel-brightdawn-build -S -80
```

Expected: the release build exits zero. Do not run a debug build, `install-wasm`, an import/upgrade script, or any command that recreates tenant configuration or state.

- [ ] **Step 4: Restart and verify through mt-admin**

```bash
sudo -n ic/scripts/lunarwing-mt-admin.sh restart-tenant brightdawn
sudo -n ic/scripts/lunarwing-mt-admin.sh status brightdawn
```

Expected: daemon, gateway, PostgreSQL, and XMPP bridge are healthy. Do not change `ENGINE_V2_CHANNELS`.

- [ ] **Step 5: Run the live gateway tool-panel gate**

In a fresh gateway turn, request one deterministic read-only tool action and verify:

1. exactly one live tool card appears;
2. expanding it shows a non-empty bounded preview;
3. refresh preserves one tool summary;
4. switching away and back preserves the same summary;
5. the terminal response appears once;
6. the gateway event trace contains one ordered `tool_started -> tool_result -> tool_completed` sequence.

- [ ] **Step 6: Recheck one supervised approval turn**

Request the configured read-only SSH approval test, approve once, and verify one prompt, one execution, one live card, one persisted summary, one terminal response, and no replayed gate.

Update the Phase 5 checkpoint only if the user requests that documentation change after the live gate.
