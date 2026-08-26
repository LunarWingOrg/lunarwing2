#!/usr/bin/env python3
# Improved by Baud on June 1 2026
"""
WeeChat WebSocket Adapter for LunarWing

Maintains a persistent WebSocket connection to WeeChat's relay API and
exposes a local HTTP API (mirroring WeeChat's REST format) that the
LunarWing WASM channel can poll. This gives real-time message delivery
(WebSocket latency) while the WASM still uses simple HTTP requests.

Architecture:
    WASM (poll every 3s) --> HTTP GET /api/buffers/<name>/lines --> this adapter
    this adapter <--> WebSocket <--> WeeChat relay
    WASM (send response) --> HTTP POST /api/input --> WeeChat (direct)

Reconnect behavior (v0.3.0):
    - aiohttp WSMsgType.ERROR frames are logged at WARNING with the exception
      payload and surfaced via /api/health (last_disconnect, reconnect_count)
      instead of looking like a routine disconnect.
    - After every (re)connect, recover_after_connect() backfills lines the
      relay accepted while the WS was down (WeeChat does not replay sync
      events); per-buffer line-id watermarks dedupe against live events.
    - Outbound /api/input proxy failures are queued in a bounded outbox and
      retried, in order, after the next successful connect.

Usage:
    python3 ws_adapter.py [--relay-url URL] [--relay-password PW] [--port PORT]
                         [--message-delay SECONDS]

    Or via environment variables:
    RELAY_URL=http://127.0.0.1:9001 RELAY_PASSWORD=secret python3 ws_adapter.py
    WEECHAT_ADAPTER_PORT=10009 python3 ws_adapter.py
    MESSAGE_DELAY_SECONDS=3 python3 ws_adapter.py  # add 3s delay to all messages

Dependencies:
    pip install aiohttp

WeeChat setup:
    /relay add api 9001
    /set relay.network.password "secret"
    /set relay.network.bind_address "127.0.0.1"
"""
import argparse
import asyncio
import base64
import json
import logging
import os
import sys
from collections import defaultdict, deque
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional
from urllib.parse import quote, unquote

try:
    import aiohttp
    from aiohttp import web, ClientSession, WSMsgType
except ImportError:
    print("Missing dependency: pip install aiohttp", file=sys.stderr)
    sys.exit(1)

log = logging.getLogger("weechat_ws")

VERSION = "0.3.0"
MAX_LINES_PER_BUFFER = 500  # Max buffered lines per IRC buffer
RECONNECT_DELAY_S = 5  # Backoff between WebSocket reconnect attempts
BACKFILL_LIMIT = 100  # Lines fetched per buffer when recovering after a reconnect
MAX_OUTBOX = 100  # Max queued outbound /api/input bodies awaiting retry


# ---------------------------------------------------------------------------
# Shared state (lives in the long-running adapter process)
# ---------------------------------------------------------------------------

class AdapterState:
    def __init__(self):
        # Recent lines per buffer: {full_name: deque of LineInfo dicts}
        self.line_buffer: dict[str, deque] = defaultdict(lambda: deque(maxlen=MAX_LINES_PER_BUFFER))
        # Cached buffer list from WeeChat
        self.buffer_list: list = []
        # WebSocket connection status
        self.ws_connected: bool = False
        self.ws_error: Optional[str] = None
        # Last disconnect (type/reason/timestamp) + counters, for /api/health.
        # Lets the host/health system see WHY the adapter is reconnecting instead
        # of a bare connected/disconnected flap.
        self.last_disconnect: Optional[dict] = None
        self.reconnect_count: int = 0
        self.ever_connected: bool = False
        # Per-buffer high-water mark of WeeChat line ids we have recorded.
        # Used to dedupe WS events against the reconnect backfill and to fetch
        # only lines newer than what we've already seen.
        self.last_line_id: dict[str, int] = {}
        # Outbound /api/input bodies buffered while the relay was unreachable,
        # flushed (in order) after the next successful connect.
        self.outbox: deque = deque()
        self.outbox_dropped: int = 0
        # WeeChat version info (from /api/version)
        self.weechat_version: str = "unknown"
        self.relay_api_version: str = "unknown"
        # Relay config (set at startup)
        self.relay_url: str = ""
        self.relay_password: str = ""
        # Message delay in seconds (0 = disabled)
        self.message_delay_s: float = 0.0
        # Global ordered event log powering the /api/wait long-poll path.
        # Each entry: {"seq": int, "full_name": str, "line": dict}
        self.event_seq: int = 0
        self.event_log: deque = deque(maxlen=2000)
        # asyncio.Event signalling new lines; created in run() (needs a loop).
        self.new_event = None

