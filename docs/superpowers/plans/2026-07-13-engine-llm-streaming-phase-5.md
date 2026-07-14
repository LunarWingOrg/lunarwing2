# Engine LLM Streaming Phase 5 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add opt-in Engine V2 final-response support for XMPP, DarkIRC, and
WeeChat through their existing channel contracts while preserving gateway-only
defaults, original routing metadata, control/gate scope, hooks, persistence,
and exactly-once delivery.

**Architecture:** Move Engine V2 channel selection into a pure, tested bridge
policy. Engine terminal text returns from `handle_with_engine()` to the agent's
existing outer outbound handler, which applies `BeforeOutbound`, suppresses
empty results, and calls `ChannelManager::respond()` once. Engine progress uses
`ChannelManager::send_status()` for every channel; gateway status metadata is a
clone of the incoming metadata extended with the engine thread ID, while WASM
channels continue ignoring `StatusUpdate::StreamChunk`.

**Tech Stack:** Rust 2024, existing `Agent`/`ChannelManager` contracts, gateway
SSE, WASM channel wrapper and current channel WIT, Engine V2 pending gates,
XMPP/DarkIRC/WeeChat routing metadata, TensorZero Gateway `2026.3.2`.

## Progress Checkpoint (2026-07-13)

**Status: paused after the XMPP pilot; implementation retained.** Brightdawn is
healthy on release commit `7e2573d` with `ENGINE_V2=true` and
`ENGINE_V2_CHANNELS=xmpp`. Gateway and XMPP use Engine V2. DarkIRC and WeeChat
remain absent from the allowlist and therefore continue through the legacy path.
Their live rollout is intentionally deferred until Phase 5 work resumes; this is
not an implementation rollback and does not broaden the enabled channel set.

The implementation and corrective commits now deployed from
`integration/enginep4p5-uifix-skillsb2b3/v2.0.0.0` are:

- `8044774` - channel-neutral Phase 5 routing, delivery, controls, and matrix;
- `12a6a74` - supervised one-shot approvals honor an already granted approval;
- `88a78fd` - approved action results resume as a structured tool result tied to
  the original call ID and internal inference transcript;
- `7e2573d` - resumed tool results preserve trusted gate-resolution context so
  the model knows the user explicitly approved the action.

Automated evidence after the approval fixes:

- `engine_v2_channel_delivery_matrix`: `1/1`, including a RED/GREEN regression
  for an identical post-approval tool replay and a second RED/GREEN regression
  for missing approval context in the resumed model turn;
- effect-adapter approval suite: `25/25`;
- bridge router suite: `30/30`;
- thread-manager suite: `13/13` after the internal-transcript resume change;
- default and all-feature compile checks, default and all-feature Clippy with
  warnings denied, formatting, and `git diff --check` passed under the six-thread
  constraint;
- the Brightdawn release build for `7e2573d` completed with
  `BUILD_EXIT_STATUS=0`, followed by a clean `mt-admin` restart and healthy
  gateway, daemon, XMPP bridge, PostgreSQL, and SSH-agent checks.

Live evidence collected before the pause:

- Gateway after XMPP opt-in: two stream chunks, one terminal response, and one
  persisted response (`/tmp/brightdawn_phase5_gateway_gate.result`).
- XMPP direct message: one OMEMO-tagged ingress, one XMPP-scoped Engine V2 turn,
  one terminal response, zero chunk messages, and one persisted response
  (`/tmp/brightdawn_phase5_xmpp_gate.result`).
- XMPP approval: one approval prompt was sent, one `yes` was received, one SSH
  execution sequence produced `PHASE5_XMPP_APPROVAL_TOOL_OK`, one terminal reply
  was sent, and no second gate remained. The first terminal reply incorrectly
  claimed that no gate fired even though the journal proved the pause and resume;
  `7e2573d` fixes that missing transcript context. The strengthened automated
  matrix passes, but the corrected final wording has not been rerun live.
- The pending-gate store was empty after the run and remained empty after the
  final deployment restart.

Remaining live work is deliberately deferred:

- XMPP room validation has no configured Brightdawn room; XMPP auth and scoped
  interrupt remain live-gate follow-ups even though their local matrix coverage
  passes.
- DarkIRC remains out of `ENGINE_V2_CHANNELS`; its live gate is pending.
- WeeChat remains out of `ENGINE_V2_CHANNELS`; its live gate is pending.
- The Phase 5 completion gate stays open. Resume from the deployed `7e2573d`
  checkpoint, optionally rerun the corrected XMPP approval wording, finish the
  remaining XMPP live cases, then enable DarkIRC and WeeChat one at a time only
  when the operator chooses to continue the rollout.

The detailed task checkboxes below remain the original execution procedure. This
checkpoint is the authoritative record of completed, deployed, and deferred work.

---

## Fixed Decisions And Boundaries

- `ENGINE_V2=false` disables Engine V2 for every channel, regardless of the
  allowlist.
- With `ENGINE_V2=true`, gateway remains enabled even when
  `ENGINE_V2_CHANNELS` is missing or empty.
- `ENGINE_V2_CHANNELS` adds only the eligible channels `xmpp`, `darkirc`, and
  `weechat`. Entries are comma-separated, trimmed, lowercased, and compared as
  exact names. Empty and unknown entries are ignored. Never use substring
  matching.
- A disabled or unknown channel continues through the legacy path.
- Final text, errors, interrupt acknowledgements, and resolved-gate results use
  the normal outer response handler exactly once. The bridge must not emit a
  direct terminal `AppEvent::Response`.
- A stopped original turn, a gate pause, and an auth pause are no-reply outcomes
  represented by `Some(String::new())`. `None` remains reserved for shutdown.
- `BeforeOutbound` modification/rejection and empty-response suppression remain
  owned by the outer agent loop. Do not duplicate them in the bridge.
- `ResponseDelta` is sent through `StatusUpdate::StreamChunk` once for gateway
  and once for the originating non-gateway channel. Remove its direct gateway
  `AppEvent` translation.
