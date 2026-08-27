#!/usr/bin/env python3
"""
Tests for ws_adapter.py reconnect resilience (work item #78 + goal-25 F3):

- WSMsgType.ERROR is distinguished from CLOSED, logged with its payload,
  and surfaced via /api/health (last_disconnect, reconnect_count).
- Lines the relay accepted while the WS was down are backfilled on
  reconnect, deduped against live events by per-buffer line-id watermarks.
- Outbound /api/input proxy failures are queued in a bounded outbox and
  flushed, in order, after the next successful connect.
- Per-buffer watermarks persist across PROCESS restarts (state file), so a
  restart backfills lines accepted while the adapter was down instead of
  reseeding and classifying them as history.

Run from this directory:  python3 -m pytest ws_adapter_tests.py -v
"""
import asyncio
import json
from collections import deque
from types import SimpleNamespace

import pytest
from aiohttp import WSMsgType

import ws_adapter
from ws_adapter import state


# ---------------------------------------------------------------------------
# Fixtures / fakes
# ---------------------------------------------------------------------------

@pytest.fixture(autouse=True)
def reset_state(tmp_path, monkeypatch):
    state.line_buffer.clear()
    state.buffer_list = []
    state.ws_connected = False
    state.ws_error = None
    state.weechat_version = "unknown"
    state.relay_api_version = "unknown"
    state.relay_url = "http://127.0.0.1:9001"
    state.relay_password = ""
    state.message_delay_s = 0.0
    state.event_seq = 0
    state.event_log.clear()
    state.new_event = None
    state.last_disconnect = None
    state.reconnect_count = 0
    state.ever_connected = False
    state.last_line_id = {}
    state.outbox = deque()
    state.outbox_dropped = 0
    state.state_file = ""
    state.restarted = False
    # Isolate every test from the operator's real environment/tenant state dir
    # so persist_state()/load_persisted_state() only touch a temp sandbox.
    state_dir = tmp_path / "state"
    monkeypatch.setenv("LUNARWING_BASE_DIR", str(state_dir))
    monkeypatch.delenv("WS_ADAPTER_STATE_FILE", raising=False)
    yield
    for leftover in state_dir.parent.glob("*.json"):
        leftover.unlink(missing_ok=True)


class FakeResp:
    """Async-CM stand-in for an aiohttp POST response."""

    def __init__(self, status=204, body=b""):
        self.status = status
        self._body = body

    async def read(self):
        return self._body

    async def __aenter__(self):
        return self

    async def __aexit__(self, *args):
        return False


class FakePostSession:
    """Stands in for aiohttp.ClientSession in handle_input/flush_outbox.

    post_behavior(json_body) -> FakeResp, or raises to simulate the relay
    being unreachable. Records posted bodies in order.
    """

    def __init__(self, post_behavior):
        self._behavior = post_behavior
        self.posted = []

    async def __aenter__(self):
        return self

    async def __aexit__(self, *args):
        return False

    def post(self, url, headers=None, json=None, timeout=None):
        self.posted.append(json)
        return self._behavior(json)


class FakeRequest:
    def __init__(self, body):
        self._body = body
        self.headers = {}

    async def json(self):
        return self._body


def make_line(line_id, message="msg"):
    return {"id": line_id, "message": message, "tags_array": ["irc_privmsg", "nick_alice"]}


# ---------------------------------------------------------------------------
# record_line dedupe / watermarks
# ---------------------------------------------------------------------------

def test_record_line_records_and_dedupes_by_id():
    assert ws_adapter.record_line("irc.libera.#t", make_line(10)) is True
    assert ws_adapter.record_line("irc.libera.#t", make_line(11)) is True
    # Replay of an already-seen id (backfill/WS overlap) is dropped.
    assert ws_adapter.record_line("irc.libera.#t", make_line(10)) is False
    assert ws_adapter.record_line("irc.libera.#t", make_line(11)) is False
    assert state.last_line_id["irc.libera.#t"] == 11
    assert len(state.line_buffer["irc.libera.#t"]) == 2
    assert [e["seq"] for e in state.event_log] == [1, 2]


