# XMPP Custom Bridge

The XMPP integration is LunarWing's reference "custom bridge": a sandboxed WASM
channel paired with a native sidecar process that owns the real protocol
session. This split keeps the long-lived XMPP/OMEMO connection and its
encryption state out of the WASM sandbox while still exposing XMPP to the agent
through the normal `Channel` interface.

## Repository status

This bridge was originally maintained as a standalone repo (historically
referenced as `kageho_ironclaw_xmpp_and_replv2`). It now lives **in-tree** in
the main LunarWing repository:

| Component | Path |
|-----------|------|
| Sidecar bridge binary | `ic/bridges/xmpp-bridge/` |
| Installable WASM channel | `ic/channels-src/xmpp/` |
| Bridge HTTP contract crate | `ic/openclaw-ports/xmpp/bridge/` |

`xmpp_bridge/README` at the repo root notes an intent to eventually split the
bridge back out into its own repository; that has not happened, so treat the
in-tree paths above as authoritative.

## Architecture

Two cooperating pieces:

1. **`xmpp-bridge`** — a standalone native binary (`xmpp-bridge` crate, Rust
   edition 2024). It owns the real XMPP connection, OMEMO state, MUC room
   membership, the reconnect loop, and outbound delivery. It exposes a small
   loopback HTTP API.
2. **`xmpp` WASM channel** — the installable channel package that runs inside
   the wasmtime sandbox. It cannot open raw sockets, so it talks to the bridge
   over loopback HTTP: it pushes configuration, polls for inbound messages by
   cursor, and posts outbound sends.

The bridge wraps the in-process `lunarwing::channels::XmppChannel`; the WASM
channel is the agent-facing adapter. Configured `rooms` are auto-joined by the
bridge on connect. Configured `encrypted_rooms` are treated as fail-closed
encrypted groupchats (validated as non-anonymous, members-only; member/admin/
owner real JIDs cached from MUC state; plaintext groupchat traffic ignored).

## Build

Bridge binary:

```bash
cd ic/bridges/xmpp-bridge
./build.sh            # cargo build --release; emits target/release/xmpp-bridge
```

WASM channel:

```bash
cd ic
./channels-src/xmpp/build.sh   # emits channels-src/xmpp/xmpp.wasm
```

## Run

```bash
XMPP_BRIDGE_BIND=127.0.0.1:8787 \
XMPP_BRIDGE_TOKEN=change-me \
./target/release/xmpp-bridge
```

The WASM channel defaults to reaching the bridge at `http://127.0.0.1:8787`
(its `bridge_url` setup field), matching the bridge's default bind.

## Environment variables

| Variable | Default | Purpose |
|----------|---------|---------|
| `XMPP_BRIDGE_BIND` | `127.0.0.1:8787` | Listen address. Non-loopback clients are always rejected regardless of bind. |
| `XMPP_BRIDGE_TOKEN` | _(unset)_ | Bearer token required on every request when set. Empty/unset disables token auth (loopback restriction still applies). |
| `XMPP_BRIDGE_MAX_MESSAGES` | `2048` | Inbound ring-buffer capacity. Values `<= 0` or unparseable fall back to the default. The production systemd unit sets this to `1024`. |
| `XMPP_BRIDGE_STALE_QUEUE_SECS` | `0` (disabled) | Opt-in expiry, minimum 120 seconds. On the first poll after at least this long without a poll, discard pending messages at least this old. Invalid values warn and disable expiry. |
| `RUST_LOG` | `info` | Standard `tracing` env filter. |
| `LUNARWING_BASE_DIR` | _(see bootstrap)_ | Base dir whose `xmpp/` subdir is the default OMEMO store when `omemo_store_dir` is not supplied. `IRONCLAW_BASE_DIR` is still accepted as a legacy alias. |

## HTTP API

All routes are versioned under `/v1` and served over loopback only.

