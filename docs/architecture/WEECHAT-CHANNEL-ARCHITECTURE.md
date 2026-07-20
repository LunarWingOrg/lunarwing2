# WeeChat Channel Architecture

How LunarWing connects to IRC through WeeChat: the components, the end-to-end message
flow, the ingestion/latency model, the configuration-precedence rules (and the trap they
create), and the known issues with their fix status.

> **Why this doc exists.** A multi-hour debugging session upgrading the `sunburst` tenant
> showed that message delivery through this channel has several independent, *invisible*
> failure modes, and that the "WebSocket adapter" was actually drained by slow HTTP polling
> (since replaced by a long-poll path, §3). Every incident re-derived the data flow and config
> rules from scratch. This is the authoritative reference so that doesn't happen again.

Line numbers below drift; treat them as hints, not contracts. Source of truth:
`lunarwing_weechat_wss/weechat_relay/src/lib.rs` (the WASM channel), `…/ws_adapter.py`
(the adapter), and `ic/src/channels/wasm/` (the host runtime).

---

## 1. Components

| Component | Where | Role |
|-----------|-------|------|
| **WeeChat** | per-tenant, runs in `tmux` (`lunarwing-weechat-<tenant>.service`) | The actual IRC client. Exposes the **relay `api`** plugin on the `weechat` port (MT: base+5). |
| **`ws_adapter.py`** | per-tenant Python process (`lunarwing-weechat-adapter-<tenant>.service`), `lunarwing_weechat_wss/weechat_relay/ws_adapter.py` | Holds a **WebSocket** to WeeChat's relay, subscribes to updates, buffers lines, and re-serves them over a small **HTTP API** on the `weechat_adapter` port (MT: base+9). |
| **WeeChat WASM channel** | in the LunarWing daemon; source `lunarwing_weechat_wss/weechat_relay/src/lib.rs` → `wasm32-wasip2`; loaded/run by `ic/src/channels/wasm/{loader,wrapper,runtime,setup}.rs` | Sandboxed channel that **long-polls (or polls) the adapter over HTTP**, applies policy, and emits `IncomingMessage`s to the agent; sends replies back to WeeChat. |

### The three hops — only one is a WebSocket

```
        WebSocket (+ /api/sync push)        HTTP long-poll (/api/wait while active;
WeeChat  ⇇———————————————————————————⇉  ws_adapter.py   host-clamped 30s callback schedule)
 relay   real-time, adapter-buffered      (:base+9)    ⇇——————————————————⇉  LunarWing daemon
(:base+5)                                                request/response       (WASM channel)
```

The **adapter↔WeeChat** hop is a real WebSocket and is real-time; the adapter even holds a
`/api/sync` subscription (`buffers`, `lines`). The **daemon↔adapter** hop is **HTTP** — the
sandboxed WASM can only make request/response calls (`channel_host::http_request`), so it cannot
hold a socket open. Instead of fixed-interval polling it issues a **blocking long-poll**
(`GET /api/wait`, §3) that returns the instant a line arrives, falling back to per-buffer polling
against an adapter that lacks the endpoint. The guest requests a 3s interval, but the host clamps
all WASM channel polling to at least 30s; §3 explains the resulting scheduling gap. "ws-adapter"
describes the *upstream* link, not the daemon's link.

**Implication:** IRC messages reach the adapter instantly and are buffered there. If a message
arrives while `/api/wait` is in flight, the daemon receives it within a network round-trip. After
an early return or a 20s heartbeat, however, the host's 30s interval can leave an idle window
before the next callback. Current end-to-end latency therefore ranges from near-immediate to
roughly one host poll interval; the fallback mode is likewise paced at 30s (see §3).

### Relay configuration ownership