#ii
state = AdapterState()


# ---------------------------------------------------------------------------
# Auth helpers
# ---------------------------------------------------------------------------

def make_auth_header(password: str) -> str:
    token = base64.b64encode(f"plain:{password}".encode()).decode()
    return f"Basic {token}"


def check_auth(request: web.Request) -> bool:
    """Verify the Authorization header matches the configured relay password."""
    if not state.relay_password:
        return True  # No password configured — allow all local requests
    auth = request.headers.get("Authorization", "")
    expected = make_auth_header(state.relay_password)
    return auth == expected


# ---------------------------------------------------------------------------
# WeeChat HTTP helpers (used by adapter internals)
# ---------------------------------------------------------------------------

async def weechat_get(session: ClientSession, path: str, timeout: float = 5.0):
    """GET from WeeChat relay API."""
    url = f"{state.relay_url.rstrip('/')}{path}"
    auth = make_auth_header(state.relay_password)
    try:
        async with session.get(url, headers={"Authorization": auth}, timeout=aiohttp.ClientTimeout(total=timeout)) as resp:
            return resp.status, await resp.read()
    except Exception as e:
        return None, str(e).encode()


async def refresh_buffer_list(session: ClientSession):
    """Fetch current buffer list and version from WeeChat HTTP API."""
    status, body = await weechat_get(session, "/api/buffers")
    if status == 200:
        try:
            data = json.loads(body)
            # WeeChat v2 returns a bare array
            if isinstance(data, list):
                state.buffer_list = data
            elif isinstance(data, dict):
                state.buffer_list = data.get("buffers", [])
            log.debug(f"Refreshed buffer list: {len(state.buffer_list)} buffers")
        except json.JSONDecodeError as e:
            log.warning(f"Failed to parse buffer list: {e}")

    status, body = await weechat_get(session, "/api/version", timeout=3.0)
    if status == 200:
        try:
            data = json.loads(body)
            state.weechat_version = data.get("weechat_version", "unknown")
            state.relay_api_version = data.get("relay_api_version", "unknown")
        except json.JSONDecodeError:
            pass


# ---------------------------------------------------------------------------
# WebSocket client loop
# ---------------------------------------------------------------------------

def note_disconnect(kind: str, reason: str):
    """Record why the WS dropped so /api/health can surface the real cause."""
    state.last_disconnect = {
        "type": kind,
        "reason": reason,
        "at": datetime.now(timezone.utc).isoformat(),
    }
    state.reconnect_count += 1