def test_record_line_without_id_always_records():
    line = {"message": "no id", "tags_array": []}
    assert ws_adapter.record_line("irc.libera.#t", line) is True
    assert ws_adapter.record_line("irc.libera.#t", dict(line)) is True
    assert "irc.libera.#t" not in state.last_line_id


# ---------------------------------------------------------------------------
# recover_after_connect: first-connect seed vs reconnect backfill
# ---------------------------------------------------------------------------

def _patch_backfill_io(monkeypatch, lines_by_buffer, captured_paths):
    async def fake_weechat_get(session, path, timeout=5.0):
        captured_paths.append(path)
        for name, lines in lines_by_buffer.items():
            if ws_adapter.quote(name, safe="") in path:
                return 200, json.dumps(lines).encode()
        return 404, b"{}"

    async def fake_flush():
        return None

    monkeypatch.setattr(ws_adapter, "weechat_get", fake_weechat_get)
    monkeypatch.setattr(ws_adapter, "flush_outbox", fake_flush)


def test_first_connect_seeds_watermarks_without_replay(monkeypatch):
    state.buffer_list = [{"id": 1, "full_name": "irc.libera.#a"}]
    lines = [make_line(i) for i in (30, 31, 32)]  # relay returns newest-first
    paths = []
    _patch_backfill_io(monkeypatch, {"irc.libera.#a": lines}, paths)

    asyncio.run(ws_adapter.recover_after_connect(None, is_reconnect=False))

    assert state.last_line_id["irc.libera.#a"] == 32
    assert len(state.event_log) == 0  # no history replay on first connect
    assert "%23a" in paths[0]  # buffer name URL-encoded


def test_reconnect_backfills_only_missed_lines(monkeypatch):
    state.buffer_list = [{"id": 1, "full_name": "irc.libera.#a"}]
    state.last_line_id["irc.libera.#a"] = 30  # seen before the disconnect
    state.ever_connected = True
    # Relay accepted 31 and 32 while the WS was down; 29/30 are old.
    lines = [make_line(i) for i in (32, 31, 30, 29)]
    _patch_backfill_io(monkeypatch, {"irc.libera.#a": lines}, [])

    asyncio.run(ws_adapter.recover_after_connect(None, is_reconnect=True))

    got_ids = [e["line"]["id"] for e in state.event_log]
    assert got_ids == [31, 32]  # only newer, oldest-first
    assert state.last_line_id["irc.libera.#a"] == 32
    assert [l["id"] for l in state.line_buffer["irc.libera.#a"]] == [31, 32]


def test_reconnect_recovers_buffer_created_during_outage(monkeypatch):
    state.buffer_list = [{"id": 2, "full_name": "irc.libera.alice"}]
    state.ever_connected = True  # no watermark: query buffer opened while down
    lines = [make_line(i) for i in (5, 4, 3)]
    _patch_backfill_io(monkeypatch, {"irc.libera.alice": lines}, [])

    asyncio.run(ws_adapter.recover_after_connect(None, is_reconnect=True))

    got_ids = [e["line"]["id"] for e in state.event_log]
    assert got_ids == [3, 4, 5]  # recovered in full, ascending
    assert state.last_line_id["irc.libera.alice"] == 5


# ---------------------------------------------------------------------------
# ws_client_loop: ERROR vs CLOSED classification
# ---------------------------------------------------------------------------

class FakeWS:
    def __init__(self, messages, exc=None):
        self._messages = messages
        self._exc = exc

    async def send_str(self, data):
        pass

    def exception(self):
        return self._exc

    def __aiter__(self):
        async def gen():
            for m in self._messages:
                yield m

        return gen()