The WeeChat relay configuration (`~/.config/weechat/relay.conf`) is generated automatically during
`add-tenant` via the supported WeeChat command interface. The password is stored as the literal
expression `${env:RELAY_PASSWORD}` in `relay.conf`; the resolved value is provided to the WeeChat
process through a dedicated, tenant-owned `env/weechat.env` file (mode `0600`) containing only
`RELAY_PASSWORD`. Relay IPv6 mode is disabled before the listener is bound to the IPv4 loopback
address `127.0.0.1`. The full tenant `lunarwing.env` is never loaded into the WeeChat process.

The adapter and daemon continue to source `RELAY_PASSWORD` from `lunarwing.env` through their
existing paths (capabilities-env bridge for the WASM channel, direct env for the adapter). The
explicit recovery command `configure-weechat-relay <tenant>` generates a missing relay configuration
using the same mechanism, with a preserve-and-fail guarantee: existing non-empty WeeChat
configuration is never overwritten.

---

## 2. End-to-end message flow

### Inbound (IRC → agent)

1. A message arrives in an IRC buffer in WeeChat (buffer name `irc.<network>.<target>`, e.g.
   `irc.sobes.#chan` or, for a DM/query, `irc.sobes.<nick>`).
2. The adapter (subscribed via WS/`/api/sync`) captures it, buffers it per-buffer, and appends it
   to a **global event log** with a monotonic `seq` (§3).
3. The WASM channel's `on_poll` runs (host-driven, §3) and dispatches by ingest mode:
   - **Long-poll (default):** `do_longpoll` issues `GET /api/wait?cursor=<n>` and receives the new
     lines across **all** buffers in one response (refreshing `/api/config` policy on heartbeat
     ticks, see §4).
   - **Poll (fallback):** `do_poll` refreshes `GET /api/config` (see §4) and then fetches
     `GET /api/buffers/<buf>/lines?limit=10` for each known buffer; `poll_buffer` keeps only lines
     with id > the per-buffer watermark in `state/last_seen_ids`.
4. `handle_inbound_line` filters and applies policy, in order:
   - **Tag filter (mirror-loop guard):** require the `irc_privmsg` tag; drop `self_msg`/`no_log`
     (`tags_allow_ingest`). This lives in `handle_inbound_line` so **both** ingest paths are covered
     — `do_longpoll` feeds lines straight here, and a `self_msg` slipping through is a mirror loop
     (the agent answers its own replies, which arrive as new lines, forever). The poll path's
     `poll_buffer` also pre-filters the same tags. See §5.
   - parse `irc.<net>.<target>` → `network_allowed` (allowlist; empty/`all`/`*` = all) →
     exclude-networks → non-empty text → **DM vs group** (`is_dm` = target not starting with
     `#`/`&`/`!`) → `dm_policy`/`group_policy` + `allow_from` / pairing store.
5. Survivors are emitted via `channel_host::emit_message`; the host (`wrapper.rs`) dispatches
   them to the agent. `on_poll completed … emitted_count=N` is logged.

### Outbound (agent → IRC)

`on_respond` parses the reply's `metadata_json` (buffer/network/target/nick), chunks the text
to `max_chunk_length` (default 420), and `POST`s each chunk to the WeeChat **relay** `/api/input`
(via `relay_url`, not the adapter). Replies therefore go straight to WeeChat.

Proactive `on_broadcast` delivery requires an explicit, network-qualified full buffer name:
`irc.<network>.<target>` (for example, `irc.libera.#lunarwing` or `irc.libera.alice`). Bare
nicks and channel names are rejected because they are ambiguous across networks. Group targets
use the full buffer with `/api/input`; DM targets retain the server-buffer `/msg` fallback used
by reactive replies. Proactive attachments are rejected explicitly rather than silently dropped.

Owner-scoped automatic target discovery is restricted to a configured owner actor. The legacy
numeric `wasm_channel_owner_ids` setting remains supported, while IRC deployments can use the
string `wasm_channel_owner_actor_ids` setting with a network-qualified account/nick principal.
Only matching owner traffic may update the persisted route, and WeeChat validates and stores the
complete `irc.<network>.<target>` buffer. Explicit delivery continues to require that same full
target. Autonomous notifications address the LunarWing owner scope, not the external actor
principal; the WASM wrapper then resolves that scope through the persisted full buffer. See
[`IRC-SENDER-IDENTITY.md`](IRC-SENDER-IDENTITY.md) for principal formats, threat boundaries, and
migration behavior.

