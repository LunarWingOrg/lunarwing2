# Engine V2 Interrupt-Aware Dispatch Design

## Status

Implemented on 2026-07-13. Amended on 2026-08-28 for v2.0.9.0 item #2:
the dispatcher now removes cross-thread gateway head-of-line blocking while
retaining same-thread and non-gateway serialization.

## Context

Engine LLM streaming Phase 4 added per-running-thread cancellation tokens,
propagated cancellation through stream acquisition and collection, mapped a
cancelled call to `ThreadOutcome::Stopped`, and routed gateway
`Submission::Interrupt` to the scoped Engine V2 conversation. Unit and bridge
tests prove those layers directly.

The Brightdawn live gate exposed an earlier ingress blocker. `Agent::run()`
spawns `handle_message()` but then awaits that task (or its 400-second soft
timeout) before polling the merged channel stream again. A `/interrupt` request
can therefore be accepted by the gateway while a response is streaming, yet it
cannot reach `SubmissionParser` or `bridge::handle_interrupt()` until the active
turn has already completed.

The 2026-07-13 reproduction started the target Engine V2 thread at
`20:24:08Z`. The target completed normally at `20:25:39Z`; the queued
`/interrupt` was not parsed until `20:25:58Z`, after another previously queued
message. The required two-second acknowledgement did not occur, and the target
was not cancelled.

## Goal

Preserve ordered execution within each conversation while allowing exact
interrupt controls and independent Engine V2 gateway threads to make progress
during a slow active turn.

## Non-Goals

- Do not run two ordinary messages for the same gateway thread concurrently.
- Do not parallelize legacy, non-gateway, control, approval, authentication, or
  arbitrary slash-command handling.
- Do not bypass `SubmissionParser`, Engine V2 scope matching, legacy interrupt
  fallback, outbound hooks, or `ChannelManager::respond()`.
- Do not change provider, channel WIT, WASM status, audio, or external
  tool-cancellation contracts.
- Do not claim that an interrupt retracts chunks already delivered to a client.

## Decision

`Agent::run()` owns an interrupt-aware bounded scheduler and a supervised task
set. It continuously polls the merged channel stream and completed tasks:

1. An exact `Submission::Interrupt` (`/interrupt` or `/stop`) is dispatched
   immediately through the normal `handle_message()` routing path.
2. Other messages enter a 256-entry queue.
3. Engine V2 `UserInput` on gateway messages with a conversation scope is keyed
   by `(channel, authenticated user, conversation scope)`. Up to four distinct
   keys for one `(channel, user)` group may run concurrently.
4. The same key remains serialized. A different user, a non-gateway message, or
   an exclusive control at the queue head is a fairness barrier and cannot be
   overtaken.
5. Completion or the existing soft timeout releases that scheduling lane.

This removes the root cause of v2.0.9.0 systemd stress finding F3: one slow
foreground thread no longer keeps an independent tail-of-soak gateway thread in
the global queue until the harness deadline. Parallel eligibility remains
narrow because channel adapters and the legacy tool path have shared routing
state that is not generally safe for concurrent turns.

Priority changes scheduling only. The existing read-only scope matcher decides
whether an interrupt belongs to an active Engine V2 conversation; unmatched
interrupts retain the existing legacy fallback.

## Dispatcher State

The run loop owns:

- a `MessageScheduler` with a 256-message waiting bound and four-lane parallel
  gateway cap;
- a `JoinSet` of supervised ordinary-message tasks and a dispatch ticket for
  each active task;
- the existing merged channel stream and shutdown signals;
- one cancellation token shared by all message supervisors for graceful
  shutdown.

The bound prevents sustained input from consuming unbounded process memory.
When it is reached, each additional ordinary message receives `Agent is busy
and its deferred message queue is full. Try again after the active turn
finishes.` through the normal outbound path and is not silently dropped.
Interrupt controls remain observable at the bound.

