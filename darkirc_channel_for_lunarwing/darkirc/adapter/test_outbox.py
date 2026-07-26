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
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import darkirc_adapter as mod
from darkirc_adapter import (
    _effective_max_bytes,
    _numbered_chunks,
    _split_message_bytes,
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
        self.assertLessEqual(frame, 512)
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