### Watermarks & new buffers (poll mode only)

In the polling fallback, `do_poll` seeds a per-buffer watermark the **first time** it sees a
buffer and does **not** emit that first batch (avoids replaying history on startup). See §5 for the
consequence on freshly-created DM/query buffers. Long-poll mode has no per-buffer watermarks — it
tracks a single global cursor seeded to the adapter's current `event_cursor` at `on_start`, so it
neither replays history nor needs per-buffer discovery.

---

## 3. Ingestion & latency

The daemon consumes the adapter in one of two modes, chosen at `on_start` by probing
`GET /api/health` for an `event_cursor` field (`detect_and_seed_ingest_mode` in `lib.rs`).

### Long-poll mode (default when the adapter supports it)

The adapter keeps a **global ordered event log** (`event_seq` + `event_log`) and records every
`buffer_line_added` into it. A line names its buffer by `buffer_id`; for a **brand-new** query/DM
buffer not yet in the adapter's cached buffer list, the adapter refreshes the list
**synchronously and retries** before recording, so the *first* line in a new buffer is captured
rather than dropped (see §5 — that drop was the real first-DM bug, below both the cursor and the
WASM). Its blocking endpoint **`GET /api/wait?cursor=<n>&timeout=<s>`** returns immediately with
all events `seq > cursor`, or blocks until a line arrives (or a ~20s heartbeat). If
`cursor > event_seq` (the adapter restarted and its in-memory seq reset below the client) it
**replays the post-restart backlog** (treats the cursor as 0) instead of reporting "caught up", so
a line recorded right after a restart isn't skipped. `on_start` seeds the WASM's
`state/event_cursor` to the adapter's current cursor so buffered history isn't replayed at daemon
start.

`on_poll → do_longpoll`: issue `GET /api/wait` (HTTP timeout 25s), feed each returned line to
`handle_inbound_line`, and advance the cursor. The request returns as soon as a line is recorded,
so delivery is near-immediate **while that request is active**. The adapter captures a new
buffer's first line, so there is no separate buffer-discovery delay, but the host may not start the
next wait until its next 30s interval tick.

**Timeout hierarchy (hard constraint):** `adapter wait ≤20s  <  WASM HTTP 25s  <  host
callback_timeout 30s`. The host loop below is unchanged; each `on_poll` blocks in
`/api/wait` up to ~20s, returns, and waits for the next host interval tick. If `/api/wait` is missing (old
adapter) or returns an unparseable body, the WASM logs it and falls back to **poll mode** for the
rest of the session.

### Poll mode (fallback / old adapters) — 30s effective floor

Used when the adapter has no `/api/wait`. `on_poll → do_poll` fetches each buffer per cycle.

### The loop (`ic/src/channels/wasm/wrapper.rs`, `start_polling` / `execute_poll`)

Each channel gets its own polling task:

```
loop {
    interval_timer.tick().await;     // effective interval >=30s, MissedTickBehavior::Skip
    execute_poll().await;            // runs the WASM on_poll to completion
}
```

`tick` and `execute_poll` are **sequential**. The start-to-start cadence is at
least the poll interval, but a slow callback can cross scheduled ticks and
extend it further. `MissedTickBehavior::Skip` advances to a future tick instead
of running missed ticks back-to-back.

```
effective cadence  >=  poll_interval
```

- The WeeChat guest requests **3s** (`default_poll_interval()=3` and
  `.max(3000)` in `lib.rs`), and its capabilities file also declares 3000ms.
  The host ignores that lower floor: `MIN_POLL_INTERVAL_MS` is **30,000ms** in
  `ic/src/channels/wasm/capabilities.rs`, and schema conversion plus
  `validate_poll_interval()` clamp every channel to at least 30s.
