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
import time
import unittest


LANE = Path(__file__).resolve().parent.parent
ENDPOINT = "https://gateway.example.test/openai/v1/"


class EntrypointConfigTests(unittest.TestCase):
    def setUp(self) -> None:
        # The CLI regression executes a fake binary; /tmp is noexec on some hosts.
        temporary = tempfile.TemporaryDirectory(prefix="opencode2-boot-", dir=LANE)
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

    def run_boot(self, **overrides: str) -> subprocess.CompletedProcess:
        entrypoint = (LANE / "entrypoint.sh").read_text()
        boundary = "# ── always start the health server"
        self.assertIn(boundary, entrypoint)
        script = entrypoint.split(boundary, 1)[0]
        script = script.replace("/app/", str(self.app) + "/")
        script = script.replace("/home/opencode2/", str(self.home) + "/")
        # Do not let a root CI invocation accidentally bypass the permissions
        # regression this harness exists to catch.
        script = 'test "$EUID" -ne 0 || exit 99\n' + script
        return subprocess.run(
            ["bash"], input=script, text=True, capture_output=True, timeout=15,
            env={**self.env, **overrides}, cwd=self.root, **self.identity,
        )

    def boot(self, **overrides: str) -> dict:
        result = self.run_boot(**overrides)
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
                # WI#427: v2.0.32+ stores remote as a route object and keeps
                # consent separately; a restart must discard both too.
                service.write_text(json.dumps({
                    "disabled": False, "hostname": "0.0.0.0",
                    "remote": {"route": "0123456789abcdef"}, "remoteAccepted": True,
                }))

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

    def test_service_reset_honours_opencode_config_dir(self) -> None:
        # FB-7: upstream reads the service config from $OPENCODE_CONFIG_DIR
        # when set, so the reset must land there, not in ~/.config/opencode.
        custom = self.root / "custom-config"
        custom.mkdir()
        if os.geteuid() == 0:
            os.chown(custom, 10003, 10003)
        self.boot(OPENCODE_CONFIG_DIR=str(custom))
        self.assertEqual(json.loads((custom / "service.json").read_text()), {"disabled": True})
        self.assertFalse((self.home / ".config/opencode/service.json").exists())

    def test_service_reset_survives_plants_at_the_service_path(self) -> None:
        # FB-6: a FIFO, directory, or symlink planted at the reset path must
        # not hang the boot, crash it, or redirect the write through the
        # symlink onto the agent's own file.
        agent_file = self.home / "agent-file.json"
        agent_file.write_text('{"agent": "kept"}')
        service = self.home / ".config/opencode/service.json"

        def own(path: Path) -> None:
            if os.geteuid() == 0:
                os.chown(path, 10003, 10003, follow_symlinks=False)

        for plant in ("fifo", "dir", "symlink"):
            with self.subTest(plant=plant):
                service.unlink(missing_ok=True)
                service.parent.mkdir(parents=True, exist_ok=True)
                own(service.parent)
                own(service.parent.parent)
                if plant == "fifo":
                    os.mkfifo(service)
                    own(service)
                elif plant == "dir":
                    service.mkdir()
                    (service / "planted").write_text("x")
                    own(service)
                else:
                    os.symlink(agent_file, service)
                    own(service)
                self.boot()
                self.assertFalse(service.is_symlink())
                self.assertTrue(service.is_file())
                self.assertEqual(json.loads(service.read_text()), {"disabled": True})
                if plant == "symlink":
                    self.assertEqual(agent_file.read_text(), '{"agent": "kept"}')

    def test_placeholder_alias_must_resolve_to_the_baked_config(self) -> None:
        # WI#529 consumer (a): the no-override path links the workspace config
        # at the immutable baked file and the placeholder sentinel reads
        # through that alias. An alias pointing anywhere else must fail loudly
        # rather than have the sentinel grep an unintended file.
        foreign = self.root / "foreign.json"
        foreign.write_text(json.dumps({"share": "disabled"}))
        alias = self.root / "alias.json"
        alias.symlink_to(foreign)
        if os.geteuid() == 0:
            os.chown(alias, 10003, 10003, follow_symlinks=False)
            os.chown(foreign, 10003, 10003)
        result = self.run_boot(OPENCODE_CONFIG=str(alias))
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("not the baked config", result.stdout + result.stderr)
        self.assertEqual(foreign.read_text(), json.dumps({"share": "disabled"}))

    def test_default_linked_config_is_accepted(self) -> None:
        # The same assertion must not refuse the real no-override alias.
        self.boot()
        self.assertTrue(self.target.is_symlink())
        self.assertEqual(self.target.resolve(), self.source.resolve())

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

    def prepare_cli(self, timeout_ms: str) -> tuple[Path, dict[str, str]]:
        """Build the full entrypoint around a CLI stuck in background_wait."""
        # The real health server is irrelevant here; exiting immediately keeps
        # the one-shot CLI process as the only child this test must reap.
        (self.app / "health_server.py").write_text("")
        fake_cli = self.root / "fake-opencode.py"
        fake_cli.write_text("""#!/usr/bin/env python3
import json
import os
import signal
import sys
import time

job = {"id": "sh_train", "kind": "shell", "title": "python train.py"}
def emit(event_type, timestamp):
    print(json.dumps({
        "type": event_type,
        "timestamp": timestamp,
        "sessionID": "ses_1",
        "jobs": [job],
        "credential_present": "AGENT_AUTH_TOKEN" in os.environ,
    }), flush=True)

# GNU timeout signals both the child and its process group, so a loaded host
# can enter this handler twice. Ignore repeats before writing; the self-signal
# makes that scheduling race deterministic for this regression test.
def cancelled(signum, frame):
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    os.kill(os.getpid(), signal.SIGTERM)
    emit("test_cancelled", 2)
    sys.exit(143)

signal.signal(signal.SIGTERM, cancelled)
emit("background_wait", 1)
while True:
    time.sleep(60)
""")
        fake_cli.chmod(0o755)
        entrypoint = (LANE / "entrypoint.sh").read_text()
        entrypoint = entrypoint.replace("/app/", str(self.app) + "/")
        entrypoint = entrypoint.replace("/home/opencode2/", str(self.home) + "/")
        script = self.root / "entrypoint-under-test.sh"
        script.write_text('test "$EUID" -ne 0 || exit 99\n' + entrypoint)
        env = {
            **self.env,
            "AGENT_AUTH_TOKEN": "fixture-hub-credential",
            "OPENCODE_MODE": "cli",
            "OPENCODE_BIN": str(fake_cli),
            "OPENCODE_RUN_TIMEOUT_MS": timeout_ms,
        }
        return script, env

    def run_cli(self, timeout_ms: str) -> subprocess.CompletedProcess:
        script, env = self.prepare_cli(timeout_ms)
        return subprocess.run(
            ["bash", str(script)], text=True, capture_output=True, timeout=15,
            env=env, cwd=self.root, **self.identity,
        )

    def test_cli_background_wait_is_bounded_and_cancelled(self) -> None:
        started = time.monotonic()
        result = self.run_cli("2000")
        self.assertEqual(result.returncode, 143, result.stdout + result.stderr)
        self.assertLess(time.monotonic() - started, 10)
        events = [
            json.loads(line) for line in result.stdout.splitlines()
            if line.startswith("{")
        ]
        self.assertEqual(events[0]["type"], "background_wait")
        self.assertEqual(events[0]["sessionID"], "ses_1")
        self.assertEqual(events[0]["jobs"][0]["id"], "sh_train")
        self.assertFalse(events[0]["credential_present"])
        self.assertEqual(events[-1]["type"], "test_cancelled")

    def test_cli_rejects_an_unbounded_or_invalid_timeout(self) -> None:
        for value in ("0", "-1", "1.5", "unbounded"):
            with self.subTest(value=value):
                result = self.run_cli(value)
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("OPENCODE_RUN_TIMEOUT_MS must be a positive integer", result.stderr)
                self.assertNotIn("background_wait", result.stdout)


if __name__ == "__main__":
    unittest.main()