- WASM `send_status(StreamChunk)` remains a no-op. Do not send one chat message
  per token, modify `channel.wit`, add message editing, or reinstall channel
  artifacts merely to enable Phase 5.
- WASM terminal delivery uses the existing `respond()` implementation, which
  passes the original `IncomingMessage.metadata` to `on_respond`.
- Approval, authentication, interrupt, clear, and new-thread controls use the
  same user/conversation scope as ordinary Engine V2 input. Non-gateway gate
  replies must come from the source channel; the already trusted gateway retains
  `TRUSTED_GATE_CHANNELS` cross-channel approval authority. When no matching
  engine approval or active engine thread exists, legacy approval and interrupt
  handling remains available during rollout.
- XMPP, DarkIRC, and WeeChat are enabled and validated one at a time. Failure in
  one channel never justifies enabling it globally or weakening the gateway
  default.
- No database migration, provider change, TensorZero config change, WIT change,
  frontend change, automatic tenant upgrade, or automatic WASM install belongs
  in this phase.

## File Map

- Modify `ic/src/bridge/router.rs`: own the allowlist policy, matching helpers,
  channel-neutral status metadata, no-reply normalization, and removal of
  direct terminal/delta SSE delivery.
- Modify `ic/src/bridge/mod.rs` and `ic/src/bridge/kmod.rs`: export the tested
  routing and control-matching helpers needed by the agent.
- Modify `ic/src/agent/agent_loop.rs`: route eligible user/control submissions
  to Engine V2 and return its terminal result to the existing outbound handler.
- Modify `ic/src/channels/wasm/wrapper.rs`: tests only; preserve the intentional
  `StreamChunk` no-op and original-metadata response behavior.
- Check/modify test modules in `ic/channels-src/xmpp/src/lib.rs`,
  `ic/channels-src/darkirc/src/lib.rs`, and
  `ic/channels-src/weechat/src/lib.rs`: prove each existing response-routing
  metadata and conversation-thread contract without changing guest behavior.
- Modify `ic/tests/support/test_channel.rs`: capture one ordered status/response
  delivery log, including the incoming message used for each response, without
  changing existing response APIs.
- Modify `ic/tests/support/test_rig.rs`: allow a named channel and optional
  hooks so one real agent harness can test all delivery contracts.
- Create `ic/tests/engine_v2_channel_delivery.rs`: one serialized Engine V2
  routing/delivery matrix.
- Modify `ic/Cargo.toml`: register the integration test behind `libsql` and
  `integration` features.
- Modify `docs/proposals/ENGINE_LLM_STREAMING.md`: replace the Phase 3 direct-SSE
  description and mark Phase 5 status only after gates pass.
- Modify `docs/architecture/ENGINE-V2.md`: document the routing policy and
  channel-neutral delivery flow.
- Modify `docs/ops/TENANT-CONFIGURATION.md`: document safe per-tenant allowlist
  rollout and rollback.
- Modify `ic/FEATURE_PARITY.md`: distinguish gateway incremental streaming from
  opt-in non-gateway final-response delivery.
- Check `docs/releases/` for the eventual 2.0 release note; do not edit an old
  1.x release note.

### Task 1: Implement A Pure Channel Routing Policy

**Files:**
- Modify: `ic/src/bridge/router.rs`
- Modify: `ic/src/bridge/mod.rs`
- Modify: `ic/src/bridge/kmod.rs`

- [ ] **Step 1: Write the routing-policy table test**

Add a pure helper test that requires all approved cases without mutating the
process environment:

```rust
#[test]
fn engine_v2_channel_policy_is_gateway_default_and_exact_opt_in() {
    let cases = [
        (false, None, "gateway", false),
        (false, Some("xmpp"), "xmpp", false),
        (true, None, "gateway", true),
        (true, None, "xmpp", false),
        (true, Some(""), "gateway", true),
        (true, Some(""), "darkirc", false),
        (true, Some(" xmpp, DARKIRC ,weechat "), "xmpp", true),
        (true, Some(" xmpp, DARKIRC ,weechat "), "darkirc", true),
        (true, Some(" xmpp, DARKIRC ,weechat "), "weechat", true),
        (true, Some("notxmpp"), "xmpp", false),
        (true, Some("xmpp-extra"), "xmpp", false),
        (true, Some("telegram"), "telegram", false),
        (true, Some(",,xmpp,,"), "xmpp", true),
    ];

    for (engine_enabled, configured, channel, expected) in cases {
        assert_eq!(
            engine_v2_channel_allowed(engine_enabled, configured, channel),
            expected,
            "enabled={engine_enabled}, configured={configured:?}, channel={channel}",
        );
    }
}
```

- [ ] **Step 2: Run the test and confirm RED**

From `ic/`:

```bash
taskset -c 0-5 cargo test -j6 --lib \
  bridge::router::tests::engine_v2_channel_policy_is_gateway_default_and_exact_opt_in \
  -- --exact --nocapture
```

Expected: compilation fails because `engine_v2_channel_allowed` does not exist.

- [ ] **Step 3: Implement the pure policy and environment wrapper**

Add near `is_engine_v2_enabled()`:

```rust
const ENGINE_V2_OPT_IN_CHANNELS: [&str; 3] = ["xmpp", "darkirc", "weechat"];

fn engine_v2_channel_allowed(
    engine_enabled: bool,
    configured_channels: Option<&str>,
    channel: &str,
) -> bool {
    if !engine_enabled {
        return false;
    }

    let channel = channel.trim().to_ascii_lowercase();
    if channel == "gateway" {
        return true;
    }
    if !ENGINE_V2_OPT_IN_CHANNELS.contains(&channel.as_str()) {
        return false;
    }

    configured_channels.is_some_and(|configured| {
        configured
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .any(|entry| entry.eq_ignore_ascii_case(&channel))
    })
}

pub fn should_route_to_engine_v2(channel: &str) -> bool {
    let configured = std::env::var("ENGINE_V2_CHANNELS").ok();
    engine_v2_channel_allowed(
        is_engine_v2_enabled(),
        configured.as_deref(),
        channel,
    )
}
```