- **`cycle_duration`** is bounded above by **`callback_timeout` = 30s**
  (`ic/src/channels/wasm/runtime.rs`), the `tokio::time::timeout` wrapping the WASM call.

The effective polling floor is therefore 30s. A callback that reaches the
timeout can extend the gap further; `MissedTickBehavior::Skip` prevents a burst
of catch-up calls afterward.

This same host loop drives **both** modes. In long-poll mode `cycle_duration` is
the up-to-20s `/api/wait` block. Once it returns, the task waits for the next
30s interval tick; an early event can therefore be followed by a substantial
idle window. In poll mode the cycle is usually short and the 30s host tick
paces it.

### Per-cycle cost (poll mode)

Everything inside one `on_poll` is **sequential**, each call with its own timeout. A fresh
WASM instance is also created per poll (`create_store` + `instantiate_component`; the runtime
"instantiates fresh per callback").

| Step | Call | Timeout (after fixes) | Frequency |
|------|------|----------------------:|-----------|
| Adapter health probe | `GET /api/version` | **1.5s** (was 2s) | every poll (auto/websocket mode) |
| Config refresh | `GET /api/config` | **2s** (was 3s) | every poll |
| Per-buffer lines | `GET /api/buffers/<buf>/lines` | **2s** (was 5s) | every poll, ×N buffers |
| Buffer-list refresh | `GET /api/buffers` | 5s | every ~30 polls, or when empty |

Worst-case cycle ≈ `1.5 + 2 + 2·N` s (was `2 + 3 + 5·N`). In the normal case
(responsive local adapter) each call returns in milliseconds, but the host
still paces callbacks at a minimum 30s interval.

### Seeing the real cadence (poll mode)

```bash
sudo -u <tenant> XDG_RUNTIME_DIR=/run/user/$(id -u <tenant>) \
  journalctl --user -u lunarwing-<tenant>.service -f \
  | grep --line-buffered "Polling .* buffers via"
```

The gap between consecutive lines is the actual cadence. To measure how long a single cycle
takes, compare `calling on_poll channel=weechat` → `on_poll completed channel=weechat`.

In **long-poll mode**, measure both the 30s host callback starts and the time
spent inside each callback. Liveness is also visible through the adapter's
`event_cursor` (`curl -s …/api/health`, §5) and `emitted_count` on the
`on_poll completed` lines.

> Channel debug logs are gated twice: by the `debug_logging` capability flag **and** by the
> daemon's `RUST_LOG`. See §5 — they used to be invisible at the default `RUST_LOG`.

---

## 4. Configuration & precedence

WeeChat config lives in several places; understanding the precedence is essential because a
stale value in a high-precedence layer silently overrides everything below it.

### At channel start (`load_channel_setup_field_overrides`, `ic/src/channels/wasm/setup.rs`)

For each capability `required_field`, the effective value is resolved **highest-first**:

| Precedence | Source | Notes |
|-----------:|--------|-------|
| 1 (highest) | DB `extensions.weechat.setup_fields` | Saved by the setup wizard / written by ops. |
| 2 | DB `setting_path` | If the field declares one. |
| 3 | `env` (capability `env` key) | **Gated to bundled channels** for security. Multi-tenant ports come from here (`RELAY_URL`, `WS_ADAPTER_URL`). |
| 4 (lowest) | capabilities `config` block | Defaults shipped in `weechat.capabilities.json`. |

Resolved overrides are merged on top of the caps `config` block and handed to `on_start`,
which persists them to channel workspace state (`state/relay_url`, `state/dm_policy`, …).

The shipped caps default for `dm_policy` is `pairing`: an unpaired sender gets pairing
instructions and does not execute under owner scope. Operators who want open DMs must set
`"dm_policy":"open"` explicitly in the adapter config (`weechat_local_config.json`),
the DB `setup_fields` row, or the setup wizard. Unknown `dm_policy` strings fail closed
(reject the sender with a warning) rather than treating them as `open`.

