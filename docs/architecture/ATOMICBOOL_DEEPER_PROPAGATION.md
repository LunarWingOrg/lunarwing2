# AtomicBool Deeper Propagation - Future Work

**Date:** 2026-05-19  
**Status:** Design Note - Not Implemented (revalidated 2026-10-01, v2.1.4.0)
**Related:** [RESPONSE_SUPPRESSION_IMPLEMENTATION.md](../internal/history/architecture/RESPONSE_SUPPRESSION_IMPLEMENTATION.md), [HANDLE_MESSAGE_FIX.md](../internal/history/architecture/HANDLE_MESSAGE_FIX.md)

## Current State (revalidated 2026-10-01, v2.1.4.0)

`agent_loop.rs::run()` creates one `Arc<AtomicBool>` for the active inbound
message and passes it to `handle_message()`. On the legacy ordinary-user-input
path, `handle_message()` passes a reference into `process_user_input()` and into
subsequent turns drained from that same message queue.

The flag currently guards only selected legacy-path behavior:

- the initial `Thinking("Processing...")` status immediately before the legacy
  agentic loop;
- the normal-result `Interrupted`, `Done`, `Suggestions`, `TurnCost`, and
  `ApprovalNeeded` statuses in `process_user_input()`;
- the drain loop, which discards already-drained and remaining queued text after
  suppression instead of starting another turn.

The outer message loop also stops waiting at the soft timeout, sends the timeout
reply itself, and never delivers the eventual top-level return value. That is why
the final conversational response is suppressed even if the inner task finishes
before the hard abort.

**The flag boundary still stops at `process_user_input()` and is incomplete even
inside that function.** Current unguarded paths include:

- the auto-compaction status sent before the legacy loop;
- `run_agentic_loop()` and `ChatDelegate`, including per-iteration thinking,
  tool reasoning/activity/result, authentication, and generated-image statuses;
- `process_approval()` and its resumed-loop completion helpers;
- the Engine V2 router/event-delivery path, which branches before legacy
  `process_user_input()` and never receives the suppression flag;
- tool implementations or other components that call channel status delivery
  directly.

The soft-timeout path starts a hard-kill task. After
`HARD_KILL_GRACE_SECS` (30 seconds), it aborts the top-level `handle_message`
task and marks a still-`Processing` legacy thread failed. This bounds emissions
owned by that task, but it is not cooperative cancellation: remote effects,
subprocesses, jobs, missions, or Engine work already spawned into independent
tasks may have their own lifetime.

## The Problem

When a soft timeout fires, the user is notified and the suppression flag is set.
During the 30-second hard-kill grace window, unguarded code in the still-running
task can continue to emit. Independently spawned effects may also continue after
the wrapper task is aborted.

**Concrete scenarios where ghost output can leak:**

- `ChatDelegate` emits a later-step thinking or tool-reasoning status;
- tool execution emits `ToolStarted`, `ToolCompleted`, or `ToolResult`;
- authentication handling emits setup instructions after the timeout reply;
- Engine V2 forwards a response delta or action event while its waiter is still
  alive;
- a long-running/custom tool calls `channels.send_status()` directly;
- approval-resume processing completes and emits `Done` or another approval.

After the user has already received the timeout message, these stray
notifications can be confusing and an authentication status may be more than
merely diagnostic.

## Proposed Solution: Deep AtomicBool Propagation

### Option A: Thread the flag all the way through

Pass `&AtomicBool` (or `Arc<AtomicBool>`) through every turn-owned path that
touches channels or emits status updates:

```
handle_message(&AtomicBool)
  ├─ process_user_input(&AtomicBool)
  │    └─ run_agentic_loop / ChatDelegate callbacks / tool execution
  ├─ process_approval(&AtomicBool)
  │    └─ resumed tool execution and agentic loop
  └─ Engine V2 handle/await/event delivery(&AtomicBool)
```

**Pros:**
- Direct, explicit, no magic
- Every emission point can be guarded
- Easy to reason about ordering

**Cons:**
- Invasive refactor — touches potentially dozens of files
- Every tool implementation needs to be aware of the flag
- Trait signatures change for `Tool`, `ChatDelegate`, etc.

### Option B: Context-Based Suppression