async def ws_client_loop():
    """
    Connect to WeeChat's WebSocket relay, subscribe to buffer events, and
    process incoming messages. Reconnects automatically on disconnect.
    """
    relay_url = state.relay_url.rstrip("/")
    ws_url = relay_url.replace("http://", "ws://").replace("https://", "wss://") + "/api"
    auth = make_auth_header(state.relay_password)
    attempt = 0
    backfill_task: Optional[asyncio.Task] = None

    while True:
        attempt += 1
        backfill_task = None
        try:
            async with ClientSession() as session:
                log.info(f"Connecting to WS at {ws_url} (attempt {attempt})")
                async with session.ws_connect(
                    ws_url,
                    headers={"Authorization": auth},
                    heartbeat=25,  # send WS pings so silent/half-open drops are detected
                    max_msg_size=0,  # No size limit
                ) as ws:
                    state.ws_connected = True
                    state.ws_error = None

                    # Seed the buffer list and version info first so we can log the banner
                    await refresh_buffer_list(session)
                    log.info(f"WS connected — relay {state.weechat_version}, API v{state.relay_api_version}")

                    # Subscribe to buffer_line_added events
                    subscribe = {
                        "request_id": "lunarwing-sync",
                        "request": "POST /api/sync",
                        "body": {
                            "sync": {
                                "nicks": False,
                                "input": False,
                                "colors": False,
                                "buffers": True,
                                "lines": True,
                            }
                        },
                    }
                    await ws.send_str(json.dumps(subscribe))
                    log.debug("Sent sync subscription")

                    # Recover lines that arrived while we were disconnected (and
                    # flush any queued outbound messages). Runs concurrently with
                    # the read loop; record_line dedupes the overlap by line id.
                    is_reconnect = state.ever_connected
                    state.ever_connected = True
                    backfill_task = asyncio.create_task(recover_after_connect(session, is_reconnect))

                    async for msg in ws:
                        if msg.type == WSMsgType.TEXT:
                            await handle_ws_event(msg.data, session)
                        elif msg.type == WSMsgType.BINARY:
                            log.debug("Ignoring binary WS message")
                        elif msg.type == WSMsgType.ERROR:
                            # An ERROR frame means the connection FAILED — it is
                            # not a clean close. Log the actual exception/payload
                            # at WARNING so the failure is visible, and expose it
                            # via /api/health (previously this was logged at INFO
                            # with no payload and looked like a normal disconnect).
                            err = ws.exception()
                            reason = str(err or msg.data)
                            state.ws_error = reason
                            note_disconnect("error", reason)
                            log.warning(
                                f"WS error frame from relay — reconnecting in "
                                f"{RECONNECT_DELAY_S}s ({reason})"
                            )
                            break
                        elif msg.type == WSMsgType.CLOSED:
                            note_disconnect("closed", str(msg.data))
                            log.info(f"WS closed by peer — reconnecting in {RECONNECT_DELAY_S}s")
                            break

                    state.ws_connected = False

        except asyncio.CancelledError:
            log.info("WebSocket loop cancelled")
            if backfill_task is not None and not backfill_task.done():
                backfill_task.cancel()
            return
        except Exception as e:
            state.ws_connected = False
            state.ws_error = str(e)
            note_disconnect("exception", str(e))
            log.warning(f"WS disconnected — reconnecting in {RECONNECT_DELAY_S}s ({e})")

        if backfill_task is not None and not backfill_task.done():
            backfill_task.cancel()
        await asyncio.sleep(RECONNECT_DELAY_S)


def record_event(full_name: str, line: dict):
    """Append a line to the global event log and wake any /api/wait waiter."""
    state.event_seq += 1
    state.event_log.append({"seq": state.event_seq, "full_name": full_name, "line": line})
    if state.new_event is not None:
        state.new_event.set()


def record_line(full_name: str, line_info: dict) -> bool:
    """Append a line to the buffer cache and event log, deduping by line id.

    WeeChat line ids increase monotonically within a buffer, so a per-buffer
    high-water mark both dedupes live WS events against the reconnect backfill
    and tells the backfill which lines are new. Lines without an id cannot be
    deduped and are always recorded (the pre-watermark behavior).

    Returns True if the line was recorded, False if it was a duplicate.
    """
    line_id = line_info.get("id")
    if isinstance(line_id, int):
        last = state.last_line_id.get(full_name)
        if last is not None and line_id <= last:
            return False
        state.last_line_id[full_name] = line_id
    state.line_buffer[full_name].append(line_info)
    record_event(full_name, line_info)
    return True