class FakeWSConnectCM:
    def __init__(self, ws):
        self._ws = ws

    async def __aenter__(self):
        return self._ws

    async def __aexit__(self, *args):
        return False


class FakeWSSession:
    """First instance yields the scripted WS; the next connect attempt raises
    CancelledError to end the otherwise-infinite reconnect loop."""

    connects = 0
    scripted_ws = None

    async def __aenter__(self):
        return self

    async def __aexit__(self, *args):
        return False

    def ws_connect(self, url, headers=None, heartbeat=None, max_msg_size=None):
        FakeWSSession.connects += 1
        if FakeWSSession.connects > 1:
            raise asyncio.CancelledError
        return FakeWSConnectCM(FakeWSSession.scripted_ws)


def _run_loop_with_messages(monkeypatch, messages, exc=None):
    FakeWSSession.connects = 0
    FakeWSSession.scripted_ws = FakeWS(messages, exc=exc)
    sleeps = []

    async def fake_sleep(delay):
        sleeps.append(delay)

    async def fake_refresh(session):
        return None

    async def fake_recover(session, is_reconnect):
        return None

    monkeypatch.setattr(ws_adapter, "ClientSession", FakeWSSession)
    monkeypatch.setattr(ws_adapter, "refresh_buffer_list", fake_refresh)
    monkeypatch.setattr(ws_adapter, "recover_after_connect", fake_recover)
    monkeypatch.setattr(asyncio, "sleep", fake_sleep)

    asyncio.run(ws_adapter.ws_client_loop())
    return sleeps


def test_error_frame_logged_with_payload_and_backoff(monkeypatch, caplog):
    state.buffer_list = [{"id": 1, "full_name": "irc.libera.#t"}]
    line_event = json.dumps({
        "event_name": "buffer_line_added",
        "buffer_id": 1,
        "body": make_line(10, "hello"),
    })
    messages = [
        SimpleNamespace(type=WSMsgType.TEXT, data=line_event),
        SimpleNamespace(type=WSMsgType.ERROR, data="boom-data"),
    ]
    with caplog.at_level("WARNING", logger="weechat_ws"):
        sleeps = _run_loop_with_messages(monkeypatch, messages, exc=ConnectionError("boom"))

    assert sleeps == [ws_adapter.RECONNECT_DELAY_S]  # real backoff on break path
    assert state.ws_error == "boom"
    assert state.last_disconnect["type"] == "error"
    assert "boom" in state.last_disconnect["reason"]
    assert state.reconnect_count == 1
    assert state.ws_connected is False
    assert state.ever_connected is True
    # The TEXT event before the failure was recorded normally.
    assert [e["line"]["id"] for e in state.event_log] == [10]
    # ERROR frame is visible at WARNING, not buried at INFO.
    assert any(r.levelname == "WARNING" and "boom" in r.getMessage() for r in caplog.records)


def test_closed_frame_is_clean_disconnect(monkeypatch):
    messages = [SimpleNamespace(type=WSMsgType.CLOSED, data=None)]
    _run_loop_with_messages(monkeypatch, messages)

    assert state.last_disconnect["type"] == "closed"
    assert state.reconnect_count == 1
    assert state.ws_error is None  # clean close is not an error condition


# ---------------------------------------------------------------------------
# Outbound outbox
# ---------------------------------------------------------------------------

def test_handle_input_queues_when_relay_unreachable(monkeypatch):
    def fail(body):
        raise OSError("relay down")

    monkeypatch.setattr(ws_adapter, "ClientSession", lambda: FakePostSession(fail))
    body = {"buffer_name": "irc.libera.#a", "command": "hi"}

    resp = asyncio.run(ws_adapter.handle_input(FakeRequest(body)))

    assert resp.status == 202
    assert json.loads(resp.text)["outbox_depth"] == 1
    assert list(state.outbox) == [body]


