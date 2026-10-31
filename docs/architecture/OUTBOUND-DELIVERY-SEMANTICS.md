# Outbound Message Delivery Semantics

**Source revalidated 2026-10-31 for v2.1.8.0.** Defines what a successful
`message`-tool send means and what the native XMPP channel guarantees about
outbound stanzas. WI#270 established acceptance wording; WI#277 added durable
spooling. The initiating incident was a mission recording "Delivered via XMPP"
after an enqueue acknowledgement, followed by a restart losing the queued stanza.

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
