#!/usr/bin/env python3
"""
darkirc-http-adapter: Bridges DarkIRC's IRC interface to HTTP.

Connects to DarkIRC's local IRC server (localhost:6667) and exposes
a simple HTTP API that the LunarWing WASM channel can poll.

Endpoints:
  GET  /poll          → Returns queued inbound DMs as JSON array
  POST /ack           → Acknowledges matching opaque batch_id (at-least-once)
  POST /send          → Queues a DM for paced delivery: {"to":"nick","text":"..."}
  GET  /health        → Health check (inbound queue + outbox depth)

Outbound delivery is asynchronous: /send splits the text into IRC-safe
chunks, numbers multi-chunk messages ("[i/N] "), and enqueues them into a
paced outbox. A background task drains one PRIVMSG per
DARKIRC_PRIVMSG_INTERVAL seconds (DarkFi metering drops messages sent
faster than that). /send returns as soon as the message is queued
("status":"queued") — the message is NOT on the wire yet. Chunks queued
while IRC is disconnected are delivered after reconnection.

Security, session management, and prompt-injection defenses remain in the
WASM channel / LunarWing host.
"""

import asyncio
import hmac
import json
import logging
import os
import re
import secrets
import signal
import sys
import time
from collections import deque
from http import HTTPStatus

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

IRC_HOST = os.getenv("DARKIRC_HOST", "127.0.0.1")
IRC_PORT = int(os.getenv("DARKIRC_PORT", "6667"))
IRC_NICK = os.getenv("DARKIRC_NICK", "kageho-bridge")
IRC_USER = os.getenv("DARKIRC_USER", "kageho")
IRC_REALNAME = os.getenv("DARKIRC_REALNAME", "DarkIRC-LunarWing Bridge")

HTTP_HOST = os.getenv("ADAPTER_HOST", "127.0.0.1")
HTTP_PORT = int(os.getenv("ADAPTER_PORT", "6680"))

# Max fresh inbound messages before IRC reads pause and apply TCP backpressure.
MAX_QUEUE = int(os.getenv("ADAPTER_MAX_QUEUE", "500"))
# Host accepts at most 100 emits from one WASM callback.
MAX_POLL_BATCH = 100

# Max UTF-8 payload bytes; per-target clamp keeps full IRC frame within 450 bytes.
MAX_IRC_MESSAGE_BYTES = int(os.getenv("DARKIRC_MAX_MESSAGE_BYTES", "400"))

# Minimum seconds between PRIVMSG sends (DarkFi metering requires spacing)
PRIVMSG_MIN_INTERVAL = float(os.getenv("DARKIRC_PRIVMSG_INTERVAL", "7.0"))

# Max chunks the paced outbox may hold before /send returns 429. At one chunk
# per PRIVMSG_MIN_INTERVAL the default 400 chunks ≈ 47 minutes of backlog.
MAX_OUTBOX = int(os.getenv("ADAPTER_MAX_OUTBOX", "400"))

# Optional JSON file persisting the outbox across adapter restarts.
# Empty disables journaling; production templates configure a durable path.
OUTBOX_JOURNAL = os.getenv("ADAPTER_OUTBOX_JOURNAL", "")

# Cap on inbound HTTP request bodies (M5: the hand-rolled reader trusts
# Content-Length via readexactly — without a cap a caller can force unbounded
# allocation → OOM). 64 KiB is far above any legitimate /send payload.
MAX_BODY_BYTES = int(os.getenv("ADAPTER_MAX_BODY_BYTES", str(64 * 1024)))

# Shared secret for basic auth between WASM channel and adapter
# The WASM channel sends this as Bearer token
ADAPTER_SECRET = os.getenv("ADAPTER_SECRET", "")

LOG_LEVEL = os.getenv("ADAPTER_LOG_LEVEL", "INFO")

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------

log = logging.getLogger("darkirc-adapter")


def setup_logging():
    level = getattr(logging, LOG_LEVEL.upper(), logging.INFO)
    fmt = logging.Formatter(
        "%(asctime)s [%(levelname)s] %(name)s: %(message)s",
        datefmt="%Y-%m-%dT%H:%M:%S",
    )
    sh = logging.StreamHandler(sys.stderr)
    sh.setFormatter(fmt)
    log.addHandler(sh)
    log.setLevel(level)


# ---------------------------------------------------------------------------
# IRC Client (async, minimal, no deps)
# ---------------------------------------------------------------------------