Store the suppression flag in a turn-scoped context (e.g. `TurnContext` struct that's already passed around), and have channel emission helpers check it automatically:

```rust
impl ChannelManager {
    pub async fn send_status_ctx(&self, ctx: &TurnContext, ...) {
        if ctx.is_suppressed() { return; }
        self.send_status(...).await
    }
}
```

**Pros:**
- Single check point per emission, no need to remember
- Less invasive than threading raw flags
- Easy to add new "suppressible" channels/events

**Cons:**
- Requires existing `TurnContext` or creation of one
- Indirect — harder to grep for "where is suppression checked?"

### Option C: Channel-Layer Token

Issue a unique "turn token" at the start of each message, and channels track which tokens are still valid. When the soft timeout fires, the token is invalidated, and any future channel emissions tagged with that token are dropped at the channel layer.

```rust
// On message arrival
let turn_token = channels.begin_turn();

// On soft timeout
channels.invalidate_turn(turn_token);

// In any tool/delegate/anywhere
channels.send_status_tagged(turn_token, status); // silently drops if invalidated
```

**Pros:**
- Suppression is enforced at the channel boundary, not at every call site
- Tools don't need to know anything about it
- Works for SSE/WebSocket too (just check token on emission)
- Naturally extends to other "turn cancelled" scenarios

**Cons:**
- New abstraction layer (turn tokens)
- Channels need to track valid tokens (memory cost, but bounded)
- Requires plumbing tokens through tool and Engine execution contexts anyway
- The timeout notification itself must use an unsuppressed control path (or be
  sent before invalidation) so the token does not suppress the only reply the
  user should receive

## Recommendation

**The original decision remains to defer until there is empirical evidence of
confusing ghost notifications.** The factual scope is broader than the previous
version of this note stated: not all `process_user_input` statuses are guarded,
and unguarded output includes authentication and approval flows as well as
lower-priority tool progress.

When users report behavior such as "I got 'timed out' but then saw 'Running
search tool...'" or a late authentication prompt, revisit this boundary.

If/when you do, **Option C (Channel-Layer Token)** remains the cleanest delivery
boundary, provided it is paired with explicit cancellation for independently
spawned work:

- Centralizes suppression logic in one place
- Avoids changing every tool implementation merely to add a boolean check
- Scales to future "turn cancelled" scenarios (user interrupt, panic recovery,
  channel disconnect)
- Keeps the timeout/error control response outside the invalidated turn

## Open Questions

1. **Ghost task side effects beyond channels:** What about DB writes, workspace file changes, sub-agent spawns? The task is hard-aborted after 30 seconds; should cancellation propagate earlier so in-flight effects can stop cooperatively?

   *Christopher's note (2026-05-19): Not enough info to answer yet.*

2. **Should the flag also propagate to non-channel emissions?** E.g. metrics counters, log spans, etc. Or are those fine to emit regardless?

3. **What about the heartbeat/routine system?** They use their own emission paths via `notify_tx`. Do those need similar suppression?

## Files Likely Affected (if Option C is chosen)

- `ic/src/agent/agent_loop.rs` — create/invalidate the turn token while keeping
  timeout delivery on a control path
- `ic/src/agent/thread_ops.rs` — legacy input and approval-resume propagation
- `ic/src/agent/dispatcher.rs` — `ChatDelegate` and tool-status propagation
- `ic/src/bridge/router.rs` — Engine V2 event/status propagation and cancellation
- `ic/src/channels/mod.rs` — token-aware delivery boundary
- `ic/src/channels/web/sse.rs` and WASM/native channel implementations — verify
  that every status/stream route uses the shared boundary
- tool/job/mission execution contexts — carry cancellation ownership for work
  that can outlive `handle_message`

## Decision Log

- **2026-05-18:** Initial AtomicBool implementation, boundary at `process_user_input`
- **2026-05-19:** Decision to defer deeper propagation, document for future revisit
- **By 2026-07-28:** A 30-second hard-abort fallback bounds the top-level task's ghost-output window; deeper propagation remains deferred
- **2026-08-25:** Re-audit found unguarded auto-compaction, `ChatDelegate`, approval-resume, and Engine V2 status paths; corrected this note without changing the defer decision
- **2026-09-11 (v2.1.1.0):** re-verified symbol anchors (`HARD_KILL_GRACE_SECS` = 30 in `agent_loop.rs`; suppression flag still created per inbound message) — no scope change; defer decision stands
- **2026-09-12 (v2.1.2.0):** re-verified symbol anchors again (`HARD_KILL_GRACE_SECS` still 30; suppression flag still created per inbound message) — no scope change; defer decision stands
- **2026-09-28 (v2.1.3.0):** re-verified symbol anchors again (`HARD_KILL_GRACE_SECS` still 30 in `agent_loop.rs:52`; suppression flag still created per inbound message at `agent_loop.rs:1330`) — no scope change; defer decision stands
- **2026-10-01 (v2.1.4.0):** re-verified symbol anchors again (`HARD_KILL_GRACE_SECS` still 30 at `agent_loop.rs:52`; suppression flag still created per inbound message at `agent_loop.rs:1330`) — no scope change; defer decision stands
- **TBD:** When user feedback or telemetry indicates ghost notifications are a real problem

---

*Documented during LunarWing review session between Christopher and Baud.*
