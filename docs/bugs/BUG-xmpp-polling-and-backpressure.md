# XMPP/WASM polling can stop delivering messages

> **Status: PARTIALLY-FIXED (verified against `51ae5a8` on 2026-07-20).**
> Poll-loop crashes and stalls now have supervision and health reporting. A
> bounded queue can still leave a poll task awaiting delivery if downstream
> consumption stalls or sustained ingress exceeds its drain rate; that
> source-confirmed residual was not reproduced and is not automatically
> recovered by the supervisor.

This is the consolidated, current form of `BUG-daemon-stops-polling-xmpp-bridge.md`.

## Original symptoms and reproduction

1. The XMPP bridge remains healthy and reports queued messages.
2. The LunarWing daemon process remains alive, but no messages are consumed.
3. Restarting the daemon restores delivery.

The original report recorded healthy bridge status for two tenants:

`noko`:

```json
{"configured":true,"running":true,"current_cursor":6,"queued_messages":6,
 "omemo_enabled":true,"bundle_published":true,"prekeys_available":99,
 "last_omemo_error":null}
```

`ono`:

```json
{"configured":true,"running":true,"current_cursor":5,"queued_messages":5,
 "omemo_enabled":true,"bundle_published":true,"prekeys_available":99,
 "last_omemo_error":null}
```

The original reproduction said both daemons and bridges were alive while no
messages were observed as consumed, and that restarting a tenant restored
delivery. Those observations are not, by themselves, proof of an undelivered
message: the XMPP bridge exposes a replay buffer. `messages_handler` reads by
cursor and `enqueue_message` appends/trims the buffer; it does not remove
entries on client consumption (`ic/bridges/xmpp-bridge/src/main.rs:414-435,489-519`).
The live outage is therefore **unverified** from the recorded fields alone.
The old proposed-fix section is no longer a plan: most of its implementation is
present below.

A valid current repro must correlate a bridge cursor that advances with no
corresponding emitted-message/agent log, inspect channel health, and show the
WASM delivery task blocked or exited. Nonzero replay-buffer depth alone is not
a failure criterion.

## Current implementation

- `WasmChannel::start_polling` creates an outer supervisor and respawns the inner
  poll loop with exponential backoff after an error or panic
  (`ic/src/channels/wasm/wrapper.rs:2366-2527`).
- Poll callbacks are bounded by the configured callback timeout
  (`ic/src/channels/wasm/wrapper.rs:2536-2564`).
- `health_check()` detects a dead supervisor and a stale
  `last_poll_epoch_ms` (`ic/src/channels/wasm/wrapper.rs:2946-2987`).
- Dedicated tests cover dead and stalled poll tasks
  (`ic/src/channels/wasm/wrapper.rs:5609-5655`).
- Channel health is aggregated by `ChannelManager`
  (`ic/src/channels/manager.rs:210-220`) and exposed by the gateway
  (`ic/src/channels/web/server.rs:2746-2856`).
- The watchdog has optional deep channel-health checks on systemd and OpenRC
  (`ic/scripts/lunarwing-watchdog.sh:60-88` and
  `ic/scripts/lunarwing-watchdog-openrc.sh:60-88`).

These changes resolve the original silent-exit failure mode. Older references
to the polling block near line 2285 no longer match the current wrapper layout.

## Residual backpressure path

The channel's message queue is bounded. `dispatch_emitted_messages` awaits its
`tx.send` (`ic/src/channels/wasm/wrapper.rs:2642-2750`; the channel is created
with capacity 256 at `:2783`). If downstream consumption stalls, or sustained
ingress exceeds the consumer's drain rate, this queue can fill. The poll task is
then pending in `send`, not exited, so the outer supervisor does not respawn it;
health timestamps can remain fresh initially because the poll cycle updates
`last_poll_epoch_ms` before dispatch (`wrapper.rs:2446-2468`).

The earlier direct coupling to a synchronous worker turn is no longer current:
the active-turn loop continues polling the merged channel stream, defers
ordinary messages in a bounded FIFO, and rejects overflow explicitly
(`ic/src/agent/agent_loop.rs:941-1016`, `ic/src/agent/dispatch.rs:6-51`). That
prevents `wait=true` by itself from halting downstream reads. The residual is
the unbounded queue-send await under an independently stalled or overloaded
consumer, not evidence that crash supervision is absent.

Possible follow-up: make delivery backpressure explicit in health metrics and/or
schedule a bounded recovery when the queue remains full. Do not claim this
follow-up is implemented.

The original operator workaround was `lunarwing-mt-admin.sh restart-tenant
<name>`. That may restore a blocked delivery path but also restarts bridge state,
so it is a recovery action rather than proof of root cause.

## Separate OMEMO report

The historical claim that the first few encrypted MUC messages after a
bridge/daemon restart are an expected plaintext-fallback warmup is not verified
in this tree. It is tracked separately, with the concrete cross-JID fallback
evidence and required live reproduction, in
[`BUG-xmpp-omemo-warmup-and-processing.md`](BUG-xmpp-omemo-warmup-and-processing.md).
Neither replay-buffer depth nor a polling restart proves that OMEMO symptom is
resolved.

## Verification record

Status is based on current source, in-tree regression tests, and the watchdog
scripts. No Cargo command, live bridge run, or full E2E run was performed in
this documentation pass; the backpressure path is therefore a residual risk,
not a reproduced current outage.