async def recover_after_connect(session: ClientSession, is_reconnect: bool):
    """Post-connect recovery: seed line-id watermarks and backfill missed lines.

    On the FIRST connect after adapter start, only seed each buffer's
    high-water mark from the relay's newest line — recording history here
    would replay old lines to the agent. On RECONNECTS, fetch recent lines
    per buffer and record only those newer than the watermark, recovering the
    lines the relay accepted while the WebSocket was down (sync events are
    NOT replayed by WeeChat after a reconnect, so without this those lines
    are silently lost). Buffers without a watermark — e.g. a query buffer
    opened during the outage — have their recent lines recorded in full.
    """
    # Relay is reachable again — retry any outbound messages queued during
    # the outage before recovering inbound lines.
    await flush_outbox()
    recovered = 0
    for buf in list(state.buffer_list):
        full_name = buf.get("full_name") or buf.get("name")
        if not full_name:
            continue
        path = f"/api/buffers/{quote(full_name, safe='')}/lines?limit={BACKFILL_LIMIT}"
        status, body = await weechat_get(session, path)
        if status != 200:
            log.debug(f"backfill: GET {path} → status {status} (skipping buffer)")
            continue
        try:
            lines = json.loads(body)
        except json.JSONDecodeError:
            log.debug(f"backfill: unparseable lines response for {full_name}")
            continue
        if not isinstance(lines, list):
            continue
        # WeeChat returns newest-first; process oldest-first, id-bearing only.
        id_lines = [l for l in lines if isinstance(l, dict) and isinstance(l.get("id"), int)]
        id_lines.sort(key=lambda l: l["id"])
        if not id_lines:
            continue
        if not is_reconnect:
            state.last_line_id[full_name] = id_lines[-1]["id"]
            continue
        for line in id_lines:
            if record_line(full_name, line):
                recovered += 1
    if is_reconnect and recovered:
        log.info(f"Recovered {recovered} line(s) missed during the disconnect window")


