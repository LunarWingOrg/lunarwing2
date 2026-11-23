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
while the daemon was down are still served. A page that was handed out but not
acknowledged is served again, so delivery stays at-least-once (WI#850) up to
the daemon's in-memory queue: the poller acknowledges a page once it has handed
it to the agent's queue, so messages still waiting in that queue when the
daemon restarts are not served again.

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

## Pre-hop drain and the stale-queue policy

The inbound queue is memory-only and belongs to the bridge process. A backlog
that built up while the daemon was down is served as fresh turns when the
daemon polls again, so right after a release hop an operator can see stale
messages delivered as new work (WI#854).

`POST /v1/messages/drain` discards queued inbound messages. With no body (or
`{}`) it drains the whole queue; `{"older_than_secs": n}` drains only messages
that have waited at least `n` seconds. The response carries `drained`,
`remaining`, and `oldest_queued_age_secs`, and the bridge logs a counted
warning whenever it discards anything, so a drain is never silent.

```bash
curl -sS -X POST "$BASE/v1/messages/drain" \
  -H "Authorization: Bearer $XMPP_BRIDGE_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{}' | jq
```

`scripts/xmpp-drain-queue.sh status|drain [older_than_secs]` wraps the status
and drain calls for pre-hop hygiene.

To bound the backlog automatically instead of draining by hand, set
`XMPP_BRIDGE_STALE_QUEUE_SECS` (default `0`, disabled): a poll then drops
queued messages that have waited at least that long — counted and logged —
instead of replaying them as new turns. `GET /v1/status` reports
`oldest_queued_age_secs` so the threshold can be chosen from live data.

Both mechanisms cover the **bridge** queue only. Messages the daemon has
already polled sit in the daemon's in-memory queue past the acknowledged
cursor and are unaffected.