_CTRL_RE = re.compile(r"[\x00-\x1f\x7f]")
_IRC_NICK_RE = re.compile(r"^[A-Za-z\[\]\\`_^{|}][A-Za-z0-9\[\]\\`_^{|}-]{0,31}$")
IRC_FRAME_MAX_BYTES = 450
NATURAL_BREAK_SLACK_BYTES = 4


class IRCClient:
    def __init__(self):
        self._reader = None
        self._writer = None
        self._connected = False
        self._registered = False
        self.nick = IRC_NICK

    @property
    def connected(self):
        return self._connected and self._registered

    async def connect(self):
        self._reader, self._writer = await asyncio.open_connection(
            IRC_HOST, IRC_PORT
        )
        self._connected = True
        self._registered = False
        await self._send(f"NICK {IRC_NICK}")
        await self._send(f"USER {IRC_USER} 0 * :{IRC_REALNAME}")
        log.info("IRC: connecting to %s:%d as %s", IRC_HOST, IRC_PORT, IRC_NICK)

    async def _send(self, line: str):
        if not self._writer:
            raise ConnectionError("not connected")
        if _CTRL_RE.search(line):
            raise ValueError("IRC protocol line contains control character")
        log.debug("IRC >>> %s", line)
        try:
            self._writer.write((line + "\r\n").encode("utf-8", errors="replace"))
            await self._writer.drain()
        except (ConnectionError, OSError) as e:
            log.error("IRC send failed: %s", e)
            self._connected = False
            self._registered = False
            raise

    async def readline(self):
        if not self._reader:
            raise ConnectionError("not connected")
        data = await self._reader.readline()
        if not data:
            return None
        line = data.decode("utf-8", errors="replace").rstrip("\r\n")
        log.debug("IRC <<< %s", line)
        return line

    async def pong(self, token: str):
        # The PONG token echoes the (possibly hostile or corrupt) PING token.
        # _send rejects any control character in a frame, so strip them here —
        # otherwise a PING whose token contains a control char would raise and
        # take down the IRC loop.
        await self._send(f"PONG :{_CTRL_RE.sub('', token)}")

    async def close(self):
        self._connected = False
        self._registered = False
        if self._writer:
            try:
                self._writer.close()
                await self._writer.wait_closed()
            except Exception:
                pass

    def parse_line(self, raw: str):
        prefix = None
        if raw.startswith(":"):
            # partition never raises: a prefix-only line (":foo", no space)
            # yields prefix="foo", raw="" instead of an unpack ValueError that
            # would propagate out and kill the IRC loop.
            prefix, _, raw = raw[1:].partition(" ")
        if " :" in raw:
            params_part, trailing = raw.split(" :", 1)
        else:
            params_part, trailing = raw, None
        parts = params_part.split()
        command = parts[0].upper() if parts else ""
        params = parts[1:] if len(parts) > 1 else []
        return {
            "prefix": prefix,
            "command": command,
            "params": params,
            "trailing": trailing,
        }

    def mark_registered(self):
        self._registered = True


# ---------------------------------------------------------------------------
# Message splitting (UTF-8 byte-safe, word-boundary-aware)
# ---------------------------------------------------------------------------

def _split_message_bytes(text: str, max_bytes: int) -> list:
    """Split text into chunks that fit within max_bytes UTF-8 encoded length,
    preferring spaces close to the byte limit.
    Always preserves character boundaries (no split mid-codepoint).

    IRC controls, including CR/LF, become spaces so no chunk can inject an
    additional protocol frame.
    """
    text = _CTRL_RE.sub(" ", text)

    if len(text.encode("utf-8")) <= max_bytes:
        return [text]

    chunks: list = []
    remaining = text

    while remaining:
        if len(remaining.encode("utf-8")) <= max_bytes:
            chunks.append(remaining)
            break

        # Binary-search the largest slice whose UTF-8 fits in max_bytes
        lo, hi = 0, len(remaining)
        while lo < hi:
            mid = (lo + hi + 1) // 2
            if len(remaining[:mid].encode("utf-8")) <= max_bytes:
                lo = mid
            else:
                hi = mid - 1

        if lo == 0:
            # Single character exceeds max_bytes — just take it anyway
            chunks.append(remaining[0])
            remaining = remaining[1:]
            continue

        cut = remaining[:lo]

        # Use a nearby natural break without shrinking chunks enough to violate
        # the host's six-message budget after numbering prefixes are added.
        sp = cut.rfind(" ")
        if sp > 0 and len(cut[sp:].encode("utf-8")) <= NATURAL_BREAK_SLACK_BYTES:
            chunks.append(cut[:sp])
            remaining = remaining[sp + 1:]  # skip the space
            continue

        # No good break point — hard cut at the byte limit
        chunks.append(cut)
        remaining = remaining[lo:]

    return chunks