Export only `should_route_to_engine_v2`; keep the parameterized helper private
for deterministic tests.

- [ ] **Step 4: Run the policy test and commit**

```bash
taskset -c 0-5 cargo test -j6 --lib \
  bridge::router::tests::engine_v2_channel_policy_is_gateway_default_and_exact_opt_in \
  -- --exact --nocapture
git add ic/src/bridge/router.rs ic/src/bridge/mod.rs ic/src/bridge/kmod.rs
git commit -m "feat(engine): add channel opt-in policy"
```

### Task 2: Return Terminal Text Through The Normal Outbound Handler

**Files:**
- Modify: `ic/src/agent/agent_loop.rs`
- Modify: `ic/src/bridge/router.rs`

- [ ] **Step 1: Add a static regression test for direct terminal SSE**

Extract the outcome-to-response/persistence behavior enough for a unit test to
assert that a completed outcome returns its text but does not call an SSE
terminal helper. The behavioral assertion is:

```rust
assert_eq!(
    thread_outcome_response(&ThreadOutcome::Completed {
        response: Some("final".into()),
    }),
    Some("final".into()),
);
assert_eq!(
    thread_outcome_response(&ThreadOutcome::Stopped),
    Some(String::new()),
);
```

Also add a source guard in the plan's final verification for the forbidden
`AppEvent::Response` block in `await_thread_outcome`.

- [ ] **Step 2: Remove direct terminal SSE delivery**

Delete only the block in `await_thread_outcome()` that matches
`ThreadOutcome::Completed` and calls:

```rust
sse.broadcast_for_user(
    &message.user_id,
    AppEvent::Response { /* terminal text */ },
);
```

Do not remove mission notification SSE, structural engine thread events, gate
events, or `GatewayChannel::respond()`.

- [ ] **Step 3: Return the engine result directly from `handle_message`**

Replace the Phase 4 literal gateway branch with the new policy and remove all
result swallowing:

```rust
if crate::bridge::should_route_to_engine_v2(&message.channel)
    && let Submission::UserInput { ref content } = submission
{
    tracing::debug!(
        message_id = %message.id,
        user_id = %message.user_id,
        channel = %message.channel,
        "routing user input through engine v2"
    );
    return crate::bridge::handle_with_engine(self, message, content).await;
}
```

The outer `Agent::run()` match remains unchanged. It already executes
`BeforeOutbound`, suppresses empty strings, calls
`ChannelManager::respond(message, ...)`, and sends one error response.

- [ ] **Step 4: Prevent empty compatibility-history rows**

Retain the Phase 4 write guard:

```rust
if let Ok(Some(ref text)) = result
    && !text.is_empty()
    && let Some(ref db) = state.db
{
    write_v1_response(db, text).await;
}
```

- [ ] **Step 5: Run narrow tests and commit**

```bash
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests -- --nocapture
taskset -c 0-5 cargo test -j6 --lib agent::agent_loop::tests -- --nocapture
git diff --check
git add ic/src/agent/agent_loop.rs ic/src/bridge/router.rs
git commit -m "feat(engine): use normal channel terminal delivery"
```

### Task 3: Migrate Gateway Deltas To Channel-Neutral Status Exactly Once

**Files:**
- Modify: `ic/src/bridge/router.rs`

- [ ] **Step 1: Replace the Phase 3 mapping tests with Phase 5 expectations**

The existing `response_delta_maps_to_stream_chunk_app_event` test becomes:

```rust
#[test]
fn response_delta_has_no_direct_gateway_app_event() {
    let event = ThreadEvent::new(
        ThreadId::new(),
        EventKind::ResponseDelta {
            content: "hi".into(),
        },
    );
    assert!(thread_event_to_app_events(&event, "thread-1").is_empty());
}
```

Replace the gateway-skip status test with one requiring both gateway and XMPP
to receive one `StatusUpdate::StreamChunk("hi")` through `send_status()`.

Add a metadata test:

```rust
#[test]
fn gateway_status_metadata_extends_instead_of_replacing_original() {
    let message = IncomingMessage::new("gateway", "alice", "hello")
        .with_metadata(serde_json::json!({
            "user_id": "alice",
            "client_marker": "keep-me"
        }));

    let metadata = engine_status_metadata(&message, "engine-thread");
    assert_eq!(metadata["user_id"], "alice");
    assert_eq!(metadata["client_marker"], "keep-me");
    assert_eq!(metadata["thread_id"], "engine-thread");
}
```

- [ ] **Step 2: Run the tests and confirm RED**

```bash
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests::response_delta \
  -- --nocapture
taskset -c 0-5 cargo test -j6 --lib \
  bridge::router::tests::gateway_status_metadata_extends_instead_of_replacing_original \
  -- --exact --nocapture
```

Expected: the direct AppEvent test fails, gateway receives no status, and the
metadata helper is missing.

- [ ] **Step 3: Build delivery metadata once per thread event**

Add:

```rust
fn engine_status_metadata(message: &IncomingMessage, thread_id: &str) -> serde_json::Value {
    let mut metadata = message.metadata.clone();
    if message.channel != "gateway" {
        return metadata;
    }

    if !metadata.is_object() {
        metadata = serde_json::json!({});
    }
    if let Some(object) = metadata.as_object_mut() {
        object.insert("thread_id".into(), serde_json::json!(thread_id));
    }
    metadata
}
```

In `deliver_thread_event`, calculate it once and pass it to
`forward_event_to_channel`:

```rust
let status_metadata = engine_status_metadata(message, thread_id);
forward_event_to_channel(
    event,
    &agent.channels,
    &message.channel,
    &status_metadata,
)
.await;
```

- [ ] **Step 4: Remove the gateway guard and direct delta arm**

Change the status match to:

```rust
EventKind::ResponseDelta { content } => {
    let _ = channels
        .send_status(
            channel_name,
            StatusUpdate::StreamChunk(content.clone()),
            metadata,
        )
        .await;
}
```

Delete `EventKind::ResponseDelta` from `thread_event_to_app_events`.