| Method & path | Purpose |
|---------------|---------|
| `POST /v1/configure` | Supply JID/password/policy and start the XMPP session. |
| `GET /v1/status` | Report configuration, connection, OMEMO, room, and rate-limit diagnostics. |
| `POST /v1/outbound-rate-limit` | Change the live outbound hourly cap without restarting. |
| `GET /v1/messages?cursor=<n>&limit=<n>` | Return a bounded page of queued inbound messages with a cursor strictly greater than `<n>` (returns immediately — not a long-poll). |
| `POST /v1/messages/drain` | Explicitly discard pending inbound messages and report the counts. |
| `POST /v1/messages/send` | Send an outbound message, optionally with base64 attachments. |

### Authentication

- The server **always** rejects non-loopback clients (`403`).
- When `XMPP_BRIDGE_TOKEN` is set, every request must carry
  `Authorization: Bearer <token>` (the `bearer` prefix is also accepted);
  missing or wrong tokens return `401`.

### `POST /v1/configure`

`configure` is idempotent: re-posting the **same** normalized config returns the
current `{configured, running, jid}` state. Posting a **different** config while
already configured returns `409 Conflict` — the bridge must be restarted to
apply a different configuration.

Notable request handling:

- `dm_policy` is lowercased; empty defaults to `allowlist`.
- `omemo_store_dir` defaults to `<base-dir>/xmpp` when omitted.
- `encrypted_rooms` entries are merged into `allow_rooms` (unless `allow_rooms`
  already contains `*`), so encrypted rooms are always joined.
- `resource`, when provided and the JID has no `/resource`, is appended to the
  JID.

### `GET /v1/status`

Returns connection and crypto diagnostics. Fields include:

- `configured`, `running`, `jid`
- `current_cursor`, `queued_messages` (retained replay entries),
  `pending_messages` (retained but unacknowledged), and
  `oldest_queued_age_secs` (oldest pending age, or `null` when none are pending)
- `configured_rooms` (the configured room join list — `allow_rooms`, into
  which any `encrypted_rooms` are merged) and `rooms_with_presence` (rooms that
  have produced MUC presence)
- Outbound rate limit: `configured_max_messages_per_hour`,
  `active_max_messages_per_hour`, `outbound_messages_last_hour`,
  `outbound_rate_limit_overridden`
- OMEMO: `omemo_enabled`, `device_id`, `fingerprint`, `bundle_published`,
  `prekeys_available`, `migration_state`, `last_omemo_error`
- Encrypted rooms: `encrypted_rooms_total`, `encrypted_rooms_ready`,
  `last_room_error`

`device_id` and `fingerprint` let you verify/trust the LunarWing device from a
client such as Gajim.

### `GET /v1/messages`

Returns a bounded page of queued inbound messages whose cursor is strictly
greater than the request `cursor`.

- `limit` defaults to 64 and is clamped to 100, so one page always fits inside
  the daemon's per-callback emit limit. A backlog larger than one page is
  drained across successive polls.
