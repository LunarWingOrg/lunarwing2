"""Unit tests for extract.py against a synthetic Hermes home. No root/DB."""

from __future__ import annotations

import sqlite3
import tempfile
import unittest
from pathlib import Path

from hermes_kawarimi.extract import extract, resolve_source


def _make_hermes_home(root: Path) -> None:
    (root / "SOUL.md").write_text("I am the agent.", encoding="utf-8")
    mem = root / "memories"
    (mem / "notes").mkdir(parents=True)
    (mem / "MEMORY.md").write_text("§ remembered a thing", encoding="utf-8")
    (mem / "USER.md").write_text("§ user likes terse replies", encoding="utf-8")
    (mem / "notes" / "foo.md").write_text("misc note", encoding="utf-8")
    (root / ".env").write_text(
        'ANTHROPIC_API_KEY="sk-abc123"\nMODEL=gpt\n# comment\n', encoding="utf-8"
    )
    (root / "auth.json").write_text('{"anthropic": {"token": "t"}}', encoding="utf-8")
    (root / "config.yaml").write_text("model: sonnet\nagent_name: kage\n", encoding="utf-8")
    _make_state_db(root / "state.db")


def _make_state_db(path: Path) -> None:
    conn = sqlite3.connect(str(path))
    conn.executescript(
        """
        CREATE TABLE sessions (
            id TEXT PRIMARY KEY, source TEXT NOT NULL, user_id TEXT,
            chat_id TEXT, thread_id TEXT, title TEXT, model TEXT,
            profile_name TEXT, started_at REAL NOT NULL, ended_at REAL,
            archived INTEGER DEFAULT 0
        );
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
            role TEXT NOT NULL, content TEXT, tool_calls TEXT, tool_name TEXT,
            timestamp REAL NOT NULL
        );
        """
    )
    conn.execute(
        "INSERT INTO sessions (id, source, started_at, ended_at, title) "
        "VALUES ('sess-1', 'cli', 1700000000.0, 1700000100.0, 'first')"
    )
    conn.executemany(
        "INSERT INTO messages (session_id, role, content, timestamp) VALUES (?,?,?,?)",
        [
            ("sess-1", "user", "hello", 1700000001.0),
            ("sess-1", "assistant", "hi there", 1700000002.0),
        ],
    )
    conn.commit()
    conn.close()


class ResolveSourceTest(unittest.TestCase):
    def test_explicit_arg_wins(self) -> None:
        self.assertEqual(resolve_source("/tmp/foo"), Path("/tmp/foo"))

    def test_missing_dir_yields_warning(self) -> None:
        snap = extract("/nonexistent/hermes/home/xyz")
        self.assertTrue(any("not found" in w for w in snap.warnings))


class ExtractTest(unittest.TestCase):
    def test_reads_full_agent(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _make_hermes_home(root)
            snap = extract(str(root))

        self.assertIn("SOUL.md", snap.markdown_docs)
        self.assertIn("MEMORY.md", snap.markdown_docs)
        self.assertIn("USER.md", snap.markdown_docs)
        self.assertIn("memories/notes/foo.md", snap.markdown_docs)

        self.assertEqual(len(snap.sessions), 1)
        self.assertEqual(snap.sessions[0].id, "sess-1")
        self.assertEqual(snap.message_count, 2)
        self.assertEqual(snap.sessions[0].messages[0].content, "hello")

        self.assertEqual(snap.env.get("ANTHROPIC_API_KEY"), "sk-abc123")
        self.assertEqual(snap.env.get("MODEL"), "gpt")
        self.assertEqual(snap.auth.get("anthropic"), {"token": "t"})

    def test_config_parsed_when_yaml_available(self) -> None:
        try:
            import yaml  # noqa: F401
        except ImportError:
            self.skipTest("pyyaml not installed")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _make_hermes_home(root)
            snap = extract(str(root))
        self.assertEqual(snap.config.get("model"), "sonnet")
        self.assertEqual(snap.config.get("agent_name"), "kage")


if __name__ == "__main__":
    unittest.main()
