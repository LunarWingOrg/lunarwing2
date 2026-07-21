"""Unit tests for the model dataclasses. No root or DB required."""

from __future__ import annotations

import unittest
from datetime import datetime, timezone

from hermes_kawarimi.model import (
    HermesAgentSnapshot,
    HermesMessage,
    HermesSession,
    MappedAgent,
    MappedConversation,
    MappedMemoryDoc,
    MappedMessage,
)


class SnapshotCountsTest(unittest.TestCase):
    def test_message_count_sums_sessions(self) -> None:
        snap = HermesAgentSnapshot(source_path="/x")
        s1 = HermesSession(id="a", source="cli", started_at=1.0)
        s1.messages = [
            HermesMessage(id=1, role="user", content="hi", timestamp=1.0),
            HermesMessage(id=2, role="assistant", content="yo", timestamp=2.0),
        ]
        s2 = HermesSession(id="b", source="cli", started_at=3.0)
        s2.messages = [HermesMessage(id=3, role="user", content="q", timestamp=3.0)]
        snap.sessions = [s1, s2]
        self.assertEqual(snap.message_count, 3)


class MappedCountsTest(unittest.TestCase):
    def test_counts_and_message_count(self) -> None:
        now = datetime(2024, 1, 1, tzinfo=timezone.utc)
        conv = MappedConversation(
            id="c1", channel="cli", thread_id=None, started_at=now, last_activity=now
        )
        conv.messages = [
            MappedMessage(id="m1", conversation_id="c1", role="user", content="hi", created_at=now)
        ]
        agent = MappedAgent(
            memory_docs=[MappedMemoryDoc(path="SOUL.md", content="x")],
            conversations=[conv],
        )
        self.assertEqual(agent.message_count, 1)
        self.assertEqual(
            agent.counts(),
            {
                "memory_docs": 1,
                "conversations": 1,
                "messages": 1,
                "secrets": 0,
                "settings": 0,
            },
        )


if __name__ == "__main__":
    unittest.main()