For event kinds already mapped to `StatusUpdate` (`StepStarted`, action
started/completed/failed, step completion, interpreted message status, and
skill activation), remove their equivalent direct gateway mappings too. Keep
only structural gateway events that have no channel-neutral status equivalent,
such as `ThreadStateChanged` and `ChildThreadSpawned`.

- [ ] **Step 5: Run delivery tests and commit**

```bash
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests::response_delta \
  -- --nocapture
taskset -c 0-5 cargo test -j6 --lib \
  bridge::router::tests::gateway_status_metadata_extends_instead_of_replacing_original \
  -- --exact --nocapture
git add ic/src/bridge/router.rs
git commit -m "refactor(engine): deliver progress through channel status"
```

### Task 4: Normalize Gate And Auth Pauses As No-Reply Outcomes

**Files:**
- Modify: `ic/src/bridge/router.rs`
- Modify: `ic/src/channels/wasm/wrapper.rs` (tests only)

- [ ] **Step 1: Add duplicate-prompt regression tests**

Add tests proving all of these return the empty sentinel after their status was
sent:

- pre-execution `insert_and_notify_pending_gate` approval;
- `ThreadOutcome::GatePaused` approval;
- `ThreadOutcome::GatePaused` authentication;
- text-based authentication fallback;
- `ThreadOutcome::Stopped` from Phase 4.

The common assertion is:

```rust
assert_eq!(result, Some(String::new()));
```

Also assert the captured channel statuses contain exactly one
`ApprovalNeeded` or `AuthRequired` as appropriate and the compatibility DB has
no empty assistant row.

- [ ] **Step 2: Run the tests and confirm RED**

```bash
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests::gate_pause \
  -- --nocapture
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests::auth_pause \
  -- --nocapture
```

Expected: current helpers return human-readable text, which would become a
second WASM `on_respond` message after terminal delivery is normalized.

- [ ] **Step 3: Return empty after status-driven pauses**

Keep the human-readable prompt construction inside the channel status path,
especially the WASM `ApprovalNeeded` handling. Change bridge return values after
successful status dispatch to:

```rust
Ok(Some(String::new()))
```

For external gates, send one best-effort `StatusUpdate::Status` before returning
empty so non-gateway users are not left without a waiting indication.

Delete the two direct `AppEvent::AuthRequired` broadcasts in
`await_thread_outcome`; `GatewayChannel::send_status(AuthRequired)` is now the
single gateway mapping. Keep direct `GateRequired`/`GateResolved` events because
they are structural engine-gate events with no equivalent `StatusUpdate`.

- [ ] **Step 4: Strengthen the WASM no-token test**

Keep `StatusUpdate::StreamChunk(_) => {}` unchanged. Extend
`test_stream_chunk_is_noop` so it establishes a pending synchronous response
waiter before sending three chunks and proves no waiter is consumed, no typing
task starts, and a later `respond()` completes once.

Add a source-level assertion around `WasmChannel::respond` that the serialized
metadata comes from `msg.metadata`, not `response.metadata`.

- [ ] **Step 5: Run and commit**

```bash
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests::gate_pause \
  -- --nocapture
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests::auth_pause \
  -- --nocapture
taskset -c 0-5 cargo test -j6 --lib \
  channels::wasm::wrapper::tests::test_stream_chunk_is_noop \
  -- --exact --nocapture
git add ic/src/bridge/router.rs ic/src/channels/wasm/wrapper.rs
git commit -m "fix(engine): suppress duplicate gate and auth replies"
```

### Task 5: Route Approval, Authentication, Interrupt, Clear, And New Thread Safely

**Files:**
- Modify: `ic/src/bridge/router.rs`
- Modify: `ic/src/bridge/mod.rs`
- Modify: `ic/src/bridge/kmod.rs`
- Modify: `ic/src/agent/agent_loop.rs`

- [ ] **Step 1: Add one read-only conversation lookup for control routing**

Use the existing scoped key and `ConversationManager::list_conversations()` so
matching never creates state:

```rust
async fn find_engine_conversation_for_message(
    state: &EngineState,
    message: &IncomingMessage,
) -> Option<lunarwing_engine::ConversationSurface> {
    let key = engine_conversation_key(message);
    state
        .conversation_manager
        .list_conversations(&message.user_id)
        .await
        .into_iter()
        .find(|conversation| conversation.channel == key)
}
```

This lookup is required for DarkIRC and WeeChat because their conversation
scopes (`darkirc:dm:...`, `weechat:dm:...`, and `weechat:group:...`) are not
UUIDs. Do not reuse `parse_scope_uuid()` as the primary control matcher.

- [ ] **Step 2: Add a conversation-scoped pending-gate matcher**

Add a private matcher that filters `pending_gates.list_for_user()` by the
conversation ID returned above, expected `ResumeKind`, optional request ID, and
the existing channel authorization rule:

```rust
fn gate_channel_matches(pending: &PendingGate, responding_channel: &str) -> bool {
    pending.source_channel == responding_channel
        || crate::gate::store::TRUSTED_GATE_CHANNELS.contains(&responding_channel)
}
```

Return `None` for zero or multiple matches. An ambiguous result must fall back or
produce the existing explicit ambiguity response; it must never select the most
recent gate from another conversation.

- [ ] **Step 3: Add a read-only approval matcher**

Implement and export:

```rust
pub async fn has_matching_engine_approval(
    message: &IncomingMessage,
    request_id: Option<uuid::Uuid>,
) -> bool
```

It must return false without initializing or mutating engine state. When state
exists, use the conversation-scoped matcher, require `ResumeKind::Approval`, and
require the explicit request ID when supplied. Ambiguous or missing matches
return false so the legacy approval path remains reachable. Source-channel
matching applies to WASM replies; the trusted gateway exception above remains
available for an explicit scoped approval.

The trusted-gateway case is intentionally separate from the channel-key lookup:
when `message.channel` is trusted, `request_id` is present, and
`message.conversation_scope()` parses as an engine `ThreadId`, peek the
`PendingGateKey { user_id, thread_id }` and require the same request ID plus
`ResumeKind::Approval`. A gateway message must not fabricate an XMPP channel key,
and a trusted but unscoped/simple `yes` must not select a cross-channel gate.