def test_handle_input_passthrough_on_success(monkeypatch):
    monkeypatch.setattr(
        ws_adapter, "ClientSession", lambda: FakePostSession(lambda body: FakeResp(204, b""))
    )
    body = {"buffer_name": "irc.libera.#a", "command": "hi"}

    resp = asyncio.run(ws_adapter.handle_input(FakeRequest(body)))

    assert resp.status == 204
    assert len(state.outbox) == 0


def test_handle_input_queues_behind_undrained_outbox(monkeypatch):
    older = {"buffer_name": "irc.libera.#a", "command": "older"}
    state.outbox.append(older)

    def fail(body):
        raise OSError("still down")

    monkeypatch.setattr(ws_adapter, "ClientSession", lambda: FakePostSession(fail))
    newer = {"buffer_name": "irc.libera.#a", "command": "newer"}

    resp = asyncio.run(ws_adapter.handle_input(FakeRequest(newer)))

    assert resp.status == 202
    assert list(state.outbox) == [older, newer]  # ordering preserved


def test_flush_outbox_drains_in_order(monkeypatch):
    bodies = [{"command": str(i)} for i in range(3)]
    state.outbox.extend(bodies)
    session = FakePostSession(lambda body: FakeResp(204, b""))
    monkeypatch.setattr(ws_adapter, "ClientSession", lambda: session)

    asyncio.run(ws_adapter.flush_outbox())

    assert session.posted == bodies  # FIFO order
    assert len(state.outbox) == 0


def test_flush_outbox_stops_at_first_failure(monkeypatch):
    bodies = [{"command": "a"}, {"command": "fail"}, {"command": "c"}]
    state.outbox.extend(bodies)

    def behavior(body):
        if body["command"] == "fail":
            raise OSError("down")
        return FakeResp(204, b"")

    monkeypatch.setattr(ws_adapter, "ClientSession", lambda: FakePostSession(behavior))

    asyncio.run(ws_adapter.flush_outbox())

    assert [b["command"] for b in state.outbox] == ["fail", "c"]


def test_flush_outbox_drops_definitive_http_errors(monkeypatch):
    state.outbox.extend([{"command": "bad"}, {"command": "ok"}])

    def behavior(body):
        return FakeResp(400, b"nope") if body["command"] == "bad" else FakeResp(204, b"")

    monkeypatch.setattr(ws_adapter, "ClientSession", lambda: FakePostSession(behavior))

    asyncio.run(ws_adapter.flush_outbox())

    assert len(state.outbox) == 0  # 400 is not retryable; the rest still flush


def test_enqueue_outbox_bounded_drops_oldest():
    for i in range(ws_adapter.MAX_OUTBOX):
        ws_adapter.enqueue_outbox({"command": str(i)})
    depth = ws_adapter.enqueue_outbox({"command": "new"})

    assert depth == ws_adapter.MAX_OUTBOX
    assert state.outbox_dropped == 1
    assert state.outbox[0]["command"] == "1"  # oldest ("0") was dropped
    assert state.outbox[-1]["command"] == "new"


# ---------------------------------------------------------------------------
# /api/health exposure
# ---------------------------------------------------------------------------

def test_health_exposes_disconnect_and_outbox_state():
    state.ws_connected = False
    state.ws_error = "boom"
    state.last_disconnect = {"type": "error", "reason": "boom", "at": "2026-08-26T00:00:00+00:00"}
    state.reconnect_count = 3
    state.outbox.append({"command": "x"})
    state.outbox_dropped = 2

    resp = asyncio.run(ws_adapter.handle_health(None))
    payload = json.loads(resp.text)

    assert payload["status"] == "degraded"
    assert payload["ws_error"] == "boom"
    assert payload["last_disconnect"]["type"] == "error"
    assert payload["reconnect_count"] == 3
    assert payload["outbox_depth"] == 1
    assert payload["outbox_dropped"] == 2
    assert "event_cursor" in payload  # long-poll support marker preserved


