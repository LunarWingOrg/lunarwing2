#!/usr/bin/env python3
"""Unit tests for the paced outbox in darkirc_adapter.py.

Covers the outbound-delivery redesign (regression tests for the DarkIRC
message-loss cluster):
  - /send used to block until every chunk was on the wire, which combined
    with WASM HTTP timeouts and the host callback timeout to silently drop
    the tail of long responses. Now /send enqueues and a background task
    paces delivery, so these tests pin down:
      * per-target byte budgets (frame-safe payload sizing)
      * "[i/N] " numbering with byte budgets still respected
      * FIFO enqueue + bounded outbox (429 semantics come from the cap)
      * journal persistence across a simulated restart
      * drain-loop ordering, pacing, and peek-send-pop retry on failure

Standalone (no pytest): run directly. Exits 0 on pass, 1 on fail.
"""

import asyncio
import json
import os
import stat
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import darkirc_adapter as mod
from darkirc_adapter import (
    _effective_max_bytes,
    _ack_batch,
    _numbered_chunks,
    _poll_messages,
    _split_message_bytes,
    _valid_irc_target,
    enqueue_send,
)


def _strip_number_prefix(chunk: str) -> str:
    """Remove a leading '[i/N] ' marker if present."""
    if chunk.startswith("["):
        close = chunk.find("] ")
        if close != -1 and "/" in chunk[1:close]:
            return chunk[close + 2:]
    return chunk


def _non_ws(text: str) -> str:
    return "".join(c for c in text if not c.isspace())


class EffectiveMaxBytesTest(unittest.TestCase):
    def test_normal_nick_uses_configured_cap(self):
        # 'PRIVMSG alice :' + payload + CRLF stays far under 512, so the
        # configured cap (default 400) wins.
        self.assertEqual(_effective_max_bytes("alice"), mod.MAX_IRC_MESSAGE_BYTES)

    def test_long_nick_shrinks_budget(self):
        nick = "n" * 200
        budget = _effective_max_bytes(nick)
        frame = len("PRIVMSG  :") + len(nick.encode()) + 2 + budget
        self.assertLessEqual(frame, mod.IRC_FRAME_MAX_BYTES)
        self.assertLess(budget, mod.MAX_IRC_MESSAGE_BYTES)

    def test_budget_never_below_one(self):
        self.assertGreaterEqual(_effective_max_bytes("n" * 600), 1)


class NumberedChunksTest(unittest.TestCase):
    def test_single_chunk_has_no_prefix(self):
        chunks = _numbered_chunks("alice", "short message")
        self.assertEqual(chunks, ["short message"])

    def test_multi_chunk_is_numbered_sequentially(self):
        text = "word " * 300  # ~1500 bytes -> several chunks at 400B budget
        chunks = _numbered_chunks("alice", text)
        self.assertGreater(len(chunks), 1)
        total = len(chunks)
        for i, chunk in enumerate(chunks, start=1):
            self.assertTrue(
                chunk.startswith(f"[{i}/{total}] "),
                f"chunk {i} missing marker: {chunk[:20]!r}",
            )

    def test_numbered_chunks_respect_byte_budget(self):
        budget = _effective_max_bytes("alice")
        for text in ("word " * 300, "é" * 900, "🐴 " * 400, "x" * 5000):
            for chunk in _numbered_chunks("alice", text):
                self.assertLessEqual(
                    len(chunk.encode("utf-8")), budget,
                    f"numbered chunk over budget: {len(chunk.encode('utf-8'))}",
                )

    def test_numbering_loses_no_content(self):
        text = "The quick brown fox\njumps over the lazy dog. " * 60
        chunks = _numbered_chunks("alice", text)
        joined = "".join(_strip_number_prefix(c) for c in chunks)
        self.assertEqual(_non_ws(joined), _non_ws(text))

    def test_numbering_converges_for_many_chunks(self):
        # Wide markers ([100/100] ) must still fit the budget.
        text = "a" * 45_000
        chunks = _numbered_chunks("alice", text)
        self.assertGreater(len(chunks), 99)
        budget = _effective_max_bytes("alice")
        for chunk in chunks:
            self.assertLessEqual(len(chunk.encode("utf-8")), budget)

    def test_host_hard_budget_fits_six_protocol_frames(self):
        # Rust DARKIRC_BUDGET allows 6 * 390 = 2,340 bytes. Spaces near
        # boundaries exercise natural splitting plus numbering overhead.
        text = ("a" * 390 + " ") * 5 + "b" * 385
        self.assertEqual(len(text.encode("utf-8")), 2_340)
        chunks = _numbered_chunks("alice", text)
        self.assertLessEqual(len(chunks), 6)
        for chunk in chunks:
            frame = f"PRIVMSG alice :{chunk}\r\n".encode("utf-8")
            self.assertLessEqual(len(frame), mod.IRC_FRAME_MAX_BYTES)

    def test_controls_are_normalized_before_framing(self):
        chunks = _numbered_chunks("alice", "hello\r\nPRIVMSG victim :owned\x00")
        self.assertEqual(len(chunks), 1)
        self.assertFalse(any(mod._CTRL_RE.search(chunk) for chunk in chunks))


