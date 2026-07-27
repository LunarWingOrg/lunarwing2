#!/usr/bin/env python3

import json
import sqlite3
import tempfile
import unittest
from pathlib import Path

import study_notes_config


class StudyNotesConfigTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.home = self.root / "study-notes"
        (self.home / "agent-notes-by-agent").mkdir(parents=True)
        (self.home / "skills" / "study-notes").mkdir(parents=True)
        for relative in (
            "study_notes_mcp.py",
            "memorydb.py",
            "skills/study-notes/SKILL.md",
        ):
            (self.home / relative).write_text("fixture\n", encoding="utf-8")
        connection = sqlite3.connect(self.home / "agent-notes.sqlite3")
        try:
            connection.execute("CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            connection.execute("INSERT INTO metadata VALUES ('schema_version', '2')")
            connection.execute("CREATE VIRTUAL TABLE sections_fts USING fts5(body)")
            connection.commit()
        finally:
            connection.close()
        self.config = self.root / "opencode.json"
        self.config.write_text(
            json.dumps(
                {
                    "skills": {"paths": ["/existing/skills"]},
                    "mcp": {"existing": {"type": "remote", "url": "https://example.test/mcp"}},
                }
            ),
            encoding="utf-8",
        )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_configure_preserves_existing_and_adds_read_only_study_mcp(self) -> None:
        study_notes_config.configure(self.config, self.home)
        result = json.loads(self.config.read_text(encoding="utf-8"))

        self.assertIn("/existing/skills", result["skills"]["paths"])
        self.assertIn(str(self.home / "skills"), result["skills"]["paths"])
        self.assertIn("existing", result["mcp"])
        study = result["mcp"]["study_notes"]
        self.assertEqual(study["type"], "local")
        self.assertTrue(study["enabled"])
        self.assertNotIn("--allow-write", study["command"])
        self.assertEqual(study["cwd"], str(self.home))

    def test_missing_checkout_file_is_rejected_without_changing_config(self) -> None:
        original = self.config.read_bytes()
        (self.home / "study_notes_mcp.py").unlink()
        with self.assertRaisesRegex(ValueError, "study_notes_mcp.py"):
            study_notes_config.configure(self.config, self.home)
        self.assertEqual(self.config.read_bytes(), original)

    def test_relative_home_is_rejected(self) -> None:
        with self.assertRaisesRegex(ValueError, "absolute"):
            study_notes_config.validate_home(Path("relative"))


if __name__ == "__main__":
    unittest.main()
