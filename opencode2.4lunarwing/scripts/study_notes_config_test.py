#!/usr/bin/env python3

import json
import os
import sqlite3
import subprocess
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
        self.create_database(self.home / "agent-notes.sqlite3")
        self.state = self.root / "study-state"
        (self.state / "agent-notes-by-agent").mkdir(parents=True)
        self.create_database(self.state / "agent-notes.sqlite3")
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

    @staticmethod
    def create_database(path: Path) -> None:
        connection = sqlite3.connect(path)
        try:
            connection.execute("CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            connection.execute("INSERT INTO metadata VALUES ('schema_version', '2')")
            connection.execute("CREATE VIRTUAL TABLE sections_fts USING fts5(body)")
            connection.commit()
        finally:
            connection.close()

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
        self.assertIn(str(self.home / "agent-notes.sqlite3"), study["command"])
        self.assertEqual(study["cwd"], str(self.home))

    def test_read_only_mode_uses_separate_state(self) -> None:
        study_notes_config.configure(self.config, self.home, self.state)
        result = json.loads(self.config.read_text(encoding="utf-8"))
        command = result["mcp"]["study_notes"]["command"]

        self.assertNotIn("--allow-write", command)
        self.assertIn(str(self.state / "agent-notes.sqlite3"), command)
        self.assertIn(str(self.state / "agent-notes-by-agent"), command)
        self.assertNotIn(str(self.home / "agent-notes.sqlite3"), command)

    def test_state_must_not_overlap_code(self) -> None:
        with self.assertRaisesRegex(ValueError, "must not overlap"):
            study_notes_config.configure(self.config, self.home, self.home)

    def test_state_remains_readable(self) -> None:
        study_notes_config.configure(self.config, self.home, self.state)
        command = json.loads(self.config.read_text(encoding="utf-8"))["mcp"]["study_notes"][
            "command"
        ]

        self.assertNotIn("--allow-write", command)
        self.assertIn(str(self.state / "agent-notes.sqlite3"), command)

    def test_symlinked_server_is_rejected(self) -> None:
        outside = self.root / "outside.py"
        outside.write_text("fixture\n", encoding="utf-8")
        server = self.home / "study_notes_mcp.py"
        server.unlink()
        server.symlink_to(outside)

        with self.assertRaisesRegex(ValueError, "must not be a symlink"):
            study_notes_config.configure(self.config, self.home)

    def test_entrypoint_rejects_worker_write_mode(self) -> None:
        # WI#449: the refusal lives in the task-uid setup the root launcher
        # delegates to via as_task — run the real launcher with /app
        # relocated to a fixture holding the real setup script.
        lane = Path(__file__).resolve().parent.parent
        entrypoint = (lane / "entrypoint.sh").read_text(encoding="utf-8")
        app = self.root / "app"
        app.mkdir()
        staged = app / "entrypoint-task-setup.sh"
        staged.write_text(
            (lane / "entrypoint-task-setup.sh").read_text(encoding="utf-8"),
            encoding="utf-8",
        )
        staged.chmod(0o755)
        result = subprocess.run(
            ["bash"],
            input=entrypoint.replace("/app/", str(app) + "/"),
            env={
                "HOME": str(self.root),
                "PATH": os.environ["PATH"],
                "TMPDIR": str(self.root),
                "WORKSPACE_ROOT": str(self.root / "workspace"),
                "LW2STUDYMCP_HOME": "/opt/lw2studymcp",
                "LW2STUDYMCP_ALLOW_WRITE": "1",
            },
            capture_output=True,
            text=True,
            check=False,
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("writes are not exposed inside OpenCode workers", result.stderr)

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