### At runtime (`refresh_policy_config` → `/api/config`)

On each poll cycle, `refresh_policy_config` fetches `GET /api/config` from the adapter (served from
`weechat_local_config.json` next to `ws_adapter.py`) and, if present, overwrites the workspace
state for `dm_policy`, `group_policy`, `allow_from`, and `networks`. Both ingest paths call it —
`do_poll` every cycle, `do_longpoll` on each `/api/wait` return (event batch or ~20s heartbeat).
Ports (`relay_url`/`ws_adapter_url`) are **not** refreshed this way — they are set only at
`on_start`.

So the live precedence is:

- **Ports:** `setup_fields` > `setting_path` > `env` > caps defaults (start-time only).
- **Policy (`dm_policy`/`group_policy`/`allow_from`/`networks`):** adapter `/api/config` (if it
  provides the key) > start-time value (above).

### ⚠ The shadowing trap

A leftover `extensions.weechat.setup_fields` row **shadows the caps config and the env for
every field it contains**. The classic symptom: *"I edited the installed `capabilities.json`
(or the env) and nothing changed."* This caused hours of confusion on `sunburst` — stale
`dm_policy` and `networks` values in that row overrode every edit.

Inspect and clear it (the `value` column is **`jsonb`**):

```bash
PGURL=$(sudo grep '^DATABASE_URL=' /home/<tenant>/lunarwing/env/lunarwing.env | cut -d= -f2-)
# inspect
sudo env PGSSLMODE=disable psql "$PGURL" -c \
  "SELECT value FROM settings WHERE key='extensions.weechat.setup_fields';"
# clear (caps + env then govern) OR surgically fix one key:
sudo env PGSSLMODE=disable psql "$PGURL" -c \
  "DELETE FROM settings WHERE key='extensions.weechat.setup_fields';"
sudo env PGSSLMODE=disable psql "$PGURL" -c \
  "UPDATE settings SET value = value || '{\"dm_policy\":\"pairing\"}'::jsonb \
   WHERE key='extensions.weechat.setup_fields';"
```

> **Note:** WeeChat's new-install `dm_policy` default is now `pairing` (previously `pairing` was the documented safe default while the implementation defaulted to `open`; see rationale in this section). Operators who intentionally want open DMs should set `"dm_policy":"open"` explicitly in their adapter config or `setup_fields` row.

Then restart the daemon so `on_start` re-resolves.

---

## 5. Known issues & gotchas

