# Engine V2 Interrupt-Aware Dispatch Design

## Status

Approved for implementation on 2026-07-13.

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

Keep ordinary channel messages globally serialized while allowing exact
interrupt controls to reach the existing scoped cancellation route during an
active message handler.

## Non-Goals

- Do not make ordinary message handlers concurrent.
- Do not bypass `SubmissionParser`, Engine V2 scope matching, legacy interrupt
  fallback, outbound hooks, or `ChannelManager::respond()`.
- Do not give clear, new-thread, approval, authentication, or arbitrary slash
  commands priority over the active handler.
- Do not change provider, engine cancellation, channel WIT, WASM status, audio,
  or external tool-cancellation contracts.
- Do not claim that an interrupt retracts chunks already delivered to a client.

## Decision

Replace the blocking per-message wait in `Agent::run()` with an
interrupt-aware serialized dispatcher.

At most one ordinary message handler remains active. While it is active, the
dispatcher continues polling the merged channel stream:

1. An exact `Submission::Interrupt` (`/interrupt` or `/stop`) is dispatched
   immediately through the normal `handle_message()` routing path.
2. Every other message is appended to a bounded FIFO deferred queue.
3. When the active ordinary handler completes or reaches its existing soft
   timeout, the oldest deferred message becomes the next ordinary handler.

Priority changes scheduling only. It does not implement cancellation itself.
The existing read-only scope matcher decides whether the interrupt belongs to
an active Engine V2 conversation; unmatched interrupts retain the existing
legacy fallback.

## Dispatcher State

The run loop owns:

- one optional active ordinary-message task;
- its original message, suppression flag, soft-timeout deadline, and abort
  handle;
- a `VecDeque<IncomingMessage>` capped at 256 deferred ordinary messages;
- the existing merged channel stream and shutdown signals.

The 256-message bound prevents a busy turn from turning channel input into
unbounded process memory. When the bound is reached, each additional ordinary
message receives `Agent is busy and its deferred message queue is full. Try
again after the active turn finishes.` through the normal outbound path and is
not silently dropped. Interrupt controls remain observable at the bound.

Messages already in the deferred queue preserve arrival order. A later
interrupt may intentionally overtake them because cancelling the active turn is
the only priority contract.

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

The active ordinary task continues until `ThreadManager::stop_thread()` cancels
its token. Its stopped result is the existing empty sentinel, so it emits no
second terminal response.

## Middleware And Voice Boundary

Ordinary deferred messages receive transcription, document extraction, and
workspace indexing only when they become active, matching current behavior.
The Phase 4 priority path recognizes explicit text controls already emitted by a
channel. A future voice channel should translate speech-start/barge-in into an
explicit scoped interrupt message; Phase 4 does not infer an interrupt from an
audio attachment or stop audio playback.

The dispatcher is channel-neutral. Phase 5 can route XMPP, DarkIRC, WeeChat, or
a future WASM voice channel through the same path without adding gateway-only
cancellation code. WASM channels still ignore `StreamChunk` status updates and
need no WIT change for text `/interrupt` handling.

## Error And Shutdown Semantics

- Failure in a priority interrupt is returned through the same one-response
  error path as ordinary input; it does not cancel the dispatcher task.
- The existing soft timeout, hard-kill grace, response suppression, and pending
  message preservation remain unchanged for the active ordinary handler.
- Ctrl+C and SIGTERM remain observable while an ordinary handler is active.
- End-of-stream waits for the active task and already deferred messages to
  settle before normal shutdown; it does not abandon them silently.
- Duplicate interrupt submissions are handled independently by existing scope
  checks. The design adds no hidden global cancellation state.

## Testing

### Scheduling Regression

Add a deterministic dispatcher test with a pending ordinary handler. Send one
ordinary follow-up and then `/interrupt`. Require the interrupt handler to run
before the pending task is released, while the ordinary follow-up remains first
in the deferred FIFO.

### Routing And Delivery

Retain and run the existing scoped bridge and engine cancellation tests. Add an
agent-level regression proving an interrupt injected through a real channel
while a stream is pending produces one acknowledgement, stops the target, and
does not emit the target's terminal response.

### Queue And Lifecycle

Cover FIFO order, bounded-queue rejection, `/stop` equivalence, non-command text
containing "interrupt", unmatched legacy fallback, soft timeout, and shutdown
while a handler is active.

### Live Gate

Rebuild Brightdawn from the corrected branch and repeat the TensorZero
`2026.3.2` gate. Require:

- at least two streamed chunks before the interrupt;
- `Interrupted.` within two seconds;
- no new chunks after the bounded delivery race drains;
- no cancelled terminal response or persisted assistant response;
- successful same-thread recovery;
- a simultaneously active second thread completes normally;
- no panic, receiver lag, rollback/failure increment, or duplicate response.

## Compatibility And Rollback

Ordinary messages retain global serialization and FIFO execution. The only
intentional behavior change is that exact interrupts can overtake queued
ordinary messages while a handler is active.

Rollback is a source/binary rollback to the pre-dispatch commit followed by the
normal tenant lifecycle restart. No database, environment, WIT, WASM artifact,
provider, or TensorZero configuration change is required.