class TargetValidationTest(unittest.TestCase):
    def test_valid_nicks(self):
        for target in ("alice", "Alice-2", "[relay]", "bridge_bot"):
            self.assertTrue(_valid_irc_target(target), target)

    def test_rejects_protocol_and_non_nick_targets(self):
        for target in (
            "bad nick",
            "bad\r\nPRIVMSG victim :owned",
            "bad\x00nick",
            "#channel",
            "9starts-with-digit",
            "n" * 33,
        ):
            self.assertFalse(_valid_irc_target(target), repr(target))


class EnqueueTest(unittest.TestCase):
    def setUp(self):
        mod.outbox.clear()
        self._max = mod.MAX_OUTBOX
        self._journal = mod.OUTBOX_JOURNAL
        mod.OUTBOX_JOURNAL = ""

    def tearDown(self):
        mod.outbox.clear()
        mod.MAX_OUTBOX = self._max
        mod.OUTBOX_JOURNAL = self._journal

    def test_enqueue_preserves_fifo_order(self):
        ok1, info1 = enqueue_send("alice", "first")
        ok2, info2 = enqueue_send("bob", "second")
        self.assertTrue(ok1 and ok2)
        self.assertEqual(info1["status"], "queued")
        self.assertEqual(
            [(i["to"], i["text"]) for i in mod.outbox],
            [("alice", "first"), ("bob", "second")],
        )

    def test_enqueue_rejects_when_outbox_full(self):
        mod.MAX_OUTBOX = 3
        ok, _ = enqueue_send("alice", "one")
        self.assertTrue(ok)
        # ~1500 bytes -> more than 2 chunks, exceeding the remaining capacity.
        ok2, info = enqueue_send("alice", "word " * 300)
        self.assertFalse(ok2)
        self.assertEqual(info["error"], "outbox full")
        # The rejected message must not be partially queued.
        self.assertEqual(len(mod.outbox), 1)

    def test_rejection_is_all_or_nothing(self):
        mod.MAX_OUTBOX = 1
        ok, info = enqueue_send("alice", "word " * 300)
        self.assertFalse(ok)
        self.assertEqual(len(mod.outbox), 0)