async def handle_ws_event(raw: str, session: ClientSession):
    """Parse and dispatch a single WebSocket event from WeeChat."""
    try:
        msg = json.loads(raw)
    except json.JSONDecodeError:
        log.debug(f"Non-JSON WS frame: {raw[:80]}")
        return

    # WeeChat relay API v0.4.1 uses "event_name", "buffer_id", and "body"
    event = msg.get("event_name") or msg.get("event") or msg.get("id")
    data = msg.get("body") or msg.get("data") or {}
    buffer_id = msg.get("buffer_id")
    log.debug(f"WS event: event={event!r} buffer_id={buffer_id!r} body_preview={str(data)[:80]}")

    if event == "buffer_line_added":
        # API v0.4.1: body IS the line object; buffer identified by top-level buffer_id
        line_info = data if isinstance(data, dict) else {}
        # Look up buffer full_name from buffer_id
        full_name = None
        if buffer_id is not None:
            buf = next((b for b in state.buffer_list if b.get("id") == buffer_id), None)
            if buf:
                full_name = buf.get("full_name") or buf.get("name")
        if full_name and line_info:
            tags = line_info.get("tags_array", line_info.get("tags", []))
            nick = next((t[5:] for t in tags if t.startswith("nick_")), "?")
            preview = str(line_info.get("message", ""))[:40]
            log.debug(f"buffer_line_added: buffer={full_name} nick={nick} msg={preview!r}")
            if state.message_delay_s > 0:
                await asyncio.sleep(state.message_delay_s)
            record_line(full_name, line_info)
        else:
            # Unknown buffer — almost always a brand-new query/DM buffer that isn't
            # in our cached buffer_list yet. Refresh SYNCHRONOUSLY and retry the
            # lookup so the FIRST line in a new buffer isn't dropped. That drop is
            # the long-standing "first DM swallowed" bug: the line never reaches
            # line_buffer OR the event log, so neither polling nor the long-poll
            # cursor can ever deliver it. record_event must see it here.
            log.debug(f"buffer_line_added: unknown buffer_id={buffer_id!r} — refreshing buffer list inline")
            await refresh_buffer_list(session)
            buf = next((b for b in state.buffer_list if b.get("id") == buffer_id), None)
            full_name = (buf.get("full_name") or buf.get("name")) if buf else None
            if full_name and line_info:
                record_line(full_name, line_info)
                log.info(f"buffer_line_added: captured first line for new buffer {full_name}")
            else:
                log.warning(f"buffer_line_added: buffer_id={buffer_id!r} still unknown after refresh — dropping line")

    elif event == "buffer_opened":
        asyncio.create_task(refresh_buffer_list(session))
        log.debug(f"Buffer opened: {data.get('full_name', data.get('name', '?'))} — refreshing buffer list")

    elif event == "buffer_title_changed":
        # Fired when a buffer is created or its title changes; body contains full buffer info
        buf_id = data.get("id")
        buf_name = data.get("full_name") or data.get("name")
        if buf_id and buf_name:
            existing = next((b for b in state.buffer_list if b.get("id") == buf_id), None)
            if existing:
                existing.update(data)
            else:
                state.buffer_list.append(data)
                log.debug(f"buffer_title_changed: registered new buffer {buf_name} (id={buf_id})")

    elif event == "buffer_closed":
        full_name = data.get("full_name") or data.get("name")
        if full_name and full_name in state.line_buffer:
            del state.line_buffer[full_name]
        if full_name:
            state.last_line_id.pop(full_name, None)
        asyncio.create_task(refresh_buffer_list(session))

    elif msg.get("request_id") == "lunarwing-sync":
        # Subscription acknowledgement
        code = msg.get("code", 0)
        log.info(f"Sync subscription acknowledged: code={code} full={msg!r}")

    else:
        log.debug(f"Unhandled WS event: {event!r} keys={list(msg.keys())}")


# ---------------------------------------------------------------------------
# HTTP API handlers (mirror WeeChat REST API format)
# ---------------------------------------------------------------------------

async def handle_version(request: web.Request) -> web.Response:
    return web.json_response({
        "weechat_version": state.weechat_version,
        "relay_api_version": state.relay_api_version,
        "adapter_version": VERSION,
        "ws_connected": state.ws_connected,
    })


async def handle_config(request: web.Request) -> web.Response:
    """GET /api/config — serve local channel config for the WASM to read on startup."""
    config_path = Path(__file__).parent / "weechat_local_config.json"
    try:
        with open(config_path) as f:
            data = json.load(f)
        return web.json_response(data)
    except FileNotFoundError:
        return web.json_response({})
    except Exception as e:
        log.warning(f"Failed to read weechat_local_config.json: {e}")
        return web.json_response({})


async def handle_health(request: web.Request) -> web.Response:
    return web.json_response({
        "status": "ok" if state.ws_connected else "degraded",
        "ws_connected": state.ws_connected,
        "ws_error": state.ws_error,
        # Why the last disconnect happened (WSMsgType.ERROR payload, clean close,
        # or exception) and how often — lets the health system distinguish a real
        # relay failure from a routine reconnect.
        "last_disconnect": state.last_disconnect,
        "reconnect_count": state.reconnect_count,
        # Outbound messages buffered while the relay was unreachable.
        "outbox_depth": len(state.outbox),
        "outbox_dropped": state.outbox_dropped,
        "buffered_buffers": len(state.line_buffer),
        "buffer_list_count": len(state.buffer_list),
        # Current global event cursor. Its presence also tells the WASM channel
        # that this adapter supports the /api/wait long-poll path.
        "event_cursor": state.event_seq,
    })


