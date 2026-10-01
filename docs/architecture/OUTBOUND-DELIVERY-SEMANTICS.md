# Outbound Message Delivery Semantics

**Added 2026-10-01 (v2.1.4.0, WI#270).** Defines what a successful
`message`-tool send means, and what the XMPP channel guarantees about
outbound stanzas. Rooted in the PR Digest #59 incident: a scheduled mission
recorded "Delivered via XMPP … Goal achieved" in its memory because the
`message` tool returned a success-looking ack, then a rebuild/restart window
ate the queued stanza and the user never received anything.

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

The WIT channel contract comments (`ic/wit/channel.wit`
`on-respond`/`on-broadcast`) state the same: `Ok` means accepted for
delivery, not a delivery receipt.

## XMPP specifics (`ic/src/channels/xmpp/mod.rs`)

Outbound stanzas flow through an in-memory `mpsc(64)` queue owned by the
client task. Three hardening rules apply (WI#270 fix ranking, items 1–3):

1. **Enqueue gate.** `queue_outbound_with_attachments` refuses enqueue
   while `connected` is false (no bound XMPP session — disconnected or
   reconnecting) with `SendFailed("XMPP is not connected (no bound
   session); message not queued")`. The caller's tool result then surfaces
   the failure instead of a success-looking ack. Without the gate, a send
   during a reconnect window was a guaranteed silent loss.
2. **Shutdown drain.** `shutdown()` gives the still-running client task a
   bounded window (`OUTBOUND_SHUTDOWN_DRAIN_TIMEOUT`, 2s) to finish
   processing already-acked stanzas, mirroring the coalesce-buffer flush.
   A pending counter (incremented at enqueue, decremented via an RAII slot
   when the client task finishes with a message) drives the wait; if the
   queue does not drain, shutdown WARNs with the pending depth instead of
   losing the messages silently. An empty queue returns immediately.
3. **Post-enqueue failures remain warn-and-drop** (send_stanza error,
   OMEMO prep error, invalid JID). The channel task stays alive and
   healthy; those stanzas are not retried.

## What is still NOT guaranteed (and where it is tracked)

- **Hard process exit** (crash, kill, rebuild): the outbound queue is
  memory-only; anything in it at process death is lost, drain or no drain.
  The durable outbound spool + mission-run ledger that fixes this class is
  the long-term fix (WI#270 suggested fix #4; platform halves tracked in
  WI#277/WI#276 per the 2.1.4.0 open-items audit).
- **End-to-end receipts.** No channel returns a delivery receipt; remote
  confirmation (e.g. XEP-0198 acks) is not part of this contract.

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