| Issue | Symptom | Root cause | Status |
|-------|---------|------------|--------|
| **`networks="all"` matched literally** | Every message dropped: `line dropped (network not in allowlist): network=…, allowed=["all"]` | The allowlist compared names literally; `"all"` matched no real network. Convention was *empty = all*, but `"all"` is the obvious thing to type. | **Fixed** — `network_allowed()` treats `all`/`*` as wildcards (empty still = all). |
| **Channel debug logs invisible** | `debug_logging=true` produced nothing in the journal | The host forwarded all guest `Info/Debug/Trace` logs via `tracing::debug!`, dropped by the default `RUST_LOG=lunarwing=info`. | **Fixed** — faithful level mapping (`Info→info!`, `Trace→trace!`); `debug_logging` is now visible at `info`. |
| **Guest 3s interval is clamped to 30s** | A message can wait after an early `/api/wait` return even though the adapter already buffered it | The guest and capabilities file request 3000ms, but host `MIN_POLL_INTERVAL_MS=30000` wins during schema conversion and startup validation. | **Open / documented** — long-poll is immediate only while a wait is active. `MissedTickBehavior::Skip` prevents catch-up bursts but does not remove the host floor. |
| **`poll_interval_ms` ignored** | Configuring the interval did nothing | Caps `config` key was `poll_interval_ms` but the struct field is `poll_interval_seconds` — different name ⇒ value dropped, struct default (3) used. | **Fixed** — caps key renamed to `poll_interval_seconds`. |
| **First DM in a new buffer swallowed** | First message after a query buffer is created never reaches the agent; the *second* does | A DM/query buffer is created *by* the first message. The **adapter** resolves a line's buffer by `buffer_id` against a cached `buffer_list` that doesn't include the new buffer yet, so it **dropped the first line entirely** — never recorded to `line_buffer` *or* the event log. Nothing downstream (neither the long-poll cursor nor the poll path) can deliver a line the adapter never recorded. (The poll path additionally seed-skipped a new buffer's first batch.) | **Fixed** — the adapter now refreshes its buffer list **synchronously and retries** before recording, so a new buffer's first line is captured (`record_event`); the long-poll global cursor then delivers it immediately (§3). `/api/wait` also replays the post-restart backlog so a DM right after an adapter restart isn't skipped. The poll fallback still emits the first batch for new **DM/query** buffers (`is_dm_buffer`). |
| **Mirror loop (long-poll mode)** | Agent answers its own messages endlessly; `event_cursor` climbs steadily with no human input | The `irc_privmsg`/`self_msg`/`no_log` tag filter lived **only** in `poll_buffer`. `do_longpoll` feeds events straight to `handle_inbound_line`, which had no tag check — so in long-poll mode the agent's own `self_msg` replies were ingested and re-answered, each reply becoming the next event. Shipped in the original long-poll commit; not the adapter work. | **Fixed** — tag filter moved into `handle_inbound_line` (`tags_allow_ingest`), the single choke point **both** ingest paths share; `poll_buffer` keeps its pre-filter. Regression test `test_tags_allow_ingest`. |
| **Password is the *second* blocker** | After ports are fixed, the adapter returns 401 | The adapter authenticates incoming WASM requests against the per-tenant `RELAY_PASSWORD` (`check_auth`); the WASM must send it. | Handled by the port fix (relay_password injection) — see archived `WEECHAT-MULTITENANT-PORT-BUG.md` in `docs/internal/history/archive/ops/`. |
| **Stale `setup_fields` shadowing** | Caps/env edits "don't take" | Highest-precedence DB layer (§4). | Documented (§4); see §6 for the proposed precedence redesign. |

### How to actually see what's happening

- Channel logs: set `debug_logging=true` (caps `config`) — now visible at `RUST_LOG=lunarwing=info`.
  For per-poll `Debug` lines, use `RUST_LOG=lunarwing=info,lunarwing::channels::wasm=debug`.
- Adapter health: `curl -s http://127.0.0.1:<base+9>/api/health` → `ws_connected: true` means the
  adapter↔WeeChat WebSocket is live; `event_cursor` is the global long-poll cursor and should climb
  as IRC lines arrive (its presence is also what makes the WASM choose long-poll over polling).
- Raw line tags (bypasses channel logging): `curl` `…/api/buffers/<buf>/lines` with
  `Authorization: Basic base64("plain:"+RELAY_PASSWORD)`.

---

## 6. Recommendations

**Applied in this change (P0/P1):**

- **Long-poll ingestion:** adapter global event log + blocking `GET /api/wait`;
  WASM `do_longpoll` consuming it with a capability probe and automatic fallback to per-buffer
  polling against old adapters (`ws_adapter.py`, `lib.rs`, with a `parse_wait_response` test).
  This replaces per-buffer fan-out with one blocking request. It provides immediate delivery while
  the request is active, but the current 30s host schedule leaves gaps between callbacks (§3).
- Faithful guest-log level mapping (`wrapper.rs`).
- `MissedTickBehavior::Skip` + tightened per-call timeouts (`wrapper.rs`, `lib.rs`) prevent
  catch-up bursts and bound a stalled fallback cycle; the host still enforces a 30s minimum
  interval.
- `network_allowed()` wildcard for `all`/`*` (`lib.rs`, with a regression test).
- `poll_interval_seconds` caps key fix (`weechat.capabilities.json`).
- **First-DM delivery (real fix is at the adapter):** the adapter refreshes its buffer list
  **synchronously and retries** before recording, so a new buffer's first line is captured into the
  event log instead of dropped (`ws_adapter.py`); the long-poll cursor then delivers it, and
  `/api/wait` replays the post-restart backlog. The poll fallback still emits the first batch for
  new **DM/query** buffers (`is_dm_buffer`, `lib.rs`, with a regression test).
- **Mirror-loop guard:** the `irc_privmsg`/`self_msg`/`no_log` tag filter now lives in
  `handle_inbound_line` (`tags_allow_ingest`) — the choke point **both** ingest paths share — so the
  long-poll path can no longer re-ingest the agent's own `self_msg` replies (`lib.rs`, with
  `test_tags_allow_ingest`).