# ---------------------------------------------------------------------------
# Outbound message queue (paced PRIVMSG delivery)
# ---------------------------------------------------------------------------

# FIFO of {"to": nick, "text": chunk} awaiting delivery. A single global queue
# keeps overall send order (DarkFi metering applies across all targets) and
# per-target chunk order.
outbox: deque = deque()
# -inf so the first send never waits, regardless of the monotonic clock origin.
_last_privmsg_time: float = float("-inf")


def _effective_max_bytes(to: str) -> int:
    """Largest payload keeping 'PRIVMSG <to> :<payload>\\r\\n' within the
    450-byte conservative IRC frame, capped by MAX_IRC_MESSAGE_BYTES.

    With the default 400-byte cap this only bites for pathologically long
    nicks; it lets operators raise DARKIRC_MAX_MESSAGE_BYTES while staying
    frame-safe on the client side.
    """
    frame_overhead = len("PRIVMSG  :") + len(to.encode("utf-8")) + 2  # +CRLF
    return max(1, min(MAX_IRC_MESSAGE_BYTES, IRC_FRAME_MAX_BYTES - frame_overhead))


def _valid_irc_target(to: str) -> bool:
    """Accept direct-message IRC nick syntax only."""
    return bool(_IRC_NICK_RE.fullmatch(to))


def _numbered_chunks(to: str, text: str) -> list:
    """Split text for the target, prefixing '[i/N] ' when it needs more than
    one chunk so recipients can spot missing or out-of-order parts.

    The prefix counts against the per-chunk byte budget: splitting is retried
    with a growing reserve until every prefixed chunk fits. The reserve uses
    the widest prefix ('[N/N] ') so every chunk index fits; the loop
    terminates because the reserve grows monotonically and is bounded by the
    prefix width for the worst-case chunk count.
    """
    budget = _effective_max_bytes(to)
    chunks = _split_message_bytes(text, budget)
    if len(chunks) <= 1:
        return chunks

    reserve = 0
    while True:
        prefix_width = len(f"[{len(chunks)}/{len(chunks)}] ".encode("utf-8"))
        if prefix_width <= reserve:
            break
        reserve = prefix_width
        chunks = _split_message_bytes(text, max(1, budget - reserve))

    total = len(chunks)
    return [f"[{i}/{total}] {chunk}" for i, chunk in enumerate(chunks, start=1)]


def enqueue_send(to: str, text: str):
    """Queue a message for paced delivery. Returns (ok, info_dict)."""
    if not _valid_irc_target(to):
        return False, {"error": "invalid IRC target"}
    chunks = _numbered_chunks(to, text)
    if len(outbox) + len(chunks) > MAX_OUTBOX:
        return False, {
            "error": "outbox full",
            "chunks": len(chunks),
            "outbox": len(outbox),
        }
    for chunk in chunks:
        outbox.append({"to": to, "text": chunk})
    if not _journal_save():
        for _ in chunks:
            outbox.pop()
        return False, {"error": "outbox journal write failed"}
    return True, {"status": "queued", "chunks": len(chunks), "outbox": len(outbox)}


def _journal_load():
    """Restore queued chunks from the journal file, if configured."""
    if not OUTBOX_JOURNAL:
        return
    try:
        with open(OUTBOX_JOURNAL, "r", encoding="utf-8") as fh:
            items = json.load(fh)
    except FileNotFoundError:
        return
    except (OSError, ValueError) as e:
        log.warning("Outbox journal unreadable, starting empty: %s", e)
        return
    restored = 0
    if isinstance(items, list):
        for item in items:
            if not isinstance(item, dict):
                continue
            to, text = item.get("to"), item.get("text")
            if (
                isinstance(to, str)
                and isinstance(text, str)
                and _valid_irc_target(to)
                and text
            ):
                outbox.append({"to": to, "text": _CTRL_RE.sub(" ", text)})
                restored += 1
    if restored:
        log.info("Outbox journal: restored %d queued chunk(s)", restored)