class JournalTest(unittest.TestCase):
    def setUp(self):
        mod.outbox.clear()
        self._journal = mod.OUTBOX_JOURNAL
        self._tmp = tempfile.TemporaryDirectory()
        mod.OUTBOX_JOURNAL = os.path.join(self._tmp.name, "outbox.json")

    def tearDown(self):
        mod.outbox.clear()
        mod.OUTBOX_JOURNAL = self._journal
        self._tmp.cleanup()

    def test_journal_round_trip(self):
        enqueue_send("alice", "persist me")
        enqueue_send("bob", "me too")
        saved = list(mod.outbox)
        # Simulate a restart: drop in-memory state, reload from journal.
        mod.outbox.clear()
        mod._journal_load()
        self.assertEqual(list(mod.outbox), saved)

    def test_journal_created_with_owner_only_permissions(self):
        ok, _ = enqueue_send("alice", "private")
        self.assertTrue(ok)
        mode = stat.S_IMODE(os.stat(mod.OUTBOX_JOURNAL).st_mode)
        self.assertEqual(mode, 0o600)

    def test_enqueue_rolls_back_when_journal_write_fails(self):
        mod.OUTBOX_JOURNAL = os.path.join(self._tmp.name, "missing", "outbox.json")
        ok, info = enqueue_send("alice", "must not claim queued")
        self.assertFalse(ok)
        self.assertEqual(info["error"], "outbox journal write failed")
        self.assertEqual(list(mod.outbox), [])

    def test_journal_fsyncs_file_then_parent_directory(self):
        real_fsync = os.fsync
        fsynced_modes = []

        def record_fsync(fd):
            fsynced_modes.append(os.fstat(fd).st_mode)
            real_fsync(fd)

        with mock.patch.object(mod.os, "fsync", side_effect=record_fsync):
            ok, _ = enqueue_send("alice", "durable")

        self.assertTrue(ok)
        self.assertEqual(len(fsynced_modes), 2)
        self.assertTrue(stat.S_ISREG(fsynced_modes[0]))
        self.assertTrue(stat.S_ISDIR(fsynced_modes[1]))

    def test_failed_write_removes_unique_temp_file(self):
        with mock.patch.object(mod.json, "dump", side_effect=OSError("write failed")):
            ok, _ = enqueue_send("alice", "rollback")

        self.assertFalse(ok)
        self.assertEqual(os.listdir(self._tmp.name), [])

    def test_journal_corrupt_file_starts_empty(self):
        with open(mod.OUTBOX_JOURNAL, "w", encoding="utf-8") as fh:
            fh.write("{not json")
        mod._journal_load()
        self.assertEqual(len(mod.outbox), 0)

    def test_journal_skips_malformed_entries(self):
        with open(mod.OUTBOX_JOURNAL, "w", encoding="utf-8") as fh:
            json.dump(
                [
                    {"to": "alice", "text": "good"},
                    {"to": "", "text": "no target"},
                    {"text": "no to"},
                    "not a dict",
                ],
                fh,
            )
        mod._journal_load()
        self.assertEqual(list(mod.outbox), [{"to": "alice", "text": "good"}])


class PendingAckTest(unittest.TestCase):
    def setUp(self):
        mod.pending_batch_id = None
        mod.message_queue.clear()
        mod.pending_ack.clear()

    def tearDown(self):
        mod.pending_batch_id = None
        mod.message_queue.clear()
        mod.pending_ack.clear()

    def test_fresh_messages_wait_behind_unacked_batch(self):
        old = {"from": "alice", "text": "unacked", "ts": "old"}
        fresh = {"from": "bob", "text": "fresh", "ts": "new"}
        mod.pending_ack.append(old)
        mod.message_queue.append(fresh)

        self.assertEqual(_poll_messages(), [old])
        batch_id = mod.pending_batch_id
        self.assertIsInstance(batch_id, str)
        self.assertEqual(list(mod.pending_ack), [old])
        self.assertEqual(list(mod.message_queue), [fresh])

        self.assertTrue(_ack_batch(batch_id))
        self.assertEqual(_poll_messages(), [fresh])
        self.assertEqual(list(mod.message_queue), [])

    def test_delayed_ack_cannot_clear_newer_batch(self):
        mod.message_queue.append({"from": "alice", "text": "one", "ts": "1"})
        _poll_messages()
        old_batch_id = mod.pending_batch_id
        self.assertTrue(_ack_batch(old_batch_id))

        newer = {"from": "bob", "text": "two", "ts": "2"}
        mod.message_queue.append(newer)
        _poll_messages()
        new_batch_id = mod.pending_batch_id
        self.assertNotEqual(old_batch_id, new_batch_id)

        self.assertFalse(_ack_batch(old_batch_id))
        self.assertEqual(list(mod.pending_ack), [newer])
        self.assertTrue(_ack_batch(new_batch_id))
        self.assertEqual(list(mod.pending_ack), [])

    def test_duplicate_ack_does_not_clear_following_batch(self):
        mod.message_queue.append({"from": "alice", "text": "one", "ts": "1"})
        _poll_messages()
        batch_id = mod.pending_batch_id
        self.assertTrue(_ack_batch(batch_id))
        self.assertFalse(_ack_batch(batch_id))

    def test_poll_batches_never_exceed_host_emit_limit(self):
        for index in range(mod.MAX_POLL_BATCH + 1):
            mod.message_queue.append(
                {"from": "alice", "text": f"message-{index}", "ts": str(index)}
            )

        first = _poll_messages()
        first_batch_id = mod.pending_batch_id
        self.assertEqual(len(first), mod.MAX_POLL_BATCH)
        self.assertEqual(len(mod.message_queue), 1)
        self.assertEqual(_poll_messages(), first)
        self.assertEqual(mod.pending_batch_id, first_batch_id)

        self.assertTrue(_ack_batch(first_batch_id))
        second = _poll_messages()
        self.assertEqual(len(second), 1)
        self.assertEqual(second[0]["text"], f"message-{mod.MAX_POLL_BATCH}")


