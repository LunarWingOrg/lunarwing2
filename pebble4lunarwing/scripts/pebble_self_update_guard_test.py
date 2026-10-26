#!/usr/bin/env python3
"""Self-test for the WI#425 pebble self-update refusal wrapper.

Drives scripts/pebble-self-update-guard.sh end to end with /bin/echo
standing in for the real pebble binary, so the refusal and pass-through
contracts are asserted without needing the pinned image.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys
import tempfile
import unittest

LANE_ROOT = pathlib.Path(__file__).resolve().parent.parent
WRAPPER = LANE_ROOT / "scripts" / "pebble-self-update-guard.sh"


def run_wrapper(*args: str) -> subprocess.CompletedProcess[str]:
    with tempfile.TemporaryDirectory() as temporary:
        wrapper = pathlib.Path(temporary) / "pebble-self-update-guard.sh"
        wrapper.write_text(
            WRAPPER.read_text(encoding="utf-8").replace(
                "REAL_PEBBLE=/usr/local/lib/pebble/pebble", "REAL_PEBBLE=/bin/echo"
            ),
            encoding="utf-8",
        )
        return subprocess.run(
            ["/bin/sh", str(wrapper), *args],
            capture_output=True,
            text=True,
            timeout=30,
            env={**os.environ, "LUNARWING_REAL_PEBBLE": "/bin/false"},
        )


class PebbleSelfUpdateGuardTests(unittest.TestCase):
    def test_wrapper_is_executable(self) -> None:
        # The Dockerfile installs the wrapper as /usr/local/bin/pebble; a
        # non-executable file would silently fall out of PATH resolution.
        self.assertTrue(os.access(WRAPPER, os.X_OK), str(WRAPPER))

    def test_self_update_refused(self) -> None:
        result = run_wrapper("self-update")
        self.assertEqual(result.returncode, 77, result.stderr)
        self.assertIn("self-update", result.stderr)
        self.assertIn("WI#425", result.stderr)

    def test_self_update_refused_after_global_flags(self) -> None:
        # The scan must cover every argv token, not only $1: upstream accepts
        # root/global flags before the subcommand (opencode-lane precedent).
        result = run_wrapper("--print-logs", "self-update")
        self.assertEqual(result.returncode, 77, result.stderr)

    def test_self_update_refused_between_task_args(self) -> None:
        # Task-supplied prompt text can legitimately contain the token; the
        # refusal is positional-naive by design (naive-use guard), but a
        # mid-argv occurrence must still refuse — prompt text is not an
        # exec vector, and over-refusal is the fail-closed direction.
        result = run_wrapper("--permission-mode", "self-update")
        self.assertEqual(result.returncode, 77, result.stderr)

    def test_other_subcommands_pass_through(self) -> None:
        # /bin/echo interprets --version itself, so that case asserts only
        # that the wrapper did not refuse; the rest check the exact argv.
        result = run_wrapper("--version")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("refused", result.stderr)
        for args in (
            ("--output-format", "json", "--model", "openai/gpt-5.2"),
            ("--permission-mode", "danger-full-access"),
            ("run", "fix the tests"),
            (),
        ):
            with self.subTest(args=args):
                result = run_wrapper(*args)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.strip(), " ".join(args))

    def test_real_pebble_override_env_is_not_honoured(self) -> None:
        script = WRAPPER.read_text(encoding="utf-8")
        self.assertIn("REAL_PEBBLE=/usr/local/lib/pebble/pebble", script)
        self.assertNotIn("REAL_PEBBLE=\"${LUNARWING_REAL_PEBBLE", script)
        result = run_wrapper("run", "fixed path")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "run fixed path")


if __name__ == "__main__":
    unittest.main()