# ---------------------------------------------------------------------------
# Persisted watermark state across process restarts (goal-25 finding F3)
# ---------------------------------------------------------------------------

def test_record_line_persists_watermark_advance():
    state.state_file = ws_adapter.resolve_state_file()  # enabled via env fixture
    assert state.state_file != ""

    ws_adapter.record_line("irc.libera.#a", make_line(10))
    saved = json.loads((ws_adapter.Path(state.state_file)).read_text())

    assert saved["last_line_id"] == {"irc.libera.#a": 10}
    assert "saved_at" in saved


def test_load_persisted_state_marks_restart_and_restores_watermarks():
    path = ws_adapter.Path(ws_adapter.resolve_state_file())
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps({
        "version": 1,
        "saved_at": ws_adapter.datetime.now(ws_adapter.timezone.utc).isoformat(),
        "last_line_id": {"irc.libera.#a": 30, "irc.libera.alice": 5},
    }))

    assert ws_adapter.load_persisted_state() is True

    assert state.restarted is True
    assert state.last_line_id == {"irc.libera.#a": 30, "irc.libera.alice": 5}


def test_load_without_state_file_is_fresh_start():
    assert ws_adapter.load_persisted_state() is False
    assert state.restarted is False
    assert state.last_line_id == {}


def test_load_discards_stale_state_file():
    from datetime import timedelta

    path = ws_adapter.Path(ws_adapter.resolve_state_file())
    path.parent.mkdir(parents=True, exist_ok=True)
    ancient = ws_adapter.datetime.now(ws_adapter.timezone.utc) - timedelta(
        days=ws_adapter.WATERMARK_MAX_AGE_DAYS + 1
    )
    path.write_text(json.dumps({
        "version": 1,
        "saved_at": ancient.isoformat(),
        "last_line_id": {"irc.libera.#a": 30},
    }))

    assert ws_adapter.load_persisted_state() is False
    assert state.last_line_id == {}  # line ids are not stable across WeeChat restarts


def test_load_discards_corrupt_state_file():
    path = ws_adapter.Path(ws_adapter.resolve_state_file())
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("{not json")

    assert ws_adapter.load_persisted_state() is False
    assert state.last_line_id == {}


def test_state_file_env_none_disables_persistence(monkeypatch):
    monkeypatch.setenv("WS_ADAPTER_STATE_FILE", "none")

    assert ws_adapter.resolve_state_file() == ""
    state.state_file = ""
    state.last_line_id["irc.libera.#a"] = 10
    ws_adapter.persist_state()  # must be a no-op, not a crash

    assert not (ws_adapter.Path(ws_adapter.resolve_state_file() or "/nonexistent/x.json")).exists()


def test_restart_backfills_lines_accepted_while_adapter_was_down(monkeypatch):
    # Goal-25 H4 replay: adapter restarts mid-stream; lines the relay accepted
    # during the outage must now be recovered instead of classified as history.
    state.state_file = ws_adapter.resolve_state_file()
    ws_adapter.record_line("irc.libera.#a", make_line(30))  # persists watermark 30

    # Simulate a new process: fresh in-memory state, then load from disk.
    state.line_buffer.clear()
    state.event_log.clear()
    state.event_seq = 0
    state.last_line_id = {}
    state.ever_connected = False
    assert ws_adapter.load_persisted_state() is True

    # Lines 31/32 arrived while the (new) adapter process was down.
    state.buffer_list = [{"id": 1, "full_name": "irc.libera.#a"}]
    lines = [make_line(i) for i in (32, 31, 30, 29)]
    _patch_backfill_io(monkeypatch, {"irc.libera.#a": lines}, [])

    asyncio.run(ws_adapter.recover_after_connect(None, is_reconnect=True))

    got_ids = [e["line"]["id"] for e in state.event_log]
    assert got_ids == [31, 32]  # recovered, not swallowed
    assert state.last_line_id["irc.libera.#a"] == 32