class FakeIRC:
    """Records PRIVMSG lines; can be told to fail sends."""

    def __init__(self):
        self.sent = []
        self.fail_next = 0
        self.connected = True
        self.send_times = []

    async def _send(self, line):
        if self.fail_next > 0:
            self.fail_next -= 1
            raise ConnectionError("simulated send failure")
        self.sent.append(line)
        self.send_times.append(asyncio.get_running_loop().time())


class OutboxLoopTest(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        mod.outbox.clear()
        self._interval = mod.PRIVMSG_MIN_INTERVAL
        self._irc = mod.irc
        self._running = mod._running
        self._journal = mod.OUTBOX_JOURNAL
        self._last = mod._last_privmsg_time
        mod.OUTBOX_JOURNAL = ""
        mod.PRIVMSG_MIN_INTERVAL = 0.05
        mod._last_privmsg_time = float("-inf")
        self.fake = FakeIRC()
        mod.irc = self.fake
        mod._running = True

    def tearDown(self):
        mod.outbox.clear()
        mod.PRIVMSG_MIN_INTERVAL = self._interval
        mod.irc = self._irc
        mod._running = self._running
        mod.OUTBOX_JOURNAL = self._journal
        mod._last_privmsg_time = self._last

    async def _drain(self, expected, timeout=5.0):
        task = asyncio.create_task(mod.outbox_loop())
        try:
            deadline = asyncio.get_running_loop().time() + timeout
            while len(self.fake.sent) < expected:
                if asyncio.get_running_loop().time() > deadline:
                    self.fail(
                        f"drain timed out: {len(self.fake.sent)}/{expected} sent"
                    )
                await asyncio.sleep(0.01)
        finally:
            mod._running = False
            task.cancel()
            try:
                await task
            except asyncio.CancelledError:
                pass

    async def test_drains_in_order_with_pacing(self):
        enqueue_send("alice", "one")
        enqueue_send("alice", "two")
        enqueue_send("bob", "three")
        await self._drain(3)
        self.assertEqual(
            self.fake.sent,
            [
                "PRIVMSG alice :one",
                "PRIVMSG alice :two",
                "PRIVMSG bob :three",
            ],
        )
        # Consecutive sends must be spaced by at least the pacing interval
        # (small tolerance for event-loop timer jitter).
        gaps = [
            b - a
            for a, b in zip(self.fake.send_times, self.fake.send_times[1:])
        ]
        for gap in gaps:
            self.assertGreaterEqual(gap, mod.PRIVMSG_MIN_INTERVAL * 0.8)
        self.assertEqual(len(mod.outbox), 0)

    async def test_failed_send_is_retried_not_dropped(self):
        self.fake.fail_next = 2
        enqueue_send("alice", "survive failures")
        await self._drain(1)
        self.assertEqual(self.fake.sent, ["PRIVMSG alice :survive failures"])

    async def test_waits_for_irc_connection(self):
        self.fake.connected = False
        enqueue_send("alice", "wait for irc")
        task = asyncio.create_task(mod.outbox_loop())
        try:
            await asyncio.sleep(0.2)
            self.assertEqual(self.fake.sent, [])
            self.assertEqual(len(mod.outbox), 1)
            self.fake.connected = True
            deadline = asyncio.get_running_loop().time() + 5.0
            while not self.fake.sent:
                if asyncio.get_running_loop().time() > deadline:
                    self.fail("chunk not delivered after reconnect")
                await asyncio.sleep(0.01)
        finally:
            mod._running = False
            task.cancel()
            try:
                await task
            except asyncio.CancelledError:
                pass


if __name__ == "__main__":
    unittest.main(verbosity=2)