async def handle_wait(request: web.Request) -> web.Response:
    """GET /api/wait?cursor=<n>&timeout=<s> — long-poll for new lines.

    Returns {"cursor": int, "events": [{"seq","full_name","line"}, ...]}.
    Blocks until there are events with seq > cursor, or a heartbeat timeout.
    """
    if not check_auth(request):
        return web.json_response({"error": "Unauthorized"}, status=401)

    try:
        cursor = int(request.rel_url.query.get("cursor", "0"))
    except ValueError:
        cursor = 0
    try:
        wait_s = float(request.rel_url.query.get("timeout", "20"))
    except ValueError:
        wait_s = 20.0
    wait_s = max(0.0, min(wait_s, 20.0))  # cap below the host's 30s callback timeout

    # Stale cursor (cursor > our seq) means the adapter restarted and its in-memory
    # event_seq reset below the client's saved cursor. Don't tell the client it's
    # already caught up — that would skip every line recorded since the restart
    # (e.g. the first DM after a setup restart). Replay from 0 instead; the
    # post-restart event_log only holds lines the client hasn't seen, so this can't
    # double-deliver. (Falls through to the normal collect/wait logic below.)
    if cursor > state.event_seq:
        log.info(f"/api/wait: client cursor {cursor} > event_seq {state.event_seq} "
                 f"(adapter restarted?) — replaying post-restart backlog")
        cursor = 0

    def collect():
        return [e for e in state.event_log if e["seq"] > cursor]

    events = collect()
    if not events and wait_s > 0 and state.new_event is not None:
        # Single waiter per tenant adapter: clear, re-check, then wait, so a line
        # arriving in the gap is never missed. Correctness comes from the cursor
        # re-scan, not the flag.
        state.new_event.clear()
        events = collect()
        if not events:
            try:
                await asyncio.wait_for(state.new_event.wait(), timeout=wait_s)
            except asyncio.TimeoutError:
                pass
            events = collect()

    cursor_out = events[-1]["seq"] if events else state.event_seq
    return web.json_response({"cursor": cursor_out, "events": events})


async def handle_buffers(request: web.Request) -> web.Response:
    if not check_auth(request):
        return web.json_response({"error": "Unauthorized"}, status=401)
    log.debug(f"GET /api/buffers → {len(state.buffer_list)} buffers")
    # Return bare array (WeeChat v2 format)
    return web.json_response(state.buffer_list)


async def handle_single_buffer(request: web.Request) -> web.Response:
    """GET /api/buffers/{name}?lines=-N — return buffer object with embedded lines (WeeChat format)."""
    if not check_auth(request):
        return web.json_response({"error": "Unauthorized"}, status=401)

    raw_name = request.match_info.get("buffer_name", "")
    buffer_name = unquote(raw_name)

    lines_param = request.rel_url.query.get("lines", "")
    limit = abs(int(lines_param)) if lines_param else 50

    # Find buffer info
    buf_info = next((b for b in state.buffer_list if b.get("name") == buffer_name or b.get("full_name") == buffer_name), None)
    result = dict(buf_info) if buf_info else {"name": buffer_name}

    buf = state.line_buffer.get(buffer_name)
    if buf:
        lines = list(buf)[-limit:]
        result["lines"] = list(reversed(lines))
    else:
        result["lines"] = []

    log.debug(f"GET /api/buffers/{buffer_name} → {len(result['lines'])} lines")
    return web.json_response(result)


async def handle_buffer_lines(request: web.Request) -> web.Response:
    if not check_auth(request):
        return web.json_response({"error": "Unauthorized"}, status=401)

    # Buffer name is URL-encoded in the path (# → %23)
    raw_name = request.match_info.get("buffer_name", "")
    buffer_name = unquote(raw_name)

    # Support both ?limit=N (legacy) and ?lines=-N (WeeChat API convention)
    lines_param = request.rel_url.query.get("lines", "")
    if lines_param:
        limit = abs(int(lines_param))
    else:
        limit = int(request.rel_url.query.get("limit", "50"))

    buf = state.line_buffer.get(buffer_name)
    if buf is None:
        log.debug(f"GET /api/buffers/{buffer_name}/lines → 0 lines buffered (unknown buffer)")
        # Unknown buffer — return empty (WASM handles this gracefully)
        return web.json_response([])

    # Return newest-first (WeeChat API default), capped at limit
    lines = list(buf)[-limit:]
    lines_newest_first = list(reversed(lines))
    log.debug(f"GET /api/buffers/{buffer_name}/lines → {len(buf)} lines buffered, returning {len(lines_newest_first)}")
    return web.json_response(lines_newest_first)


