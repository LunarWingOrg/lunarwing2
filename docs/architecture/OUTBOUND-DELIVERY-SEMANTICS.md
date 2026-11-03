# Outbound Message Delivery Semantics

**Source revalidated 2026-11-01 for v2.1.8.0.** Defines what a successful
`message`-tool send means and what the native XMPP channel guarantees about
outbound stanzas. WI#270 established acceptance wording; WI#277 added durable
spooling. The initiating incident was a mission recording "Delivered via XMPP"
after an enqueue acknowledgement, followed by a restart losing the queued stanza.
It also defines the per-message lifecycle the agent reports to the channel that
produced a message ([Message lifecycle and turn outcomes](#message-lifecycle-and-turn-outcomes),
added 2026-11-03), which the gateway maps to its `turn_started`/`turn_finished`
events.

Source anchors: `MessageTool::execute` and `resolve_message_target` in
`ic/src/tools/builtin/message.rs`; `queue_outbound_with_attachments` and
`OUTBOUND_SHUTDOWN_DRAIN_TIMEOUT` in `ic/src/channels/xmpp/mod.rs`;
`OutboundSpool`, `SPOOL_MAX_ATTEMPTS`, and retry constants in
`ic/src/channels/xmpp/spool.rs`. Mission recovery is a separate contract in
[ENGINE-V2.md](ENGINE-V2.md).

## Routing and partial acceptance

Explicit tool parameters win. For autonomous executions, job-local
`notify_channel`, `notify_user`, and `owner_id` metadata take precedence over
shared interactive defaults. Switching channels does not reuse a target from a
different source channel; channel-aware fallback resolves the owner or active
conversation target. Gateway sends also carry `notify_thread_id` when present.

A multi-channel broadcast succeeds if at least one channel accepts it; its ack
names only successful channels. Failed channels are logged, and an all-failed
or no-connected-channels result is an error. A success therefore does not imply
that every configured channel accepted the message.

## The contract: acceptance, not delivery

The `message` tool (`ic/src/tools/builtin/message.rs`) calls
`ChannelManager::broadcast`, whose channel implementations queue the
outbound message and return. A successful return means **the channel
accepted the message for delivery** — it is now the channel's outbound
queue's problem. It does **not** mean the recipient received it, and
agents/missions must not record "delivered" in memory based on this tool
alone (the tool description says exactly this).

Since v2.1.4.0 the tool ack is worded honestly:

- single channel: `Queued message to <channel>:<target>`
- broadcast: `Queued message to <channels> (target: <target>)`

The WIT channel interface (`ic/wit/channel.wit` `on-respond`/`on-broadcast`)
inherits the same contract: `Ok` means accepted for delivery, not a delivery
receipt. The interface text is intentionally left untouched — a comment-only
WIT edit would force a package version bump (and registry-wide extension
version churn) with zero behavioral effect; the semantics are spelled out in
the tool description and this document instead.

## XMPP specifics (`ic/src/channels/xmpp/mod.rs`)

Outbound stanzas flow through an in-memory `mpsc(64)` queue owned by the
client task, backed by a durable on-disk spool. Four hardening rules apply
(WI#270 fix ranking, items 1–3, plus WI#277):

1. **Enqueue gate.** `queue_outbound_with_attachments` refuses enqueue
   while `connected` is false (no bound XMPP session — disconnected or
   reconnecting) with `SendFailed("XMPP is not connected (no bound
   session); message not queued")`. The caller's tool result then surfaces
   the failure instead of a success-looking ack. Without the gate, a send
   during a reconnect window was a guaranteed silent loss.
2. **Durable spool (WI#277).** Before the mpsc handoff is acked, the
   message is persisted to `<omemo_store_dir>/spool/<millis>-<seq>-<uuid>.json`
   (atomic tmp-write + fsync + rename; dir `0700`, files `0600`). If the
   spool write fails, the enqueue is refused — the channel never acks a
   message it cannot keep. A failed mpsc handoff discards the record because
   that caller was never acknowledged. The client task claims a record by
   atomic rename before sending, preventing simultaneous spool and queue
   copies from sending it. It deletes the record on send success and replays
   pending records that are due on each `Event::Online` bind and subsequent
   retry ticks. Backoff still applies after reconnect; parked failures need
   operator attention, so persistence is not an unconditional delivery promise.
   The hourly rate limiter is checked before persistence and is not re-applied
   to replays. Spooled content is plaintext JSON, the same on-disk exposure
   class as the adjacent OMEMO identity store. Internal protocol sends
   (e.g. pairing prompts) are ephemeral and deliberately not spooled.
3. **Shutdown drain.** `shutdown()` gives the still-running client task a
   bounded window (`OUTBOUND_SHUTDOWN_DRAIN_TIMEOUT`, 2s) to finish
   processing already-acked stanzas, mirroring the coalesce-buffer flush.
   A pending counter (incremented at enqueue, decremented via an RAII slot
   when the client task finishes with a message) drives the wait; if the
   queue does not drain, shutdown WARNs with the pending depth — a
   delivery-*delay* notice now, not data loss, because every acked message
   still pending has a spool record the next bind replays. An empty queue
   returns immediately.
4. **Post-enqueue failures retry from the spool** (WI#277). A send_stanza
   error, OMEMO/encrypted-room prep error, or XEP-0363 upload failure marks
   the record for retry with exponential backoff (30s base, capped at 1h);
   after 24 attempts the record is parked as `<key>.failed.json` with the
   last error for operator inspection — never silently dropped. An invalid
   target JID can never parse, so it is parked as failed immediately. The
   channel task stays alive and healthy throughout.

## What is still NOT guaranteed (and where it is tracked)

- **Exactly-once delivery.** The spool makes the XMPP path at-least-once:
  a crash after the stanza reached the wire but before the record was
  cleared replays the message on the next bind, so duplicates are possible
  (whole-message granularity — a retried multi-part message re-sends parts
  that already went out). End-to-end receipts would be needed to close
  this; no channel returns a delivery receipt and remote confirmation
  (e.g. XEP-0198 acks) is not part of this contract.
- **Mission-run ledger.** The engine-side half of WI#270's long-term fix is
  separate from the channel spool. For a mid-run fire of an Active mission,
  the run is stopped within a short grace, the interruption is recorded,
  and a persisted retry marker re-fires it after restart within the
  mission's retry policy (WI#276; see ENGINE-V2.md, mission retries — a
  policy with no attempts left records the interruption without a re-fire).
  A mission paused or retired mid-run instead records a quiet stop with no
  retry. An abort between the settled stop and the mission-row save still
  loses the fire, but the next boot releases its recorded dedup key without
  auto-refiring (WI#394), so an external redelivery can proceed. A run that
  cannot stop within the grace is recorded and stamped at shutdown; the
  boot sweep resolves the stamp against the thread's final state and
  retries a settled stop, while a hard kill stays restart recovery's
  record-only case (WI#301). A re-fired run may repeat side effects the
  interrupted run already performed (at-least-once).
- **Other channels.** The spool is XMPP-local. Channels with similar
  in-memory outbound queues keep their existing semantics.

## Mission-spec guidance (the agent-side half of WI#270)

Mission/routine specs that deliver proactive output should:

1. write the daily log entry,
2. send the message (treat success as *accepted for send, confirmation
   unknown*),
3. only then write memory, recording acceptance — never "delivered" —
   and capture the tool's error text verbatim on failure;
4. update the run's baseline/state file so the next run delta-tracks
   instead of rescanning (WI#270 Defect B).

The PR Digest mission spec itself is tenant workspace data (not in this
repo); the above is the contract platform-side code now enforces and
documents.

## Message lifecycle and turn outcomes

**Added 2026-11-03 (TUI design 8.2, milestone M2a).** A reply alone does not
tell a client that the agent is done with a message: a turn can end without a
reply, stop at a gate, or keep running in the background. The agent therefore
reports, for every inbound message, when it started handling it and how that
ended. The vocabulary is `lunarwing_common::TurnOutcome`; the gateway's wire
form is specified in [GATEWAY_CHAT_API.md](../guides/GATEWAY_CHAT_API.md#turn-lifecycle).

Source anchors: `Channel::message_lifecycle` (`ic/src/channels/channel.rs`);
`MessageLifecycle` and `OutcomeHints` (`ic/src/channels/lifecycle.rs`);
`ChannelManager::{message_lifecycle, note_turn_outcome}`
(`ic/src/channels/manager.rs`); `Agent::{supervise_message,
handle_priority_control, acknowledge_cancelled_lane, defer_message,
handle_priority_interrupt, report_started, report_finished}`, `turn_outcome`,
`submission_outcome_hint` and `none_result_control`
(`ic/src/agent/agent_loop.rs`); `TurnCompletion::{await_outcome, complete,
deliver_late}` (`ic/src/bridge/router/outcome.rs`) and
`insert_and_notify_pending_gate` (`ic/src/bridge/router.rs`).

### The hook

`Channel::message_lifecycle(msg, Started | Finished(outcome))` has a no-op
default. `ChannelManager::message_lifecycle` looks up the channel named by
`msg.channel` (an `Arc` clone; no registry lock is held across the call) and
forwards the event; an unknown channel is a no-op. Implementations must not
block: the agent awaits the call on the task that supervises the message.
Wrappers forward it (`SharedWasmChannel`; the test `TestChannelHandle`).

| Channel | Mapping |
| --- | --- |
| `gateway` | SSE `turn_started` / `turn_finished`, only for messages that carry a chat-handler `message_id` and a thread; internal messages (job monitor, routines) and WebSocket sends emit nothing |
| `unix_socket_repl` | nothing yet (the socket milestones add `TurnStarted` / `TurnFinished` frames) |
| every other channel | the default no-op |

### Outcomes

| `TurnOutcome` | Meaning |
| --- | --- |
| `replied` | A non-empty reply was handed to `respond()` before the event |
| `no_reply` | Handling ended without a reply: a suppressed or silent reply, a consumed control, a stale or bare approval keyword, a hook rejection, the DEC-1 `Ok(None)` |
| `paused` | The turn stopped at a gate (approval, credential, external callback); a continuation follows the decision |
| `interrupted` | A running turn was stopped by `/interrupt` |
| `detached` | The turn outlived its wait budget and continues in the background; not terminal, exactly one more `Finished` follows |
| `failed` | The handler returned an error or an `Error: ...` reply, panicked, or the engine reported failure (including max iterations) |
| `timed_out` | The agent loop's soft timeout fired; the timeout notice was sent |
| `cancelled` | The message did not complete: removed from the queue by an interrupt or refused by a full queue (it never ran), or cancelled by shutdown while it ran |

`Unknown` exists only for readers of newer daemons; producers never emit it.

### When the events fire

`Started` fires when the scheduler hands the message to `supervise_message`
(its first statement), or right away for a priority control handled inline
(`/interrupt`, `/stop`, `/steer`). `Finished(outcome)` fires exactly once per
message, after any reply was handed to `respond()`:

| Path | `Finished` |
| --- | --- |
| The handler returned | the outcome computed from its result, refined by a hint (below) |
| The handler panicked | `failed`, after the panic notice |
| The soft timeout fired | `timed_out`, after the timeout notice |
| Shutdown cancelled the running message | `cancelled` |
| A queued prompt was removed by an interrupt | `cancelled`, with no `Started`; the interrupt then gets `Started`, its `Interrupted.` reply and `replied` |
| The deferred queue (256 messages) was full | the busy reply, then `cancelled`, with no `Started` |

- The one exception to "exactly once": `Finished(detached)`, an Engine V2 turn
  that outlived its wait budget, is followed by exactly one more `Finished`
  from the late watcher (`deliver_late`), after its late delivery.
- `paused` ends the message. The continuation (the approval, or the Engine V2
  credential resume) is a new message with its own `Started` and `Finished`;
  the gateway correlates it to the owning turn through `client_turn_id`.
- Messages still waiting in the scheduler when the process exits get nothing.

### Ordering

The reply is broadcast before the `Finished` of its message:
`deliver_message_result` hands it to `respond()` and `report_finished` runs
afterwards on the same task, and the gateway broadcasts synchronously into its
one SSE channel, so every subscriber sees `response` before `turn_finished`.
Exception: replies to coordination rooms with outbound coalescing
(`COORD_CHANNELS`, `CoalesceConfig` in `ic/src/channels/coalesce.rs`) can be
buffered and flushed after `Finished`. The gateway and the socket are not
coordination channels.

### How the outcome is computed

The agent loop rates the handler result: a non-empty reply is `replied`; an
empty reply (the suppression sentinels: approval prompt, hook rejection, stale
approval) or `Ok(None)` is `no_reply`; an error is `failed`. Code that knows
more records an outcome hint with `ChannelManager::note_turn_outcome` before
the handler returns:

| Producer | Hint |
| --- | --- |
| V1 `handle_message`: `NeedApproval` / `Interrupted` / `Error` results (the `/quit` refusal and the thread-hydrate rejection included) | `paused` / `interrupted` / `failed` |
| V2 `insert_and_notify_pending_gate` (approval, credential and external gates) | `paused` |
| V2 `TurnCompletion::complete`: `Stopped` / `Failed` or `MaxIterations` | `interrupted` / `failed` |
| V2 `TurnCompletion::await_outcome` when the wait budget is exhausted | `detached` |

Rules (`OutcomeHints`):

- A hint replaces only a computed `replied` or `no_reply`. `failed`,
  `timed_out` and `cancelled` computed by the agent loop are authoritative.
- A later hint for the same message replaces an earlier one.
- The next `Finished` for the message consumes the hint whatever the result,
  also for an unknown channel.
- At most 1024 hints are kept, each for at most 30 minutes, behind a std mutex
  that is never held across an `.await`.
- V1 records no hint once the soft timeout has fired (the loop has already
  reported `timed_out`).
- The late watcher rates its late result the same way, refined by the hints
  the late completion recorded.

### The DEC-1 guard: `Ok(None)` on client channels

`Ok(None)` from the message handler is the agent loop's shutdown signal
(`/quit`, `/exit`, `/shutdown` on the in-process REPL). Model output reaches it
too: a V1 reply that is `NO_REPLY` (`is_silent_reply` allows up to four
trailing spaces or punctuation characters) and Engine V2
`Completed { response: None }`. On the two channels whose clients attach to a
running service, `gateway` and `unix_socket_repl`:

- `/quit`, `/exit` and `/shutdown` are refused before they reach the
  shutdown path (`quit_refusal` in `ic/src/agent/commands.rs`): the client gets
  an `Error: ...` reply and the message finishes `failed`.
- `deliver_message_result` maps `Ok(None)` to `Continue` instead of `Shutdown`
  (`none_result_control`, with a warning), and the message finishes
  `no_reply`.

Every other channel, including the in-process REPL, keeps the shutdown. The
producers themselves stay tracked as separate bugs. To revert the guard, map
`Ok(None)` to `MessageLoopControl::Shutdown` in `deliver_message_result` again
(the old line is quoted in a comment there), delete `none_result_control` and
its unit test, and delete the two `*_silent_reply_does_not_stop_agent` tests in
`ic/tests/e2e_quit_refusal.rs` with their trace
`ic/tests/fixtures/llm_traces/tui/silent_reply.json`. The `/quit` refusal is
independent of the guard.

### Known gaps

- A panic in `supervise_message` itself, outside the handler task (a
  panicking channel `respond()`, a hook, or a channel's own
  `message_lifecycle`), reports no `Finished`; the main loop only logs it.
- Messages still queued at process exit get no `Finished`; clients
  resynchronize when they reconnect.
- With `AGENT_HANDLE_MESSAGE_TIMEOUT_SECS` between 580 and 610 (below the
  recommended 630), the loop reports `timed_out` before Engine V2 detaches at
  610 s. The `detached` hint then outlives the loop's report, and the late
  watcher's `Finished` reports `detached` instead of its real outcome. Clients
  ignore a second finish of a sealed turn.
