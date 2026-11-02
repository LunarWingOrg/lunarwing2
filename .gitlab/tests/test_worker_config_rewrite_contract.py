"""Bind the WI#529 baked-config rewrite contract to its consumers.

c437692e9 made /app/config immutable by construction. Three independent
consumers of the opencode v1 config carried a "my copy is writable"
assumption (the model/baseURL rewrite, the Paseo injector, the Study Notes
injector) and nothing bound them to the hardening. These tests are that
binding: static assertions read the real Dockerfile and task-setup script,
and the live cases execute the real script against a 0444 fixture, as the
task uid when the runner is root.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
LANE = ROOT / "opencode4lunarwing"
SETUP = LANE / "entrypoint-task-setup.sh"
DOCKERFILE = LANE / "Dockerfile"
KIMI_SETUP = ROOT / "kimicode4lunarwing" / "entrypoint-task-setup.sh"
KIMI_DOCKERFILE = ROOT / "kimicode4lunarwing" / "Dockerfile"
TASK_UID = 10001
PLACEHOLDER = "http://set-opencode-base-url.invalid/openai/v1/"
ENDPOINT = "https://gateway.example.test/openai/v1/"
REWRITE_MARKERS = ("materialize_config_copy", "ensure_config_rewritable")
HELPERS = re.compile(
    r"materialize_config_copy\(\)\s*\{.*?\n\}\n\nensure_config_rewritable\(\)\s*\{.*?\n\}\n",
    re.DOTALL,
)


def write_call_sites(script: str) -> list[int]:
    """Offsets of every in-place write of a workspace config."""
    return [match.start() for match in re.finditer(r'with open\([^)]*,\s*"w"\)', script)]


class StaticContractTests(unittest.TestCase):
    def test_every_in_place_config_write_is_gated_by_the_helper(self) -> None:
        script = SETUP.read_text()
        sites = write_call_sites(script)
        self.assertGreaterEqual(len(sites), 2, "expected the model and Paseo writers")
        for offset in sites:
            with self.subTest(offset=offset):
                window = script[max(0, offset - 2500):offset]
                self.assertTrue(
                    any(marker in window for marker in REWRITE_MARKERS),
                    "in-place config write is not preceded by a rewrite-contract helper",
                )

    def test_copy_path_no_longer_duplicates_the_chmod(self) -> None:
        script = SETUP.read_text()
        self.assertIn(
            'materialize_config_copy /app/config/opencode.json \\\n'
            '      "$WORKSPACE_ROOT/.opencode/opencode.json"',
            script,
        )
        self.assertNotIn("cp /app/config/opencode.json", script)

    def test_injectors_call_the_helper_independently(self) -> None:
        script = SETUP.read_text()
        self.assertEqual(script.count("ensure_config_rewritable /app/config/opencode.json"), 2)
        study = script.find("study_notes_config.py")
        self.assertGreater(study, 0)
        self.assertIn(
            "ensure_config_rewritable",
            script[max(0, study - 800):study],
            "the Study Notes injector must bind to the contract before rewriting",
        )

    def test_dockerfile_gate_names_the_rewrite_contract(self) -> None:
        dockerfile = DOCKERFILE.read_text()
        self.assertIn("chmod -R a-w /app/config", dockerfile)
        self.assertIn("WI#529 REWRITE CONTRACT", dockerfile)
        self.assertIn("test_worker_config_rewrite_contract.py", dockerfile)

    def test_sentinel_alias_is_bound_to_the_baked_original(self) -> None:
        script = SETUP.read_text()
        self.assertIn("WI#529 consumer (a)", script)
        self.assertIn('readlink -f "$runtime_config"', script)

    def test_kimi_lane_never_rewrites_its_baked_source(self) -> None:
        script = KIMI_SETUP.read_text()
        self.assertIn('open(source, "r"', script)
        self.assertIsNone(re.search(r'open\(source,\s*"w"', script))
        self.assertIn("chmod -R a-w /app/config", KIMI_DOCKERFILE.read_text())


class LiveBootTests(unittest.TestCase):
    """Execute the real task setup with the image's 0444 baked config."""

    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="opencode-v1-boot-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.root.chmod(0o755)
        self.app = self.root / "app"
        self.home = self.root / "home"
        self.workspace = self.root / "workspace"
        for path in (self.app / "config", self.app / "scripts", self.home, self.workspace):
            path.mkdir(parents=True)
        self.source = self.app / "config" / "opencode.json"
        self.target = self.workspace / ".opencode" / "opencode.json"
        shutil.copyfile(LANE / "scripts" / "study_notes_config.py",
                        self.app / "scripts" / "study_notes_config.py")
        self.env = {
            "PATH": os.environ["PATH"],
            "HOME": str(self.home),
            "TMPDIR": str(self.root),
            "WORKSPACE_ROOT": str(self.workspace),
            "FILE_UMASK": "0002",
        }
        self.identity: dict = {}
        if os.geteuid() == 0:
            if shutil.which("setpriv") is None:
                self.skipTest("root without setpriv cannot drop to the task uid")
            for path in (self.home, self.workspace):
                os.chown(path, TASK_UID, TASK_UID)
            self.identity = {"user": TASK_UID, "group": TASK_UID, "extra_groups": []}

    def bake(self, text: str) -> str:
        self.source.write_text(text)
        self.source.chmod(0o444)
        self.source.parent.chmod(0o555)
        return text

    def boot(self, **overrides: str) -> subprocess.CompletedProcess:
        script = SETUP.read_text()
        script = script.replace("/app/", str(self.app) + "/")
        script = script.replace("/home/opencode/", str(self.home) + "/")
        script = 'test "$EUID" -ne 0 || exit 99\n' + script
        environment = {**self.env, **overrides}
        return subprocess.run(
            ["bash"], input=script, text=True, capture_output=True, timeout=30,
            env=environment, cwd=self.root, **self.identity,
        )

    def baked(self) -> str:
        return (LANE / "config" / "opencode.json").read_text()

    def test_materialized_copy_is_writable_and_source_untouched(self) -> None:
        original = self.bake(self.baked().replace(PLACEHOLDER, ENDPOINT))
        result = self.boot(OPENCODE_MODEL="fixture-model", OPENCODE_BASE_URL=ENDPOINT)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.source.read_text(), original)
        self.assertEqual(self.source.stat().st_mode & 0o777, 0o444)
        self.assertFalse(self.target.is_symlink())
        self.assertTrue(self.target.stat().st_mode & 0o200)
        config = json.loads(self.target.read_text())
        self.assertTrue(config["model"].endswith("/fixture-model"))
        provider = next(iter(config["provider"].values()))
        self.assertEqual(provider["options"]["baseURL"], ENDPOINT)

    def test_paseo_only_materializes_a_writable_copy(self) -> None:
        self.bake(self.baked().replace(PLACEHOLDER, ENDPOINT))
        result = self.boot(PASEO_URL="https://paseo.example.test/", PASEO_TOKEN="fixture")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        config = json.loads(self.target.read_text())
        self.assertEqual(config["mcp"]["paseo"]["url"], "https://paseo.example.test/mcp/agents")
        self.assertTrue(self.target.stat().st_mode & 0o200)
        self.assertEqual(self.source.stat().st_mode & 0o777, 0o444)

    def test_ensure_helper_replaces_an_alias_and_keeps_a_writable_copy(self) -> None:
        self.bake(self.baked().replace(PLACEHOLDER, ENDPOINT))
        helper = HELPERS.search(SETUP.read_text())
        self.assertIsNotNone(helper)
        script = (
            "set -euo pipefail\n"
            + helper.group(0)
            + f'\nensure_config_rewritable {self.source} {self.target}\n'
            + 'test ! -L "$2" && test -w "$2"\n'
        )
        script = script.replace("/home/opencode/", str(self.home) + "/")
        (self.workspace / ".opencode").mkdir()
        self.target.symlink_to(self.source)
        result = subprocess.run(
            ["bash", "-c", script, "helper", str(self.source), str(self.target)],
            text=True, capture_output=True, timeout=15, env=self.env,
            cwd=self.root, **self.identity,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(self.target.is_symlink())
        self.assertEqual(self.source.read_text(), self.baked().replace(PLACEHOLDER, ENDPOINT))

    def test_study_notes_only_materializes_over_readonly_source(self) -> None:
        self.bake(self.baked().replace(PLACEHOLDER, ENDPOINT))
        study = self.root / "study-notes"
        (study / "agent-notes-by-agent").mkdir(parents=True)
        (study / "skills" / "study-notes").mkdir(parents=True)
        for name in ("study_notes_mcp.py", "memorydb.py", "skills/study-notes/SKILL.md"):
            (study / name).write_text("fixture\n")
        with sqlite3.connect(study / "agent-notes.sqlite3") as database:
            database.execute("CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            database.execute("INSERT INTO metadata VALUES ('schema_version', '2')")
            database.execute("CREATE VIRTUAL TABLE sections_fts USING fts5(body)")
        if self.identity:
            subprocess.run(["chown", "-R", f"{TASK_UID}:{TASK_UID}", str(study)], check=True)
        result = self.boot(LW2STUDYMCP_HOME=str(study))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        config = json.loads(self.target.read_text())
        self.assertIn("study_notes", config["mcp"])
        self.assertEqual(self.source.stat().st_mode & 0o777, 0o444)

    def test_placeholder_sentinel_fires_on_the_linked_path(self) -> None:
        self.bake(self.baked())
        result = self.boot()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("OPENCODE_BASE_URL is unset", result.stdout + result.stderr)
        self.assertTrue(self.target.is_symlink())
        self.assertEqual(self.target.resolve(), self.source.resolve())
        self.assertEqual(self.source.read_text(), self.baked())

    def test_linked_path_boot_succeeds_when_the_placeholder_is_gone(self) -> None:
        self.bake(self.baked().replace(PLACEHOLDER, ENDPOINT))
        result = self.boot()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(self.target.is_symlink())
        self.assertEqual(self.source.stat().st_mode & 0o777, 0o444)


if __name__ == "__main__":
    unittest.main()
