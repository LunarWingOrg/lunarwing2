#!/usr/bin/env python3
"""Run the real boot configuration as non-root with the image's 0444 source.

Only container paths are relocated; stop before health/server startup. Root
CI/builds drop to uid 10003, while developer runs use their non-root uid.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile
import unittest


LANE = Path(__file__).resolve().parent.parent
ENDPOINT = "https://gateway.example.test/openai/v1/"


class EntrypointConfigTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="opencode2-boot-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.root.chmod(0o755)
        self.app = self.root / "app"
        self.home = self.root / "home"
        self.workspace = self.root / "workspace"
        for path in (self.app / "config", self.app / "scripts", self.home, self.workspace):
            path.mkdir(parents=True)
        self.source = self.app / "config/opencode2.json"
        self.baked = (LANE / "config/opencode2.json").read_text().replace(
            "http://set-opencode2-base-url.invalid/openai/v1/", ENDPOINT,
        )
        self.source.write_text(self.baked)
        self.source.chmod(0o444)
        self.source.parent.chmod(0o555)
        shutil.copyfile(LANE / "scripts/study_notes_config.py",
                        self.app / "scripts/study_notes_config.py")
        self.target = self.workspace / ".opencode/opencode2.json"
        self.env = {
            "PATH": os.environ["PATH"], "HOME": str(self.home),
            "TMPDIR": str(self.root), "WORKSPACE_ROOT": str(self.workspace),
        }
        self.identity = {}
        if os.geteuid() == 0:
            for path in (self.home, self.workspace):
                os.chown(path, 10003, 10003)
            self.identity = {"user": 10003, "group": 10003, "extra_groups": []}

    def boot(self, **overrides: str) -> dict:
        entrypoint = (LANE / "entrypoint.sh").read_text()
        boundary = "# ── always start the health server"
        self.assertIn(boundary, entrypoint)
        script = entrypoint.split(boundary, 1)[0]
        script = script.replace("/app/", str(self.app) + "/")
        script = script.replace("/home/opencode2/", str(self.home) + "/")
        # Do not let a root CI invocation accidentally bypass the permissions
        # regression this harness exists to catch.
        script = 'test "$EUID" -ne 0 || exit 99\n' + script
        result = subprocess.run(
            ["bash"], input=script, text=True, capture_output=True, timeout=15,
            env={**self.env, **overrides}, cwd=self.root, **self.identity,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.source.stat().st_mode & 0o777, 0o444)
        self.assertEqual(self.source.read_text(), self.baked)
        return json.loads(self.target.read_text())

    def test_managed_overrides_work_on_first_boot_and_restart(self) -> None:
        service = self.home / ".config/opencode/service.json"
        overrides = {
            "OPENCODE2_BASE_URL": "https://override.example.test/v1/",
            "OPENCODE2_MODEL": "test-model",
            "PASEO_URL": "https://paseo.example.test/",
            "PASEO_TOKEN": "fixture-token",
        }
        for boot in range(2):
            with self.subTest(boot=boot):
                config = self.boot(**overrides)
                provider = next(iter(config["providers"].values()))
                self.assertEqual(provider["settings"]["baseURL"], overrides["OPENCODE2_BASE_URL"])
                self.assertTrue(config["model"].endswith("/test-model"))
                self.assertEqual(config["mcp"]["paseo"]["url"], "https://paseo.example.test/mcp/agents")
                self.assertEqual(config["share"], "disabled")
                self.assertEqual(config["update"], "disable")
                self.assertTrue(self.target.stat().st_mode & 0o200)
                self.assertEqual(json.loads(service.read_text()), {"disabled": True})
                # Simulate the mutable files left behind by an earlier boot.
                self.target.write_text('{"share": "auto", "update": "enable"}')
                service.write_text('{"disabled": false, "hostname": "0.0.0.0"}')

    def test_paseo_only_can_edit_the_materialized_config(self) -> None:
        config = self.boot(PASEO_URL="https://paseo.example.test", PASEO_TOKEN="fixture-token")
        self.assertEqual(config["mcp"]["paseo"]["headers"]["Authorization"], "Bearer fixture-token")
        self.assertEqual(config["share"], "disabled")

    def test_service_default_uses_home_without_overrides(self) -> None:
        self.boot()
        self.assertTrue(self.target.is_symlink())
        service = self.home / ".config/opencode/service.json"
        self.assertEqual(json.loads(service.read_text()), {"disabled": True})

    def test_service_default_honours_xdg_config_home(self) -> None:
        xdg = self.home / "custom-config"
        self.boot(XDG_CONFIG_HOME=str(xdg))
        self.assertEqual(json.loads((xdg / "opencode/service.json").read_text()), {"disabled": True})
        self.assertFalse((self.home / ".config/opencode/service.json").exists())

    def test_study_notes_only_can_materialize_config(self) -> None:
        study = self.root / "study-notes"
        (study / "agent-notes-by-agent").mkdir(parents=True)
        (study / "skills/study-notes").mkdir(parents=True)
        for name in ("study_notes_mcp.py", "memorydb.py", "skills/study-notes/SKILL.md"):
            (study / name).write_text("fixture\n")
        with sqlite3.connect(study / "agent-notes.sqlite3") as db:
            db.execute("CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            db.execute("INSERT INTO metadata VALUES ('schema_version', '2')")
            db.execute("CREATE VIRTUAL TABLE sections_fts USING fts5(body)")
        config = self.boot(LW2STUDYMCP_HOME=str(study))
        self.assertEqual(config["share"], "disabled")
        self.assertNotIn("--allow-write", config["mcp"]["study_notes"]["command"])
        self.assertIn(str(study / "skills"), config["skills"]["paths"])


if __name__ == "__main__":
    unittest.main()