The queue is FIFO except for the narrow safe-lane rule: a message blocked by its
already-active gateway thread may be skipped by a later distinct thread for the
same authenticated user. A different user or exclusive message stops that scan,
preventing unbounded overtaking. Exact interrupts intentionally bypass the
queue.

## Interrupt Path

Priority classification uses the existing `SubmissionParser`; substring or
prefix checks are not allowed. Only `Submission::Interrupt` qualifies.

The priority path calls `handle_message()` with its own unsuppressed flag, then
uses the same outbound result handling as an ordinary message. This preserves:

- Engine V2 versus legacy routing;
- user, channel, and conversation-scope isolation;
- the single `Interrupted.` acknowledgement;
- `BeforeOutbound` modification or rejection;
- empty-response suppression and normal channel metadata.

The targeted ordinary task continues until `ThreadManager::stop_thread()`
cancels its token. Its stopped result is the existing empty sentinel, so it
emits no second terminal response.

## Middleware And Voice Boundary

Ordinary queued messages receive transcription, document extraction, and
workspace indexing only when they become active, matching prior behavior. The
priority path recognizes explicit text controls already emitted by a channel. A
future voice channel should translate speech-start/barge-in into an explicit
scoped interrupt message; the dispatcher does not infer an interrupt from an
audio attachment or stop audio playback.

All channels use the scheduler and priority path, but concurrent ordinary turns
are currently limited to scoped Engine V2 gateway input. XMPP, DarkIRC, WeeChat,
and future WASM voice channels retain serialized ordinary handling. WASM
channels still ignore `StreamChunk` status updates and need no WIT change for
text `/interrupt` handling.

## Error And Shutdown Semantics

- Failure in a priority interrupt is returned through the same one-response
  error path as ordinary input; it does not cancel unrelated supervisors.
- Each ordinary supervisor owns its original message, suppression flag,
  soft-timeout deadline, and inner handler abort guard.
- The existing soft timeout, hard-kill grace, and response suppression remain
  unchanged. A soft-timed-out inner handler may continue during its grace
  period, matching prior behavior.
- Ctrl+C and SIGTERM cancel and join all supervisors before channel shutdown.
  Abrupt supervisor cancellation aborts its inner handler as defense in depth.
- Awaiting an Engine V2 foreground outcome is cancellation-safe: dropping that
  wait requests thread cancellation, so shutdown cannot leave an orphaned LLM
  or tool task running in global engine state.
- End-of-stream drains active tasks and already queued messages before normal
  shutdown. Duplicate interrupts retain existing scope checks.

## Testing

`ic/src/agent/dispatch.rs` covers distinct-thread parallelism, same-thread
serialization, the four-lane cap, user and exclusive fairness barriers, FIFO
behavior, the queue bound, and exact interrupt classification.

`ic/tests/engine_v2_interrupt_ingress.rs` covers:

- an independent gateway thread starting while another LLM call is pending;
- same-thread follow-up serialization and recovery after interrupt;
- scoped and unmatched interrupts;
- bounded-queue overflow;
- soft-timeout detach behavior; and
- cancellation of an active Engine V2 handler when the agent task is aborted.

The v2.0.9.0 item-2 gate ran the eight-test integration suite repeatedly, the 51
focused agent dispatch/dispatcher tests, default `cargo check`, formatting, and
zero-warning clippy. A fresh tenant was not required: F3's queueing mechanism is
deterministically reproduced with a pending first LLM and an independent second
thread, and the regression fails against the pre-fix dispatcher.

## Compatibility And Rollback

Same-thread gateway messages and every non-gateway/control path retain ordered
serialization. The intentional behavior change is bounded concurrency for
distinct scoped Engine V2 gateway threads belonging to one authenticated user;
exact interrupts continue to overtake queued ordinary messages.

Rollback is a source/binary rollback to the pre-item-2 commit followed by the
normal tenant lifecycle restart. No database, environment, WIT, WASM artifact,
provider, or TensorZero configuration change is required.