Refactor `handle_approval()` and `handle_exec_approval()` to use the same matcher
instead of `approval_thread_scope_hint()` or a user-wide request-ID fallback.
Resolution still goes through `PendingGateStore::take_verified()` so the final
request-ID, channel, and expiry checks remain atomic.

- [ ] **Step 4: Add a read-only active-thread matcher**

Implement and export:

```rust
pub async fn has_active_engine_thread(message: &IncomingMessage) -> bool
```

It must use `find_engine_conversation_for_message()`, inspect its active-thread
list, and call `ThreadManager::is_running()` without creating a conversation.
Return true only if at least one thread in that exact user/channel/scope is
currently running.

- [ ] **Step 5: Write control-routing scope tests**

Add tests with two scopes for the same user and channel:

1. Simple `yes` in XMPP room A matches only room A's pending gate.
2. A request ID from room A supplied in room B does not match.
3. DarkIRC `darkirc:dm:alice` and `darkirc:dm:bob` do not collide even when
   both messages execute under the owner scope.
4. WeeChat `weechat:dm:network:nick` and
   `weechat:group:network:#room` do not collide.
5. Authentication gates are not classified as approvals.
6. An interrupt in one scope reports an active engine thread only for that
   scope.
7. A missing engine match returns false and leaves the corresponding legacy
   route available.
8. A trusted gateway approval with the exact request ID and thread scope may
   resolve an XMPP-originated gate; an unrelated gateway scope may not.

- [ ] **Step 6: Route matching controls before legacy session resolution**

Inside the `should_route_to_engine_v2` block, use a borrowed match so the
submission remains available when no engine match exists:

```rust
match &submission {
    Submission::UserInput { content } => {
        return crate::bridge::handle_with_engine(self, message, content).await;
    }
    Submission::ExecApproval {
        request_id,
        approved,
        always,
    } if crate::bridge::has_matching_engine_approval(message, Some(*request_id)).await => {
        return crate::bridge::handle_exec_approval(
            self,
            message,
            *request_id,
            *approved,
            *always,
        )
        .await;
    }
    Submission::ApprovalResponse { approved, always }
        if crate::bridge::has_matching_engine_approval(message, None).await =>
    {
        return crate::bridge::handle_approval(self, message, *approved, *always).await;
    }
    Submission::Interrupt
        if crate::bridge::has_active_engine_thread(message).await =>
    {
        return crate::bridge::handle_interrupt(self, message).await;
    }
    Submission::Clear => {
        return crate::bridge::handle_clear(self, message).await;
    }
    Submission::NewThread => {
        return crate::bridge::handle_new_thread(self, message).await;
    }
    _ => {}
}
```

Authentication tokens need no separate arm: while an auth gate is pending they
parse as `UserInput`, and `handle_with_engine_inner` resolves the gate before
safety scanning, normal history, logging, or dual-write.

- [ ] **Step 7: Make authentication resolution conversation-scoped**

Replace both `resolve_pending_gate_for_user(..., thread_scope)` calls at the top
of `handle_with_engine_inner()` with the conversation-scoped pending-gate
matcher. Require `ResumeKind::Authentication` and the same source/trusted-channel
authorization rule before treating `UserInput` as a credential. This keeps
non-UUID DarkIRC/WeeChat scopes isolated and preserves the existing ordering:
credential resolution occurs before safety scanning, user-history writes, and
normal engine submission.

- [ ] **Step 8: Add an auth-history regression test**

Insert authentication gates for two non-UUID scopes owned by the same user,
submit a unique sentinel token from one scope as `UserInput`, and require only
that scope to resume. Complete the scripted turn and inspect both engine messages
and v1 compatibility history. Assert neither contains the sentinel. Do not print
the sentinel in test failure messages.

- [ ] **Step 9: Run and commit control routing**

```bash
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests::engine_approval \
  -- --nocapture
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests::engine_interrupt \
  -- --nocapture
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests::engine_auth_token \
  -- --nocapture
taskset -c 0-5 cargo test -j6 --lib agent::agent_loop::tests -- --nocapture
git add ic/src/bridge/router.rs ic/src/bridge/mod.rs ic/src/bridge/kmod.rs \
  ic/src/agent/agent_loop.rs
git commit -m "feat(engine): route scoped channel controls"
```

### Task 6: Add A Real Agent Delivery Matrix

**Files:**
- Modify: `ic/tests/support/test_channel.rs`
- Modify: `ic/tests/support/test_rig.rs`
- Create: `ic/tests/engine_v2_channel_delivery.rs`
- Modify: `ic/Cargo.toml`

- [ ] **Step 1: Extend test support without changing existing callers**

Add an ordered capture type and log to `TestChannel`:

```rust
#[derive(Debug, Clone)]
pub enum CapturedDelivery {
    Status(StatusUpdate),
    Response {
        message: IncomingMessage,
        response: OutgoingResponse,
    },
}
```

Store `Arc<Mutex<Vec<CapturedDelivery>>>`. In `respond()`, append `Response`
before the existing response push; in `send_status()`, append `Status` before the
existing status push. Add snapshot accessors on `TestChannel` and `TestRig`, and
clear the ordered log in the existing `clear()` method. This single log is what
proves a `StreamChunk` preceded the terminal response.

Add these `TestRigBuilder` fields and methods:

```rust
channel_name: String,
extra_hooks: Vec<Arc<dyn lunarwing::hooks::Hook>>,

pub fn with_channel_name(mut self, channel_name: impl Into<String>) -> Self {
    self.channel_name = channel_name.into();
    self
}

pub fn with_hook(mut self, hook: Arc<dyn lunarwing::hooks::Hook>) -> Self {
    self.extra_hooks.push(hook);
    self
}
```

Default `channel_name` to `"test"`. Before building `AgentDeps`, register every
extra hook with `components.hooks.register(hook).await`. Construct
`TestChannel::new().with_name(channel_name)`.

