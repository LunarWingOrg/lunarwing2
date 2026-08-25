# XMPP/WASM polling can stop delivering messages

> **Status: PARTIALLY-FIXED (verified against current source on 2026-08-25).**
> Poll-loop crashes and stalls now have supervision and health reporting. A
> bounded queue can still leave a poll task awaiting delivery if downstream
> consumption stalls or sustained ingress exceeds its drain rate; that
> source-confirmed residual was not reproduced. The in-process supervisor does
> not restart a merely-blocked poll task, but because the poll epoch only
> advances after a successful dispatch, a queue-blocked poll trips the stall
> detector and is visible to health reporting (and to the optional watchdog
> deep check).

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
entries on client consumption (`ic/bridges/xmpp-bridge/src/main.rs:446-463,515-546`).
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
  (`ic/src/channels/wasm/wrapper.rs:2311-2476`).
- Poll callbacks are bounded by the configured callback timeout
  (`ic/src/channels/wasm/wrapper.rs:2484-2513`).
- `health_check()` detects a dead supervisor and a stale
  `last_poll_epoch_ms` (`ic/src/channels/wasm/wrapper.rs:2933-2975`).
- Dedicated tests cover dead and stalled poll tasks
  (`ic/src/channels/wasm/wrapper.rs:5719-5783`).
- Channel health is aggregated by `ChannelManager`
  (`ic/src/channels/manager.rs:345-354`) and exposed by the gateway
  (`ic/src/channels/web/server.rs:2929-2971`).
- The watchdog has optional deep channel-health checks on systemd and OpenRC
  (`ic/scripts/lunarwing-watchdog.sh:83-102` and
  `ic/scripts/lunarwing-watchdog-openrc.sh:75-94`).

These changes resolve the original silent-exit failure mode. Older references
to the polling block near line 2285 no longer match the current wrapper layout
(`start_polling` is now at line 2311).

## Residual backpressure path

The channel's message queue is bounded (capacity 256, created at
`ic/src/channels/wasm/wrapper.rs:985,2770`). `dispatch_emitted_messages`
(`wrapper.rs:2595-2678`) awaits `tx.reserve_many` (`:2639-2645`) and sends via
permits. If downstream consumption stalls, or sustained ingress exceeds the
consumer's drain rate, this queue can fill and the `reserve_many` await blocks
indefinitely; the dispatch path only errors for a closed queue, a missing
sender, an oversized batch, or rate limiting. The poll task stores
`last_poll_epoch_ms` only after a successful dispatch
(`wrapper.rs:2409-2416`), so a queue-blocked poll stops refreshing the epoch
and trips the stall detector once `max(3 x poll interval, 30s)` elapses.

The earlier direct coupling to a synchronous worker turn is no longer current:
the active-turn loop continues polling the merged channel stream, defers
ordinary messages in a bounded FIFO, and rejects overflow explicitly
(`ic/src/agent/agent_loop.rs:1106-1184`, `ic/src/agent/dispatch.rs:6-51`). That
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
not a reproduced current outage. Re-verified on 2026-08-13 (revision
`7e813db4`): the residual section was re-checked — the poll epoch is still
stored only after a successful dispatch, so a queue-blocked poll remains
visible to the stall detector, and a full queue still blocks `reserve_many`
rather than erroring. Re-verified on 2026-08-20 (revision `2d10832a`): the
same residual holds — epoch store at `wrapper.rs:2409-2416`, `reserve_many`
await at `:2639-2645` — and line references were refreshed again for
`wrapper.rs` (`start_polling` `:2311-2476`, `health_check` `:2933-2975`, queue
creation `:985,2770`, `dispatch_emitted_messages` `:2595-2678`, poll tests
`:5719-5783`), `manager.rs` (`health_check_all` `:345-354`), and `server.rs`
(`:2929-2971`) churn. No status changed. Re-verified on 2026-08-25 (revision
`57e8a36f`): no status changed. `channels/wasm/wrapper.rs`, `manager.rs`, and
`server.rs` are unchanged, so every wrapper/manager/server reference still
matches exactly. The XMPP bridge grew (attachment/upload work), moving
`messages_handler` to `main.rs:446-463` and `enqueue_message` to `:515-546`;
the watchdog scripts gained checks, moving the deep channel-health regions to
`lunarwing-watchdog.sh:83-102` and `lunarwing-watchdog-openrc.sh:75-94`; and
the work-item #73 `agent_loop.rs` insertions moved the FIFO/deferral region to
`:1106-1184`. The residual queue-backpressure path is unchanged.