def _journal_save():
    """Persist outbox with crash-durable same-directory atomic replacement."""
    if not OUTBOX_JOURNAL:
        return True
    parent = os.path.dirname(os.path.abspath(OUTBOX_JOURNAL))
    basename = os.path.basename(OUTBOX_JOURNAL)
    tmp = None
    fd = None
    try:
        # O_EXCL plus random name prevents symlink/hardlink replacement races.
        for _ in range(100):
            candidate = os.path.join(parent, f".{basename}.{secrets.token_hex(16)}.tmp")
            try:
                fd = os.open(candidate, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                tmp = candidate
                break
            except FileExistsError:
                continue
        else:
            raise OSError("could not allocate unique journal temp file")

        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            fd = None  # fdopen owns descriptor from here.
            json.dump(list(outbox), fh, ensure_ascii=False)
            fh.flush()
            os.fsync(fh.fileno())
        os.replace(tmp, OUTBOX_JOURNAL)
        tmp = None

        dir_flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
        dir_fd = os.open(parent, dir_flags)
        try:
            os.fsync(dir_fd)
        finally:
            os.close(dir_fd)
        return True
    except OSError as e:
        log.warning("Outbox journal write failed: %s", e)
        return False
    finally:
        if fd is not None:
            try:
                os.close(fd)
            except OSError:
                pass
        if tmp is not None:
            try:
                os.unlink(tmp)
            except FileNotFoundError:
                pass
            except OSError as e:
                log.warning("Outbox journal temp cleanup failed: %s", e)


async def outbox_loop():
    """Single consumer draining the outbox one paced PRIVMSG at a time.

    Peek-send-pop: a chunk is only removed after the socket write succeeds,
    so an IRC disconnect retries the same chunk after reconnection instead
    of dropping it.
    """
    global _last_privmsg_time
    while _running:
        if not outbox or not irc.connected:
            await asyncio.sleep(0.5)
            continue

        now = asyncio.get_running_loop().time()
        wait = PRIVMSG_MIN_INTERVAL - (now - _last_privmsg_time)
        if wait > 0:
            # Re-check running/connected/queue state after the pacing sleep.
            await asyncio.sleep(wait)
            continue

        item = outbox[0]
        try:
            await irc._send(f"PRIVMSG {item['to']} :{item['text']}")
        except Exception as e:
            log.warning("Outbox send failed (will retry): %s", e)
            await asyncio.sleep(1.0)
            continue
        _last_privmsg_time = asyncio.get_running_loop().time()
        outbox.popleft()
        _journal_save()
        log.info("→ DM chunk sent to %s (%d still queued)", item["to"], len(outbox))


# ---------------------------------------------------------------------------
# Message queue (inbound DMs from DarkIRC)
# ---------------------------------------------------------------------------

message_queue: deque = deque(maxlen=MAX_QUEUE)

# Messages delivered via /poll but not yet acked (M4: at-least-once delivery).
# Re-served on the next /poll until /ack clears them, so a host crash between
# poll and processing doesn't lose DMs.
pending_ack: deque = deque(maxlen=MAX_QUEUE)
pending_batch_id = None


def _poll_messages() -> list:
    """Return current at-least-once batch without evicting unacked entries."""
    global pending_batch_id
    if not pending_ack:
        while message_queue and len(pending_ack) < min(MAX_QUEUE, MAX_POLL_BATCH):
            pending_ack.append(message_queue.popleft())
        if pending_ack:
            pending_batch_id = secrets.token_urlsafe(24)
        else:
            pending_batch_id = None
    elif pending_batch_id is None:
        pending_batch_id = secrets.token_urlsafe(24)
    return list(pending_ack)


def _ack_batch(batch_id: str) -> bool:
    """Clear only batch identified by current opaque token."""
    global pending_batch_id
    if not pending_ack or not pending_batch_id or batch_id != pending_batch_id:
        return False
    pending_ack.clear()
    pending_batch_id = None
    return True


# ---------------------------------------------------------------------------
# IRC loop
# ---------------------------------------------------------------------------

irc = IRCClient()
_running = True


async def irc_loop():
    global _running
    delay = 5

    while _running:
        try:
            await irc.connect()
            delay = 5

            while _running:
                # Stop reading so TCP applies backpressure instead of deque
                # eviction when host has not drained fresh messages.
                if len(message_queue) >= MAX_QUEUE:
                    await asyncio.sleep(0.1)
                    continue
                line = await asyncio.wait_for(irc.readline(), timeout=300)
                if line is None:
                    log.warning("IRC EOF, reconnecting")
                    break

                parsed = irc.parse_line(line)
                cmd = parsed["command"]

                if cmd == "PING":
                    await irc.pong(parsed["trailing"] or "")
                    continue

                if cmd == "001":
                    irc.mark_registered()
                    log.info("IRC: registered as %s", irc.nick)
                    continue

                if cmd == "433":
                    irc.nick += "_"
                    await irc._send(f"NICK {irc.nick}")
                    continue

                if cmd == "PRIVMSG":
                    target = parsed["params"][0] if parsed["params"] else ""
                    text = parsed["trailing"] or ""
                    prefix = parsed["prefix"] or ""

                    # Only DMs (target == our nick)
                    if target.startswith("#") or target.startswith("&"):
                        continue

                    sender = prefix.split("!")[0] if prefix else ""
                    if not sender:
                        continue

                    clean = _CTRL_RE.sub("", text).strip()
                    if not clean:
                        continue

                    message_queue.append({
                        "from": sender,
                        "text": clean,
                        "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                    })
                    log.info("← DM from %s queued (%d chars)", sender, len(clean))

        except asyncio.TimeoutError:
            log.warning("IRC timeout, reconnecting")
        except (ConnectionError, OSError) as e:
            log.error("IRC error: %s", e)
        except Exception as e:
            # Last-resort self-heal: reconnect instead of letting the task die
            # silently. A dead irc_loop stops all inbound DMs AND paced outbound
            # delivery (outbox_loop only drains while irc.connected) while the
            # HTTP server keeps reporting the adapter healthy.
            log.error("Unexpected IRC loop error, reconnecting: %s", e)
        finally:
            await irc.close()

        if _running:
            log.info("IRC reconnecting in %ds", delay)
            await asyncio.sleep(delay)
            delay = min(delay * 2, 300)


# ---------------------------------------------------------------------------
# HTTP server (async, stdlib only — no aiohttp/flask)
# ---------------------------------------------------------------------------

async def handle_http(reader: asyncio.StreamReader, writer: asyncio.StreamWriter):
    try:
        # Read request line
        request_line = await asyncio.wait_for(reader.readline(), timeout=10)
        if not request_line:
            writer.close()
            return

        request_str = request_line.decode("utf-8", errors="replace").strip()
        parts = request_str.split(" ")
        if len(parts) < 2:
            await send_response(writer, 400, {"error": "bad request"})
            return

        method, path = parts[0], parts[1]

        # Read headers
        headers = {}
        while True:
            header_line = await asyncio.wait_for(reader.readline(), timeout=5)
            if not header_line or header_line == b"\r\n" or header_line == b"\n":
                break
            decoded = header_line.decode("utf-8", errors="replace").strip()
            if ":" in decoded:
                key, val = decoded.split(":", 1)
                headers[key.strip().lower()] = val.strip()

        # Auth check (M6: constant-time comparison to avoid a timing oracle on
        # the bearer secret). Empty ADAPTER_SECRET => no auth (dev); mt-admin
        # always sets a secret, so multi-tenant deployments are always authed.
        if ADAPTER_SECRET:
            auth = headers.get("authorization", "")
            if not hmac.compare_digest(auth, f"Bearer {ADAPTER_SECRET}"):
                await send_response(writer, 401, {"error": "unauthorized"})
                return

        # Read body if present (M5: cap Content-Length and tolerate a malformed
        # value instead of trusting it blindly into readexactly → OOM).
        body = b""
        try:
            content_length = int(headers.get("content-length", "0"))
        except ValueError:
            await send_response(writer, 400, {"error": "invalid content-length"})
            return
        if content_length < 0 or content_length > MAX_BODY_BYTES:
            await send_response(writer, 413, {"error": "payload too large"})
            return
        if content_length > 0:
            body = await asyncio.wait_for(
                reader.readexactly(content_length), timeout=10
            )

        # Route
        if method == "GET" and path == "/health":
            await send_response(writer, 200, {
                "status": "ok",
                "irc_connected": irc.connected,
                "irc_nick": irc.nick,
                "queue_size": len(message_queue),
                "outbox": len(outbox),
            })

        elif method == "GET" and path == "/poll":
            # Never combine fresh messages with an unacked full batch: bounded
            # pending_ack would evict old entries. Fresh messages remain queued
            # until /ack, while IRC input receives TCP backpressure at its cap.
            messages = _poll_messages()
            await send_response(writer, 200, {
                "messages": messages,
                "batch_id": pending_batch_id,
            })

        elif method == "POST" and path == "/ack":
            if not body:
                await send_response(writer, 400, {"error": "empty body"})
                return
            try:
                payload = json.loads(body)
            except json.JSONDecodeError:
                await send_response(writer, 400, {"error": "invalid json"})
                return
            batch_id = payload.get("batch_id") if isinstance(payload, dict) else None
            if not isinstance(batch_id, str) or not batch_id:
                await send_response(writer, 400, {"error": "missing 'batch_id'"})
                return
            if not _ack_batch(batch_id):
                await send_response(writer, 409, {"error": "batch_id does not match pending batch"})
                return
            await send_response(writer, 200, {"status": "acked"})

        elif method == "POST" and path == "/send":
            if not body:
                await send_response(writer, 400, {"error": "empty body"})
                return
            try:
                payload = json.loads(body)
            except json.JSONDecodeError:
                await send_response(writer, 400, {"error": "invalid json"})
                return

            if not isinstance(payload, dict):
                await send_response(writer, 400, {"error": "invalid payload"})
                return
            to = payload.get("to", "")
            text = payload.get("text", "")

            if not isinstance(to, str) or not isinstance(text, str) or not to or not text:
                await send_response(writer, 400, {"error": "missing 'to' or 'text'"})
                return
            if not _valid_irc_target(to):
                await send_response(writer, 400, {"error": "invalid IRC target"})
                return
            text = text.strip()
            if not text:
                await send_response(writer, 400, {"error": "missing 'to' or 'text'"})
                return

            # No irc.connected gate: chunks queued while IRC is down are
            # delivered after reconnection — rejecting at accept time was a
            # message-loss mode (callers dropped the response on 503).
            ok, info = enqueue_send(to, text)
            if not ok:
                status = 429 if info["error"] == "outbox full" else 503
                await send_response(writer, status, info)
                return
            info["irc_connected"] = irc.connected
            await send_response(writer, 200, info)

        else:
            await send_response(writer, 404, {"error": "not found"})

    except (asyncio.TimeoutError, ConnectionError, OSError):
        pass
    finally:
        try:
            writer.close()
            await writer.wait_closed()
        except Exception:
            pass


async def send_response(writer, status_code: int, body: dict):
    status_text = HTTPStatus(status_code).phrase
    payload = json.dumps(body, ensure_ascii=False).encode("utf-8")
    response = (
        f"HTTP/1.1 {status_code} {status_text}\r\n"
        f"Content-Type: application/json\r\n"
        f"Content-Length: {len(payload)}\r\n"
        f"Connection: close\r\n"
        f"\r\n"
    ).encode("utf-8") + payload
    writer.write(response)
    await writer.drain()


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

async def main():
    global _running
    setup_logging()

    log.info("darkirc-http-adapter starting")
    log.info("  IRC: %s:%d nick=%s", IRC_HOST, IRC_PORT, IRC_NICK)
    log.info("  HTTP: %s:%d", HTTP_HOST, HTTP_PORT)
    if not ADAPTER_SECRET:
        log.warning(
            "ADAPTER_SECRET not set — running WITHOUT auth. This is dev-only; "
            "multi-tenant deployments must set it (lunarwing-mt-admin.sh does so "
            "automatically). Without it any local process can read/send DMs."
        )

    # Restore any chunks left queued by a previous run (if journaling enabled)
    _journal_load()

    # Start HTTP server
    server = await asyncio.start_server(
        handle_http, HTTP_HOST, HTTP_PORT
    )
    log.info("HTTP server listening on %s:%d", HTTP_HOST, HTTP_PORT)

    loop = asyncio.get_running_loop()
    for sig in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(sig, lambda: _shutdown())

    # Run IRC loop, paced outbox drain, and HTTP server concurrently
    irc_task = asyncio.create_task(irc_loop())
    outbox_task = asyncio.create_task(outbox_loop())

    async with server:
        await server.serve_forever()


def _shutdown():
    global _running
    _running = False
    log.info("Shutdown signal received")
    # Stop the event loop
    for task in asyncio.all_tasks():
        task.cancel()


if __name__ == "__main__":
    asyncio.run(main())