- The response `cursor` is the cursor of the **last message in the page**, or
  the cursor the page started after when the page is empty. Pollers persist it
  verbatim;
  returning the newest cursor instead made a large backlog replay every tick
  and left inbound XMPP wedged until a restart (WI#850).
- The bridge's cursor sequence is per-process. A request cursor above the
  bridge's current cursor means the bridge restarted: the response carries the
  bridge's current cursor so the poller resynchronizes rather than waiting
  behind an unreachable position.
- A request that echoes the previous response cursor acknowledges everything
  up to it. A request cursor below the acknowledged one means the poller lost
  its cursor (the daemon keeps it in memory, so a daemon-only restart or a
  re-activated channel polls from zero): the page then starts after the
  acknowledged cursor, so delivered messages are not replayed as new inbound
  turns, while messages queued meanwhile are still served. A page handed out
  but not yet acknowledged is served again (at-least-once) (WI#850). The
  poller acknowledges a page once the daemon has queued it for the agent, so
  messages still in that in-memory queue at a daemon restart are not served
  again.

### `POST /v1/messages/send`

Sends an outbound message, optionally with base64 attachments.

### `POST /v1/messages/drain`

Requires `Content-Type: application/json` and exactly one discard mode:
`{"all":true}` or `{"older_than_secs":n}` (inclusive age). Missing or invalid
requests leave the queue unchanged. The response reports `drained`,
`remaining`, and `oldest_queued_age_secs` for pending entries only; a served
but unacknowledged page is still pending. Nonzero drops are logged.

Run `ic/scripts/xmpp-drain-queue.sh status` with an explicit loopback `BASE`
and `XMPP_BRIDGE_TOKEN` before choosing `drain --all` or `drain <seconds>`.
Discard only from a bridge that survives a daemon-only stop (for example
on OpenRC) and before polling resumes. Stock systemd service coupling and
managed tenant upgrades stop the bridge itself, clearing its memory-only
ring. This is not a recovery mechanism for acknowledged messages in the
daemon's queue.

### `POST /v1/outbound-rate-limit`

Changes the active outbound hourly cap live (no restart). Omitting
`max_messages_per_hour` keeps the current cap. Set `reset_counter` to `true` to
also clear the current rolling-hour usage.

```bash
curl -sS -X POST "http://127.0.0.1:8787/v1/outbound-rate-limit" \
  -H "Authorization: Bearer $XMPP_BRIDGE_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"max_messages_per_hour":0}' | jq
```

A cap of `0` disables outbound rate limiting. The repo also ships
`ic/scripts/xmpp-rate-limit.sh` (`status`, `set <n>`, `off`, `reset`) as a
convenience wrapper.

## Reconnect behavior

After `configure`, the bridge consumes the inbound stream in a background task.
If the stream ends, it enters a reconnect loop with exponential backoff: it
starts at 1 second, doubles on each failed attempt, caps at 300 seconds (5
minutes), and adds up to 500 ms of jitter. Backoff resets to 1 second after a
successful reconnect. The shared channel reference is swapped atomically on each
reconnect so sends always target the live connection.

## WASM channel configuration

The `xmpp` channel package (`ic/channels-src/xmpp/`) consumes:

- Secrets: `xmpp_password`, `xmpp_bridge_token` (optional)
- Setup fields: `xmpp_jid`, `bridge_url`, `dm_policy`, `allow_from`, `rooms`,
  `encrypted_rooms`, `allow_plaintext_fallback`, `max_messages_per_hour`,
  `resource`, `device_id`, `omemo_store_dir`

`max_messages_per_hour` caps outbound XMPP sends per bot instance; set it to `0`
to disable the cap. Secrets and fields are persisted via onboarding, the channel
setup UI, or extension configuration flows, then pushed to the bridge by the
channel's `configure` call. The capability manifest is
`ic/channels-src/xmpp/xmpp.capabilities.json`.

## Deployment (services)

On Linux, `lunarwing service install` detects the host service manager:

- **systemd** — installs `lunarwing.service` plus a companion
  `xmpp-bridge.service` user unit under `~/.config/systemd/user/` when the
  bridge binary is available.
- **OpenRC** — installs `/etc/init.d/lunarwing`, plus `/etc/init.d/xmpp-bridge`
  only when the bridge binary is available (the same condition as the systemd
  companion unit).

`lunarwing service start` / `lunarwing service stop` manage both services
together when the bridge binary is present.

Production system-level templates live at `ic/systemd/lunarwing.service` and
`ic/systemd/xmpp-bridge.service`. Their coupling:

- `lunarwing.service` declares `Wants=` / `After=` on `xmpp-bridge.service`, so
  the bridge starts before `lunarwing run`.
- `xmpp-bridge.service` declares `PartOf=lunarwing.service`, so stops and
  restarts of the main daemon cascade to the bridge.

Keep `XMPP_BRIDGE_BIND` loopback-only unless a separate audited
network/auth boundary sits in front of the bridge.

## See also

- `ic/bridges/xmpp-bridge/README.md` — the bridge's own README (kept in sync
  with this reference).
- `ic/channels-src/xmpp/README.md` — the WASM channel package README.
- `ic/docs/LUNARWING_XMPP_TESTING.md` — isolated XMPP bridge test harness,
  local API smoke tests, live configuration wrappers, and generated user-systemd
  units.
- `ic/scripts/xmpp-configure.sh`, `ic/scripts/xmpp-rate-limit.sh` — live bridge
  configuration and rate-limit helpers.
