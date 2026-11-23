# xmpp-bridge

`xmpp-bridge` is the local sidecar used by the installable LunarWing `xmpp` WASM channel.

It owns the real XMPP connection, OMEMO state, room membership, and outbound delivery.
The WASM channel talks to it over loopback HTTP.

## Build

```bash
cd bridges/xmpp-bridge
./build.sh
```

## Run

```bash
XMPP_BRIDGE_BIND=127.0.0.1:8787 \
XMPP_BRIDGE_TOKEN=change-me \
./target/release/xmpp-bridge
```

For an isolated LunarWing/XMPP bridge test harness, including local API smoke
tests, live configuration wrappers, and generated user-systemd units, see
[`docs/LUNARWING_XMPP_TESTING.md`](../../docs/LUNARWING_XMPP_TESTING.md).

## Services

On Linux, `lunarwing service install` detects the host service manager:

- systemd: installs `lunarwing.service` and, when available, a companion
  `xmpp-bridge.service` user unit under `~/.config/systemd/user/`
- OpenRC: installs `/etc/init.d/lunarwing` and `/etc/init.d/xmpp-bridge`

`lunarwing service start` and `lunarwing service stop` manage both services
together when the bridge binary is available.

If you run LunarWing as a system service instead of a user service, use the
production templates in:

- `systemd/lunarwing.service`
- `systemd/xmpp-bridge.service`

The main daemon unit declares `Wants=` / `After=` on `xmpp-bridge.service` so
the bridge starts before `lunarwing run`. The bridge unit declares
`PartOf=lunarwing.service` so service stops and restarts are coupled.

## API

- `POST /v1/configure`
- `GET /v1/status`
- `POST /v1/outbound-rate-limit`
- `GET /v1/messages?cursor=<n>&limit=<n>`
- `POST /v1/messages/send`

`GET /v1/messages` returns a bounded page: `limit` defaults to 64 and is
clamped to 100 so one page always fits inside the daemon's per-callback emit
limit. The response `cursor` is the cursor of the last message in the page (or
the cursor the page started after when it is empty), so pollers must persist it verbatim
and poll again to drain the rest of a larger backlog. Returning the newest
cursor instead parked the channel on a backlog larger than one page and left
inbound XMPP wedged after an upgrade (WI#850).

The cursor sequence is per bridge process. If a poller presents a cursor above
the bridge's current cursor (a bridge restart reset the sequence), the response
carries the bridge's current cursor so the poller resynchronizes instead of
waiting forever behind a cursor that can never be reached.

A request that echoes the previous response cursor acknowledges every message
up to it. The daemon keeps its cursor in memory, so after a daemon-only restart
(OpenRC restarts the daemon without this bridge) or a re-activated channel it
polls from zero while this process still holds up to `XMPP_BRIDGE_MAX_MESSAGES`
delivered messages. A request cursor below the acknowledged one is therefore
read as such a reset: the page starts after the acknowledged cursor, so nothing
already delivered comes back as a new inbound turn, and messages that arrived
while the daemon was down are served unless discarded. A retained page that
was handed out but not acknowledged is served again, so delivery stays
at-least-once (WI#850) up to the daemon's in-memory queue: the poller
acknowledges a page once it has handed it to the agent's queue, so messages
still waiting in that queue when the daemon restarts are not served again.

Upgrade order does not matter for delivery. An older bridge ignores `limit`
and returns its whole backlog with its newest cursor; the XMPP guest then
processes at most one page (64 messages) and persists the cursor of the last
one it processed, computed from the consecutive cursor sequence, so the rest
comes back on the next poll instead of being dropped by the host's emit limit.
An older guest against this bridge gets the default page of 64.

When `XMPP_BRIDGE_TOKEN` is set, requests must include `Authorization: Bearer <token>`.
The server also rejects non-loopback clients.

`GET /v1/status` includes the configured plain-room list in `configured_rooms`,
rooms that have produced MUC presence in `rooms_with_presence`, and the active
OMEMO `device_id` and `fingerprint` so you can verify or trust the LunarWing
device from clients like Gajim. It also reports the configured outbound XMPP
hourly cap (`configured_max_messages_per_hour`), the live active cap
(`active_max_messages_per_hour`), and how many outbound messages are still
counted in the current rolling hour (`outbound_messages_last_hour`).

`POST /v1/outbound-rate-limit` lets you change the active outbound XMPP hourly
cap without restarting the bridge or LunarWing. Example:

```bash
curl -sS -X POST "$BASE/v1/outbound-rate-limit" \
  -H "Authorization: Bearer $XMPP_BRIDGE_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"max_messages_per_hour":0}' | jq
```

Set `reset_counter` to `true` if you want to clear the current rolling-hour
usage at the same time.

## Pending inbound messages and optional discard

`GET /v1/status` reports `pending_messages` and `oldest_queued_age_secs`
(`null` when none are pending). Pending means unacknowledged by the poller;
it can include a page already served but not yet acknowledged.
`queued_messages` still counts the entire replay ring.

`POST /v1/messages/drain` requires a JSON body with exactly one mode:
`{"all":true}` or `{"older_than_secs":n}` (age at least `n` seconds).
An age-based drain discards only pending messages that have never been served,
leaving any unacknowledged page available for retry. `all` discards all pending
messages, including any unacknowledged page, and gives up that retry. Both modes
report `drained`, `remaining`, and `oldest_queued_age_secs`. Nonzero drops are
logged.

```bash
BASE=http://127.0.0.1:8787 ic/scripts/xmpp-drain-queue.sh status
# Only if you intend to discard messages:
BASE=http://127.0.0.1:8787 ic/scripts/xmpp-drain-queue.sh drain 3600
```

Supply `XMPP_BRIDGE_TOKEN` through the environment and use the tenant's
registered bridge port on multi-tenant hosts. Use `drain --all` only for an
intentional full discard of pending messages.

Use a manual drain only while the daemon is not polling and the bridge is
still running, before the daemon resumes. This can apply to a daemon-only
restart that leaves a separate bridge running (for example, on OpenRC).
The stock systemd `PartOf=lunarwing.service` coupling and a managed
`upgrade-tenant` on either init stop the bridge, so its memory-only ring
does not survive those stops.

`XMPP_BRIDGE_STALE_QUEUE_SECS` defaults to `0` (off). A value of at least
120 enables expiry on the first poll after a gap of at least that many
seconds. Only never-served pending messages at least that old are dropped;
a served but unacknowledged page remains available for retry.
The first poll of a new bridge process and normal successive polls do not
expire messages when their gap is below the threshold. If you opt in, choose a
threshold above the configured poll interval and expected processing delays;
a long processing stall is indistinguishable from poller downtime here.
Invalid values warn and disable expiry. Managed bridge env
re-renders preserve a configured numeric value.

Neither mechanism protects acknowledged messages already queued in the
daemon at restart. That durability gap (WI#854 D3) remains open.
