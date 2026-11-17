#!/usr/bin/env python3
"""Contract tests for the fail-closed managed-policy patch installer."""

from __future__ import annotations

import hashlib
import pathlib
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock

import apply_managed_policy_precedence as subject


class ApplyManagedPolicyPrecedenceTests(unittest.TestCase):
    def test_real_upstream_patch_applies_to_the_pin(self) -> None:
        pristine = pathlib.Path("/tmp/wi745-pristine")
        if not (pristine / "packages/core/src/config/plugin/policy.ts").is_file():
            self.skipTest("pristine v2.2.7 tree is unavailable")
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary) / "opencode"
            shutil.copytree(pristine, root, symlinks=True)
            for command in (
                ["git", "init", "--quiet"],
                ["git", "add", "-A"],
                ["git", "-c", "user.name=test", "-c", "user.email=test@example.invalid", "commit", "-qm", "pin"],
            ):
                result = subprocess.run(command, cwd=root, capture_output=True, text=True, timeout=120)
                self.assertEqual(result.returncode, 0, result.stderr)
            subject.apply(root, run_tests=False)
            self.assertIn("policy.managed", (root / "packages/core/src/config/plugin/policy.ts").read_text())
            result = subprocess.run(
                ["git", "apply", "--check", "--reverse", str(subject.PATCH)],
                cwd=root, capture_output=True, text=True, check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_drifted_patch_input_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            for name in subject.SOURCES:
                (root / name).parent.mkdir(parents=True, exist_ok=True)
                (root / name).write_text("drifted\n")
            with mock.patch.object(subject, "apply") as apply:
                with self.assertRaises(SystemExit) as error:
                    subject.main(root)
        self.assertIn("re-read the upstream policy precedence", str(error.exception))
        apply.assert_not_called()

    def test_patch_drift_is_refused(self) -> None:
        source = pathlib.Path("/tmp/wi745-fresh")
        if not (source / ".git").is_dir():
            self.skipTest("pinned upstream clone is unavailable")
        with self.assertRaises(SystemExit) as error:
            with mock.patch.object(subject, "PATCH_DIGEST", "0" * 64):
                subject.main(source)
        self.assertIn("patch content changed", str(error.exception))

    @mock.patch.object(subject, "_run")
    @mock.patch.object(subject.subprocess, "run")
    def test_unexpected_changed_path_is_refused(self, run: mock.Mock, execute: mock.Mock) -> None:
        run.return_value.returncode = 0
        run.return_value.stdout = " M packages/core/src/managed-policy.ts\n M packages/cli/src/other.ts\n"
        run.return_value.stderr = ""
        with self.assertRaises(SystemExit) as error:
            subject.apply(pathlib.Path(__file__).parent)
        self.assertIn("unexpected source set", str(error.exception))
        self.assertEqual(execute.call_count, 2)

    @mock.patch.object(subject.subprocess, "run")
    def test_failed_command_is_fatal(self, run: mock.Mock) -> None:
        run.side_effect = subprocess.CalledProcessError(1, "patch", output="hunk failed")
        with self.assertRaises(SystemExit) as error:
            subject.apply(pathlib.Path(__file__).parent)
        self.assertIn("hunk failed", str(error.exception))


if __name__ == "__main__":
    unittest.main()