def enqueue_outbox(body: dict) -> int:
    """Buffer an outbound /api/input body for retry after the relay recovers.

    Bounded at MAX_OUTBOX; when full the oldest entry is dropped (newer
    messages are more valuable) and the drop is counted for /api/health.
    Returns the resulting queue depth.
    """
    if len(state.outbox) >= MAX_OUTBOX:
        state.outbox.popleft()
        state.outbox_dropped += 1
        log.warning(f"Outbox full ({MAX_OUTBOX}) — dropped oldest queued message")
    state.outbox.append(body)
    return len(state.outbox)


async def flush_outbox():
    """Best-effort ordered flush of outbound messages queued while the relay
    was unreachable. Stops at the first connection failure, preserving order
    for the next attempt; entries that get a definitive HTTP response (even an
    error status, which retrying would never fix) are dropped from the queue.
    """
    if not state.outbox:
        return
    target_url = f"{state.relay_url.rstrip('/')}/api/input"
    auth = make_auth_header(state.relay_password)
    flushed = 0
    async with ClientSession() as session:
        while state.outbox:
            body = state.outbox[0]
            try:
                async with session.post(
                    target_url,
                    headers={"Authorization": auth, "Content-Type": "application/json"},
                    json=body,
                    timeout=aiohttp.ClientTimeout(total=5),
                ) as resp:
                    await resp.read()
                    state.outbox.popleft()
                    flushed += 1
                    if resp.status >= 400:
                        log.warning(
                            f"Outbox flush: relay answered {resp.status} for a queued "
                            "message — not retryable, dropping it"
                        )
            except Exception as e:
                log.warning(f"Outbox flush stopped after {flushed} message(s): {e}")
                break
    if flushed:
        log.info(f"Flushed {flushed} queued outbound message(s)")


async def handle_input(request: web.Request) -> web.Response:
    """
    Proxy POST /api/input to WeeChat's HTTP API.
    The WASM calls relay_url directly for sends, but this endpoint exists
    so the WASM can optionally route everything through the adapter.

    If the relay is unreachable the body is queued in a bounded outbox and
    retried, in order, after the next successful connect — a relay outage
    must not silently drop outbound messages. Queued sends return 202.
    """
    if not check_auth(request):
        return web.json_response({"error": "Unauthorized"}, status=401)

    try:
        body = await request.json()
    except Exception:
        return web.json_response({"error": "Invalid JSON body"}, status=400)

    target_url = f"{state.relay_url.rstrip('/')}/api/input"
    auth = make_auth_header(state.relay_password)

    if state.message_delay_s > 0:
        log.debug(f"Delaying outbound message by {state.message_delay_s}s")
        await asyncio.sleep(state.message_delay_s)

    # Preserve ordering: anything still queued must go out before this send,
    # so if the flush didn't fully drain we queue this one behind them.
    if state.outbox:
        await flush_outbox()
        if state.outbox:
            depth = enqueue_outbox(body)
            return web.json_response({"status": "queued", "outbox_depth": depth}, status=202)

    try:
        async with ClientSession() as session:
            async with session.post(
                target_url,
                headers={"Authorization": auth, "Content-Type": "application/json"},
                json=body,
                timeout=aiohttp.ClientTimeout(total=5),
            ) as resp:
                resp_body = await resp.read()
                log.debug(f"POST /api/input → proxied to WeeChat, status {resp.status}")
                return web.Response(
                    status=resp.status,
                    body=resp_body,
                    content_type="application/json",
                )
    except Exception as e:
        depth = enqueue_outbox(body)
        log.warning(f"Failed to proxy /api/input ({e}) — queued for retry (outbox depth {depth})")
        return web.json_response({"status": "queued", "outbox_depth": depth}, status=202)


