#!/usr/bin/env python3
"""Exercise the snapshot apt retry helper without contacting Debian."""

import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".gitlab/scripts/ci-apt-with-retry.sh"


class CiAptWithRetryTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(dir=ROOT)
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for name in ("sh", "grep", "tee", "rm", "timeout"):
            tool = shutil.which(name)
            self.assertIsNotNone(tool, f"required tool {name!r} not found")
            (self.bin / name).symlink_to(tool)

        self.calls = self.root / "apt-calls.jsonl"
        self.sleeps = self.root / "sleeps.txt"
        self.write_shim(
            "sleep",
            f"#!/bin/sh\nprintf '%s\\n' \"$1\" >> '{self.sleeps}'\n",
        )
        self.write_shim("apt-get", f"""#!{sys.executable}
import json
import os
import pathlib
import sys
import time

args = sys.argv[1:]
with open(os.environ["APT_CALLS"], "a") as log:
    log.write(json.dumps(args) + "\\n")
state = pathlib.Path(os.environ["APT_STATE"])
count = int(state.read_text().strip()) + 1 if state.exists() else 1
state.write_text(str(count))
mode = os.environ.get("APT_MODE", "ok")
install = "install" in args
if mode == "hang_update_once" and count == 1:
    time.sleep(2)
if mode == "hang_install_once" and count == 2:
    time.sleep(2)
if install and (mode == "fail_snapshot_always" or (mode == "fail_snapshot_once" and count == 2)):
    print("E: Failed to fetch https://snapshot.debian.org/archive/debian/pool/python3.deb  504  Gateway Timeout", file=sys.stderr)
    sys.exit(100)
if install and mode == "fail_package":
    print("E: Unable to locate package python3", file=sys.stderr)
    sys.exit(100)
""")
        self.env = {
            **os.environ,
            "PATH": str(self.bin),
            "APT_CALLS": str(self.calls),
            "APT_STATE": str(self.root / "apt-count"),
            "CI_JOB_ID": "2",
            "CI_PROJECT_DIR": str(self.root),
        }

    def write_shim(self, name, source):
        path = self.bin / name
        path.write_text(source, encoding="utf-8")
        path.chmod(0o755)

    def run_apt(self, mode="ok"):
        self.env["APT_MODE"] = mode
        return subprocess.run(
            ["sh", str(SCRIPT), "apt", "python3"],
            cwd=self.root,
            env=self.env,
            capture_output=True,
            text=True,
            timeout=30,
        )

    def apt_calls(self):
        return [json.loads(line) for line in self.calls.read_text().splitlines()]

    def recorded_sleeps(self):
        if not self.sleeps.exists():
            return []
        return [int(line) for line in self.sleeps.read_text().split()]

    def assert_attempt_files_removed(self):
        self.assertFalse((self.root / ".ci-apt-attempt.log").exists())
        self.assertFalse((self.root / ".ci-apt-attempt.rc").exists())

    def test_success_disables_internal_retries_without_sleeping(self):
        result = self.run_apt()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.apt_calls()
        self.assertEqual(len(calls), 2)
        self.assertIn("update", calls[0])
        self.assertIn("Acquire::Check-Valid-Until=false", calls[0])
        self.assertIn("install", calls[1])
        self.assertEqual(calls[1][-3:], ["--no-install-recommends", "apt", "python3"])
        for call in calls:
            self.assertIn("Acquire::Retries=0", call)
            self.assertIn("Acquire::https::Timeout=60", call)
        self.assertEqual(self.recorded_sleeps(), [])
        self.assert_attempt_files_removed()

    def test_snapshot_fetch_failure_is_retried(self):
        result = self.run_apt("fail_snapshot_once")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(self.apt_calls()), 4)
        self.assertEqual(self.recorded_sleeps(), [122])
        self.assertIn("transient snapshot.debian.org failure (attempt 1/3)", result.stderr)
        self.assert_attempt_files_removed()

    def test_hung_update_times_out_and_is_retried(self):
        self.env["CI_APT_UPDATE_TIMEOUT"] = "1"
        result = self.run_apt("hang_update_once")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(self.apt_calls()), 3)
        self.assertEqual(self.recorded_sleeps(), [122])
        self.assertIn("transient snapshot.debian.org failure (attempt 1/3)", result.stderr)
        self.assert_attempt_files_removed()

    def test_hung_install_times_out_and_is_retried(self):
        self.env["CI_APT_INSTALL_TIMEOUT"] = "1"
        result = self.run_apt("hang_install_once")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(self.apt_calls()), 4)
        self.assertEqual(self.recorded_sleeps(), [122])
        self.assertIn("transient snapshot.debian.org failure (attempt 1/3)", result.stderr)
        self.assert_attempt_files_removed()

    def test_timeout_ladder_fits_inside_lint_runner_hour(self):
        # Three attempts at 240 s update + 600 s install, plus both maximum
        # backoffs and jitter, leave 602 s for the rest of worker-kimi-ts.
        self.assertEqual(3 * (240 + 600) + 120 + 300 + 2 * 29, 2998)
        self.assertLess(3 * (240 + 600) + 120 + 300 + 2 * 29, 3600)

    def test_persistent_snapshot_failure_exhausts_three_attempts(self):
        result = self.run_apt("fail_snapshot_always")
        self.assertEqual(result.returncode, 100)
        self.assertEqual(len(self.apt_calls()), 6)
        self.assertEqual(self.recorded_sleeps(), [122, 302])
        self.assertIn("failure (attempt 1/3)", result.stderr)
        self.assertIn("failure (attempt 2/3)", result.stderr)
        self.assertIn("attempt 3/3 failed (exit 100); not retrying", result.stderr)
        self.assert_attempt_files_removed()

    def test_package_resolution_failure_is_not_retried(self):
        result = self.run_apt("fail_package")
        self.assertEqual(result.returncode, 100)
        self.assertEqual(len(self.apt_calls()), 2)
        self.assertEqual(self.recorded_sleeps(), [])
        self.assertIn("attempt 1/3 failed (exit 100); not retrying", result.stderr)
        self.assertNotIn("retrying after a transient", result.stderr)
        self.assert_attempt_files_removed()


if __name__ == "__main__":
    unittest.main()