Also expose the existing libSQL database handle from `TestRig` under
`#[cfg(feature = "libsql")]` so the matrix can resolve the scoped v1
conversation and count assistant rows:

```rust
pub fn database(&self) -> &Arc<dyn Database> {
    &self.db
}
```

- [ ] **Step 2: Register one integration target**

Add to `ic/Cargo.toml`:

```toml
[[test]]
name = "engine_v2_channel_delivery"
required-features = ["libsql", "integration"]
```

- [ ] **Step 3: Write one serialized matrix test**

Use a single `#[tokio::test]` in the new file so process-global Engine V2 state
and environment values are never mutated concurrently. For each of `gateway`,
`xmpp`, `darkirc`, and `weechat`:

1. Set `ENGINE_V2=true` and the exact allowlist needed for that case.
2. Build a named `TestRig` with a deterministic text-stream LLM.
3. Send an `IncomingMessage` with realistic scope and channel metadata: UUIDs
   for gateway/XMPP, `darkirc:dm:alice` for DarkIRC, and
   `weechat:group:libera:#lunarwing` for the WeeChat group case.
4. Wait for exactly one terminal response.
5. Assert the delivery's input metadata equals the original metadata.
6. Assert the response text is exact and there is exactly one assistant history
   row.
7. Assert one ordered `StreamChunk` status occurred before the response using
   `CapturedDelivery`, not two unrelated capture vectors.
8. Drop/shutdown the rig and call `lunarwing::bridge::reset_engine_state().await`
   before the next case.

Count compatibility-history rows through the real DB trait:

```rust
let conversation_id = rig
    .database()
    .get_or_create_scoped_conversation(channel, user_id, scope)
    .await
    .expect("scoped conversation should resolve");
let messages = rig
    .database()
    .list_conversation_messages(conversation_id)
    .await
    .expect("history should load");
assert_eq!(
    messages.iter().filter(|message| message.role == "assistant").count(),
    1,
);
```

Use these metadata shapes:

```rust
serde_json::json!({"thread_id": scope, "user_id": "test-user"})
serde_json::json!({"xmpp_target": "room@conference.example.org", "xmpp_room": "room@conference.example.org", "xmpp_type": "groupchat"})
serde_json::json!({"nick": "alice"})
serde_json::json!({"buffer": "irc.libera.#lunarwing", "network": "libera", "target": "#lunarwing", "nick": "alice", "is_dm": false})
```

Guard unsafe Rust 2024 environment changes with a small RAII restore type and a
comment explaining that this integration binary runs one test only.

- [ ] **Step 4: Add legacy fallback cases**

Within the same test function, prove an unset allowlist leaves XMPP on the
legacy path and an unknown channel remains legacy even when named in
`ENGINE_V2_CHANNELS`. Assert that the legacy response arrives and that no new
Engine V2 thread/conversation is created for that scoped message; do not rely
only on response text because both engines share the configured host provider.

- [ ] **Step 5: Add outbound hook modification and suppression cases**

Define a test hook for `HookPoint::BeforeOutbound`. One case returns
`HookOutcome::modify("modified-by-hook".into())` and requires that exact channel
response. A second returns `HookOutcome::reject("blocked")` and requires zero
channel responses after the engine completes.

- [ ] **Step 6: Add error and no-reply cases**

Require:

- one provider/engine error produces one `Error:` channel response;
- a stopped original turn produces no response from that message task;
- a gate pause emits one status-driven prompt and no terminal duplicate;
- the subsequent approval reply emits only the resumed terminal response;
- three stream chunks on a WASM-named test channel do not become three terminal
  responses.

- [ ] **Step 7: Run the integration target**

```bash
taskset -c 0-5 cargo test -j6 --no-default-features \
  --features libsql,integration --test engine_v2_channel_delivery \
  -- --test-threads=1 --nocapture
```

Expected: gateway plus all three eligible channels pass exactly-once response,
metadata, hook, persistence, error, no-reply, and fallback assertions.

- [ ] **Step 8: Commit the integration matrix**

```bash
git add ic/tests/support/test_channel.rs ic/tests/support/test_rig.rs \
  ic/tests/engine_v2_channel_delivery.rs ic/Cargo.toml
git commit -m "test(engine): cover channel-neutral delivery matrix"
```

### Task 7: Verify The Actual WASM Channel Boundaries

**Files:**
- Modify tests only if coverage is missing:
  `ic/channels-src/xmpp/src/lib.rs`,
  `ic/channels-src/darkirc/src/lib.rs`,
  `ic/channels-src/weechat/src/lib.rs`
- Verify unchanged: `ic/wit/channel.wit`,
  `ic/channels-src/weechat/channel.wit`

- [ ] **Step 1: Run host wrapper regressions**

```bash
taskset -c 0-5 cargo test -j6 --lib \
  channels::wasm::wrapper::tests::test_stream_chunk_is_noop \
  -- --exact --nocapture
taskset -c 0-5 cargo test -j6 --lib \
  channels::wasm::wrapper::tests::test_respond_cancels_typing_task \
  -- --exact --nocapture
taskset -c 0-5 cargo test -j6 --lib \
  channels::wasm::wrapper::tests::test_dispatch_emitted_messages_owner_binding_sets_owner_scope \
  -- --exact --nocapture
taskset -c 0-5 cargo test -j6 --lib \
  channels::wasm::wrapper::tests::test_dispatch_emitted_messages_guest_sender_stays_isolated \
  -- --exact --nocapture
```

- [ ] **Step 2: Run each channel crate's routing tests from `ic/`**

```bash
taskset -c 0-5 cargo test -j6 --manifest-path channels-src/xmpp/Cargo.toml
taskset -c 0-5 cargo test -j6 --manifest-path channels-src/darkirc/Cargo.toml
taskset -c 0-5 cargo test -j6 --manifest-path channels-src/weechat/Cargo.toml
```

These tests must cover XMPP target/room metadata, DarkIRC nick/thread identity,
and WeeChat buffer/network/target identity. Add narrowly named tests in the
channel crate if an assertion is missing; do not change protocol behavior.