**Open / recommended next:**

- **P1 — Reconcile long-poll scheduling with the host minimum.** Either give trusted bundled
  long-poll channels a supervised continuous-wait loop or lower the minimum safely. Until then,
  do not describe the daemon-to-adapter path as continuously near real-time.
- **P2 — Batched "all new lines" endpoint for the fallback path.** `/api/wait` already returns all
  new lines across buffers in one call, so the long-poll path no longer fans out per buffer; only
  the polling fallback still issues N sequential `/lines` fetches. Low priority now.
- **P2 — Config-precedence redesign:** make `env`-declared deployment fields win over a stale
  `setup_fields` row (or have `mt-admin patch-env`/upgrade clear stale weechat `setup_fields`),
  and extend `ic/scripts/lunarwing-weechat-preflight.sh` to surface the *effective* value
  (DB `setup_fields` + workspace state), not just the env file.

---

## 7. Rollout note

These changes split across three artifacts:

- **Host** (log mapping, `MissedTickBehavior`): `cargo build --release --bin lunarwing` → deploy binary.
- **WASM channel** (long-poll consumer, **mirror-loop tag filter `tags_allow_ingest`**, per-call
  HTTP timeouts, `network_allowed`, caps key): `scripts/build-wasm-extensions.sh` →
  `lunarwing-mt-admin.sh install-wasm <tenant>`.
- **Adapter** (`ws_adapter.py`: `/api/wait`, global event log, `/api/health` cursor, **first-line
  capture for new buffers**, **post-restart replay**): a plain Python file that runs from the
  **tenant's own clone**, so it ships with a `git pull` in the tenant home and takes effect when
  its service restarts.

The adapter and WASM must update **together** (the WASM probes `/api/health` for the adapter's
`event_cursor` and only long-polls if present; otherwise it falls back to polling). Per tenant:
`git pull` in the tenant home → `build-tenant <t> --with-wasm` → `install-wasm <t>` →
`restart-tenant <t>` (restarts both the daemon and the adapter service). New tenants created via
`create-tenant-*` get all three from the start. No behavior changes until they are redeployed.

---

## Cross-references

- `docs/ops/WEECHAT-SERVICES.md` — services, ports, env vars, day-to-day ops, automatic relay
  bootstrap, and recovery.
- `WEECHAT-MULTITENANT-PORT-BUG.md` — the per-tenant port/password fix and the
  env-sourced-fields mechanism (archived to `docs/internal/history/archive/ops/`).
- `docs/proposals/WEECHAT_WS_ADAPTER_SYNC_PROTOCOL.md`,
   `docs/proposals/WEECHAT_WS_ADAPTER_MISSING_DEPENDENCY_AND_AUTOMATION.md` — the adapter sync protocol,
   dependency + automation.
- `ic/scripts/lunarwing-weechat-preflight.sh` — read-only pre-flight that validates the generated
  relay configuration and dedicated minimal env without sourcing either file.
- Code: `lunarwing_weechat_wss/weechat_relay/src/lib.rs`, `…/ws_adapter.py`,
  `ic/src/channels/wasm/{setup,wrapper,runtime,loader}.rs`.
