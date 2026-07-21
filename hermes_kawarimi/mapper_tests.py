"""Unit tests for the pure mapper. No root/DB required."""

from __future__ import annotations

import json
import unittest

from hermes_kawarimi.mapper import map_agent
from hermes_kawarimi.model import (
    HermesAgentSnapshot,
    HermesMessage,
    HermesSession,
)


def _snapshot() -> HermesAgentSnapshot:
    snap = HermesAgentSnapshot(source_path="/x")
    snap.markdown_docs = {
        "SOUL.md": "persona",
        "MEMORY.md": "notes",
        "USER.md": "prefs",
    }
    session = HermesSession(
        id="sess-1",
        source="telegram",
        started_at=1700000000.0,
        ended_at=1700000100.0,
        thread_id="thread-9",
        title="chat",
        model="sonnet",
    )
    session.messages = [
        HermesMessage(id=1, role="USER", content="hello", timestamp=1700000001.0),
        HermesMessage(id=2, role="assistant", content=None, tool_calls='{"t":1}', timestamp=1700000002.0),
        HermesMessage(id=3, role="tool", content=None, tool_calls=None, timestamp=1700000003.0),
    ]
    snap.sessions = [session]
    snap.env = {
        "ANTHROPIC_API_KEY": "sk-a",
        "OPENAI_API_KEY": "sk-o",
        "CUSTOM_TOKEN": "tok",
        "MODEL": "gpt",  # not a secret
        "EMPTY_API_KEY": "",  # skipped (no value)
    }
    snap.auth = {"anthropic": {"token": "t"}}
    snap.config = {"model": "sonnet", "temperature": 0.7, "nested": {"a": 1}}
    return snap


class MemoryMappingTest(unittest.TestCase):
    def test_markdown_and_config_reference(self) -> None:
        mapped = map_agent(_snapshot())
        paths = {d.path for d in mapped.memory_docs}
        self.assertIn("SOUL.md", paths)
        self.assertIn("MEMORY.md", paths)
        self.assertIn("USER.md", paths)
        self.assertIn("imported/hermes-config.json", paths)
        ref = next(d for d in mapped.memory_docs if d.path == "imported/hermes-config.json")
        self.assertEqual(json.loads(ref.content)["model"], "sonnet")


class ConversationMappingTest(unittest.TestCase):
    def test_conversation_fields_and_content_coalescing(self) -> None:
        mapped = map_agent(_snapshot())
        self.assertEqual(len(mapped.conversations), 1)
        conv = mapped.conversations[0]
        self.assertEqual(conv.channel, "telegram")
        self.assertEqual(conv.thread_id, "thread-9")
        self.assertEqual(conv.metadata["hermes_session_id"], "sess-1")
        self.assertEqual(len(conv.messages), 3)
        # role lowercased
        self.assertEqual(conv.messages[0].role, "user")
        self.assertEqual(conv.messages[0].content, "hello")
        # None content + tool_calls -> tool_calls string
        self.assertEqual(conv.messages[1].content, '{"t":1}')
        # both None -> ""
        self.assertEqual(conv.messages[2].content, "")

    def test_uuid_determinism(self) -> None:
        a = map_agent(_snapshot())
        b = map_agent(_snapshot())
        self.assertEqual(a.conversations[0].id, b.conversations[0].id)
        self.assertEqual(
            [m.id for m in a.conversations[0].messages],
            [m.id for m in b.conversations[0].messages],
        )
        # conversation id differs from message ids
        self.assertNotIn(a.conversations[0].id, {m.id for m in a.conversations[0].messages})


class SecretMappingTest(unittest.TestCase):
    def test_classification_and_providers(self) -> None:
        mapped = map_agent(_snapshot())
        by_name = {s.name: s for s in mapped.secrets}
        self.assertIn("anthropic_api_key", by_name)
        self.assertEqual(by_name["anthropic_api_key"].provider, "anthropic")
        self.assertIn("openai_api_key", by_name)
        self.assertIn("custom_token", by_name)  # TOKEN hint
        self.assertIn("hermes_auth_json", by_name)  # auth.json carried whole
        self.assertNotIn("model", by_name)  # not a secret
        self.assertNotIn("empty_api_key", by_name)  # empty value skipped

    def test_auth_json_roundtrips(self) -> None:
        mapped = map_agent(_snapshot())
        auth = next(s for s in mapped.secrets if s.name == "hermes_auth_json")
        self.assertEqual(json.loads(auth.value)["anthropic"]["token"], "t")


class SettingsMappingTest(unittest.TestCase):
    def test_scalar_config_namespaced(self) -> None:
        mapped = map_agent(_snapshot())
        keys = {s.key for s in mapped.settings}
        self.assertIn("hermes.model", keys)
        self.assertIn("hermes.temperature", keys)
        self.assertNotIn("hermes.nested", keys)  # nested dicts stay in the ref doc


if __name__ == "__main__":
    unittest.main()