- [ ] **Step 3: Prove no ABI change**

From the repository root:

```bash
git diff --exit-code -- ic/wit/channel.wit ic/channels-src/*/channel.wit
git diff --check -- ic/channels-src/xmpp ic/channels-src/darkirc \
  ic/channels-src/weechat
git diff -- ic/channels-src/xmpp ic/channels-src/darkirc \
  ic/channels-src/weechat
```

The final command may show test-only edits made in Step 2; review them and
require no exported WIT signature or production `on_respond` behavior change.
`git diff --check` must pass; the informational diff is not an exit-code gate.

- [ ] **Step 4: Commit any missing channel-contract tests**

If Step 2 required new tests, stage only the three channel `src/lib.rs` files and
commit them. If existing coverage already proves all three contracts, record
that evidence in the Phase 5 status section and make no empty commit.

```bash
git add ic/channels-src/xmpp/src/lib.rs \
  ic/channels-src/darkirc/src/lib.rs \
  ic/channels-src/weechat/src/lib.rs
git commit -m "test(channels): lock engine response routing metadata"
```

### Task 8: Documentation And Full Local Verification

**Files:**
- Modify: `docs/proposals/ENGINE_LLM_STREAMING.md`
- Modify: `docs/architecture/ENGINE-V2.md`
- Modify: `docs/ops/TENANT-CONFIGURATION.md`
- Modify: `ic/FEATURE_PARITY.md`

- [ ] **Step 1: Document final policy and actual status**

Document:

- gateway-only default and exact allowlist syntax;
- eligible channel names;
- final-response versus incremental-streaming distinction;
- approval/auth/interrupt scope behavior;
- empty-sentinel no-reply behavior;
- rollback by removing an allowlist entry;
- TensorZero `2026.3.2` remains the provider compatibility target;
- no WIT migration or WASM reinstall is required.

Mark Phase 5 complete only after automated gates and every currently configured
live channel gate pass. If DarkIRC or WeeChat is not configured, state
"implementation ready, live validation pending" rather than claiming it was
tested.

- [ ] **Step 2: Run formatting, compile, lint, and tests**

From `ic/`:

```bash
taskset -c 0-5 cargo fmt --all -- --check
taskset -c 0-5 cargo check -j6
taskset -c 0-5 cargo check -j6 --no-default-features --features postgres
taskset -c 0-5 cargo check -j6 --no-default-features --features libsql
taskset -c 0-5 cargo test -j6 -p lunarwing_engine -- --test-threads=6
taskset -c 0-5 cargo test -j6 --lib bridge::router::tests -- --test-threads=6
taskset -c 0-5 cargo test -j6 --lib channels::wasm::wrapper::tests \
  -- --test-threads=6
taskset -c 0-5 cargo test -j6 --no-default-features \
  --features libsql,integration --test engine_v2_channel_delivery \
  -- --test-threads=1
taskset -c 0-5 cargo clippy -j6 --all --benches --tests --examples -- -D warnings
taskset -c 0-5 cargo clippy -j6 --all --benches --tests --examples \
  --all-features -- -D warnings
```

From the repository root:

```bash
git diff --check
rg -n "systemctl is-active|systemctl status|rc-service" \
  lunarwing_mt_onboard lunarwing_mt_onboard_web
rg -n "AppEvent::Response|ResponseDelta|StreamChunk|ENGINE_V2_CHANNELS" \
  ic/src/bridge/router.rs ic/src/agent/agent_loop.rs
```

Review the last search manually. `AppEvent::Response` must not occur in
`await_thread_outcome`; `ResponseDelta` must have one channel-status route;
`ENGINE_V2_CHANNELS` must be read only by the bridge policy.

- [ ] **Step 3: Commit documentation and verification evidence**

```bash
git add docs/proposals/ENGINE_LLM_STREAMING.md \
  docs/architecture/ENGINE-V2.md docs/ops/TENANT-CONFIGURATION.md \
  ic/FEATURE_PARITY.md
git commit -m "docs: document engine channel rollout"
```

### Task 9: Stage Brightdawn Without Replacing Env, State, Or WASM

**Files:**
- Preserve: `/home/brightdawn/lunarwing/env/`
- Preserve: `/home/brightdawn/lunarwing/state/`
- No automatic WASM reinstall.

- [x] **Step 1: Push before deployment**

```bash
git status --short --branch
git push origin integration/enginep4p5-uifix-skillsb2b3/v2.0.0.0
```

Require a clean worktree and exact local/origin HEAD equality.

- [x] **Step 2: Update source and build only the release binary**

```bash
sudo -n -u brightdawn git -C /home/brightdawn/lunarwing fetch origin \
  integration/enginep4p5-uifix-skillsb2b3/v2.0.0.0
sudo -n -u brightdawn git -C /home/brightdawn/lunarwing switch \
  integration/enginep4p5-uifix-skillsb2b3/v2.0.0.0
sudo -n -u brightdawn git -C /home/brightdawn/lunarwing merge --ff-only \
  origin/integration/enginep4p5-uifix-skillsb2b3/v2.0.0.0
tmux new-session -d -s phase5-brightdawn-build \
  "cd /home/brightdawn/lunarwing/ic && taskset -c 0-5 cargo build --release -j6 --bin lunarwing 2>&1 | tee /tmp/phase5-brightdawn-build.log"
```

Do not run `install-wasm`, an import/upgrade script, `git clean`, or any command
that recreates `env/` or `state/`.

- [ ] **Step 3: Prove the default remains gateway-only before editing env**

Restart through the lifecycle owner and test gateway plus XMPP with the existing
unset/empty allowlist. Gateway must use Engine V2; XMPP must remain legacy.

```bash
sudo -n ic/scripts/lunarwing-mt-admin.sh restart-tenant brightdawn
sudo -n ic/scripts/lunarwing-mt-admin.sh status brightdawn
```

- [x] **Step 4: Enable only XMPP with a targeted env-key update**

Back up the env file without reading it into logs, then update or append only
`ENGINE_V2_CHANNELS`:

```bash
backup="/home/brightdawn/lunarwing/env/lunarwing.env.pre-phase5.$(date +%Y%m%dT%H%M%S)"
sudo -n -u brightdawn cp -a \
  /home/brightdawn/lunarwing/env/lunarwing.env \
  "$backup"
printf 'Phase 5 env backup: %s\n' "$backup"
sudo -n sh -c 'envf=/home/brightdawn/lunarwing/env/lunarwing.env; if grep -q "^ENGINE_V2_CHANNELS=" "$envf"; then sed -i "s/^ENGINE_V2_CHANNELS=.*/ENGINE_V2_CHANNELS=xmpp/" "$envf"; else printf "\nENGINE_V2_CHANNELS=xmpp\n" >> "$envf"; fi; chown brightdawn:brightdawn "$envf"; chmod 600 "$envf"'
sudo -n ic/scripts/lunarwing-mt-admin.sh restart-tenant brightdawn
sudo -n ic/scripts/lunarwing-mt-admin.sh status brightdawn
```

Record the unique backup path. Do not print the full env file. The edit may
change only `ENGINE_V2_CHANNELS`; compare key presence/value without displaying
unrelated secret-bearing lines.

- [ ] **Step 5: Pass the XMPP gate (pilot partially complete)**

Direct-message delivery and one-shot approval passed live. The corrected
approval-context wording is deployed and covered locally but has not been rerun
live. Room validation is blocked by zero configured rooms; live auth and scoped
interrupt checks are deferred with the rest of Phase 5.

Test a direct chat and a room independently. For each, require:

- one complete Engine V2 reply reaches the original JID/room;
- no individual provider chunk becomes an XMPP stanza;
- OMEMO direct-message encryption/decryption still succeeds;
- an approval prompt appears once, `yes` resolves only that scope, and the
  final result appears once;
- an auth token is not echoed or stored in chat history;
- interrupt stops only the active scope and emits no cancelled terminal reply;
- a later message in the same scope completes normally;
- gateway streaming still emits ordered chunks plus one terminal response.

Rollback XMPP immediately by setting `ENGINE_V2_CHANNELS=` and restarting if
any item fails.

- [ ] **Step 6: Enable and test DarkIRC only when configured**

**Deferred by operator on 2026-07-13.** Keep `darkirc` absent from
`ENGINE_V2_CHANNELS`; local routing and scope coverage remain in place for the
future live gate.

After XMPP passes, set `ENGINE_V2_CHANNELS=xmpp,darkirc`, restart, and require
one final response to the original nick, no token messages, nick-scope approval
and interrupt isolation, and no cross-nick delivery. If Brightdawn's DarkIRC
service is not configured, leave `darkirc` out of the live allowlist and record
the live gate as pending; automated tests must still pass.

- [ ] **Step 7: Enable and test WeeChat only when configured**

**Deferred by operator on 2026-07-13.** Keep `weechat` absent from
`ENGINE_V2_CHANNELS`; local routing and scope coverage remain in place for the
future live gate.

After DarkIRC passes or is explicitly deferred, add `weechat` only on a tenant
where its relay/adapter/channel are healthy. Require one final response to the
original buffer/target, no token messages, DM/group isolation, scoped approval,
scoped interrupt, and no delivery to another buffer. If Brightdawn's WeeChat
path is not configured, leave it out and record the live gate as pending.

- [ ] **Step 8: Record evidence and final rollback procedure**

Evidence available at the pause is summarized in the progress checkpoint above.
Final per-channel evidence remains open until the deferred live gates run.

For each enabled channel record sanitized timestamps, channel/scope labels,
status/chunk counts, terminal-response count, approval/auth/interrupt results,
history count, and post-turn recovery. Never record tokens, passwords, bearer
headers, OMEMO key material, or complete secret-bearing env values.

Fast rollback is a targeted allowlist removal followed by:

```bash
sudo -n ic/scripts/lunarwing-mt-admin.sh restart-tenant brightdawn
sudo -n ic/scripts/lunarwing-mt-admin.sh status brightdawn
```

Removing all entries restores gateway-only Engine V2. Setting `ENGINE_V2=false`
is the broader rollback and returns every channel to the legacy engine. Neither
rollback requires a database migration, state restoration, or WASM reinstall.
If Phase 5 introduced the key into an env file where it was previously absent,
restore the recorded timestamped backup instead of leaving configuration churn;
never replace `state/` or regenerate the rest of `env/`.

## Phase 5 Completion Gate

Phase 5 implementation is complete only when every applicable item is true:

**Current decision:** do not mark this gate complete. XMPP is the only live
opt-in pilot; DarkIRC and WeeChat are intentionally deferred and remain on the
legacy path.

- [ ] `ENGINE_V2=false` disables every channel.
- [ ] Unset/empty allowlist preserves gateway-only Engine V2.
- [ ] Only exact eligible entries enable XMPP, DarkIRC, and WeeChat.
- [ ] Disabled and unknown channels stay legacy.
- [ ] Every completed Engine V2 turn uses `ChannelManager::respond()` once with
  the original routing metadata.
- [ ] Gateway receives each delta once through channel status and each terminal
  response once through `GatewayChannel::respond()`.
- [ ] WASM channels ignore every `StreamChunk` and receive one final response.
- [ ] Gate/auth/stopped no-reply outcomes do not create duplicate messages or
  empty assistant history rows.
- [ ] `BeforeOutbound` modification/rejection and engine error handling retain
  their existing behavior.
- [ ] XMPP room/JID, DarkIRC nick, and WeeChat buffer/target scopes do not
  collide for conversation, approval, authentication, or interrupt handling.
- [ ] No WIT, DB schema, TensorZero provider, frontend, or automatic WASM
  installation change was introduced.
- [ ] Local default-feature, postgres-only, libsql-only, integration, Clippy,
  formatting, and channel-crate gates pass under the six-thread constraint.
- [ ] Each live-configured channel passes its staged Brightdawn gate; any
  unconfigured channel is explicitly documented as live validation pending and
  remains absent from the tenant allowlist.