# ---------------------------------------------------------------------------
# App setup
# ---------------------------------------------------------------------------

def make_app() -> web.Application:
    app = web.Application()
    app.router.add_get("/api/config", handle_config)
    app.router.add_get("/api/version", handle_version)
    app.router.add_get("/api/health", handle_health)
    app.router.add_get("/api/wait", handle_wait)
    app.router.add_get("/api/buffers", handle_buffers)
    app.router.add_get(r"/api/buffers/{buffer_name:.+}/lines", handle_buffer_lines)
    app.router.add_get(r"/api/buffers/{buffer_name:.+}", handle_single_buffer)
    app.router.add_post("/api/input", handle_input)
    return app


async def run(relay_url: str, password: str, port: int, message_delay: float = 0.0):
    state.relay_url = relay_url.rstrip("/")
    state.relay_password = password
    state.message_delay_s = message_delay
    state.new_event = asyncio.Event()  # must be created inside the running loop

    app = make_app()

    # Start WebSocket client in background
    ws_task = asyncio.create_task(ws_client_loop())

    runner = web.AppRunner(app)
    await runner.setup()
    site = web.TCPSite(runner, "127.0.0.1", port)
    await site.start()

    log.info(f"WeeChat WS adapter listening on http://127.0.0.1:{port}")
    log.info(f"Relay: {relay_url}  |  WebSocket: {relay_url.replace('http://', 'ws://').replace('https://', 'wss://')}/api  |  Auth: {'yes' if password else 'no'}")
    if message_delay > 0:
        log.info(f"Message delay enabled: {message_delay}s (applies to both inbound and outbound)")

    try:
        await asyncio.Event().wait()
    finally:
        ws_task.cancel()
        await runner.cleanup()


def main():
    parser = argparse.ArgumentParser(
        description="WeeChat WebSocket Adapter — bridges WeeChat WS events to a local HTTP API"
    )
    parser.add_argument(
        "--relay-url",
        default=os.environ.get("RELAY_URL", "http://127.0.0.1:9001"),
        help="WeeChat relay HTTP URL (default: http://127.0.0.1:9001)",
    )
    parser.add_argument(
        "--relay-password",
        default=os.environ.get("RELAY_PASSWORD", ""),
        help="WeeChat relay password (or set RELAY_PASSWORD env var)",
    )
    parser.add_argument(
        "--port",
        type=int,
        default=int(os.environ.get("ADAPTER_PORT") or os.environ.get("WEECHAT_ADAPTER_PORT") or "6681"),
        help="Local HTTP port for this adapter (default: 6681).  Also reads ADAPTER_PORT or WEECHAT_ADAPTER_PORT from env.",
    )
    parser.add_argument(
        "--message-delay",
        type=float,
        default=float(os.environ.get("MESSAGE_DELAY_SECONDS", "0")),
        metavar="SECONDS",
        help="Delay applied to both inbound and outbound messages (default: 0)",
    )
    parser.add_argument(
        "--debug",
        action="store_true",
        default=os.environ.get("DEBUG", "") != "",
        help="Enable debug logging",
    )
    args = parser.parse_args()

    logging.basicConfig(
        level=logging.DEBUG if args.debug else logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )

    if not args.relay_password:
        log.warning("No relay password set — adapter will accept unauthenticated requests")

    try:
        asyncio.run(run(args.relay_url, args.relay_password, args.port, args.message_delay))
    except KeyboardInterrupt:
        log.info("Adapter stopped")


if __name__ == "__main__":
    main()
