#!/usr/bin/env python3
"""WI#862: the shared in-Dockerfile retry for a pinned-input download.

The lane Dockerfiles are built both by the CI candidate job and by mt-admin's
build-tenant/upgrade-tenant path, so the helper in each lane's scripts/
directory must behave the same in both. These tests run it against a stub
command, so they pin the classification and the bounded ladder without a
builder, a registry or a network.
"""

import hashlib
import pathlib
import shutil
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
LANES = (
    "pebble4lunarwing",
    "opencode4lunarwing",
    "opencode2.4lunarwing",
    "kimicode4lunarwing",
    "lunardepth4lunarwing",
)
HELPER = pathlib.Path("scripts/worker-download-retry.sh")


class WorkerDownloadRetryTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(dir=ROOT)
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for name in ("sh", "grep", "cat", "tee", "rm"):
            tool = shutil.which(name)
            self.assertIsNotNone(tool, f"required tool {name!r} not found")
            (self.bin / name).symlink_to(tool)
        self.sleeps = self.root / "sleeps.txt"
        self.write_shim(
            "sleep",
            f"#!/bin/sh\nprintf '%s\\n' \"$1\" >> '{self.sleeps}'\nexit 0\n",
        )
        self.helper = self.root / "worker-download-retry.sh"
        shutil.copy(ROOT / "lunardepth4lunarwing" / HELPER, self.helper)

    def write_shim(self, name, source):
        path = self.bin / name
        path.write_text(source, encoding="utf-8")
        path.chmod(0o755)

    def recorded_sleeps(self):
        if not self.sleeps.exists():
            return []
        return [int(line) for line in self.sleeps.read_text().split()]

    def run_retry(self, command, cleanup="-"):
        script = self.root / "run.sh"
        script.write_text(
            "set -eu\n"
            f". '{self.helper}'\n"
            f"wdr_retry 'fixture download' '{cleanup}' '{command}'\n",
            encoding="utf-8",
        )
        return subprocess.run(
            ["sh", str(script)],
            cwd=self.root,
            env={"PATH": str(self.bin), "HOME": str(self.root)},
            capture_output=True,
            text=True,
            timeout=30,
        )

    def test_copies_are_byte_identical(self):
        # One shared helper: a lane that drifts from the others would retry
        # under different rules in CI and in an mt-admin tenant build.
        digests = {
            lane: hashlib.sha256((ROOT / lane / HELPER).read_bytes()).hexdigest()
            for lane in LANES
        }
        self.assertEqual(len(set(digests.values())), 1, digests)

    def test_every_lane_sources_the_helper_before_its_downloads(self):
        for lane in LANES:
            with self.subTest(lane=lane):
                dockerfile = (ROOT / lane / "Dockerfile").read_text(encoding="utf-8")
                # pebble's build context is the repository root, so its COPY
                # names the lane prefix; the other four build from the lane
                # directory and COPY the bare path.
                copy = (
                    "COPY pebble4lunarwing/scripts/worker-download-retry.sh"
                    if lane == "pebble4lunarwing"
                    else "COPY scripts/worker-download-retry.sh"
                )
                self.assertIn(copy, dockerfile)
                self.assertIn("wdr_retry", dockerfile)
                # The COPY must precede the first use, or the source fails.
                self.assertLess(
                    dockerfile.index(copy),
                    dockerfile.index("wdr_retry"),
                    f"{lane}: the helper COPY must precede its first use",
                )

    def test_job_204104_drop_is_retried(self):
        self.write_shim(
            "git",
            "#!/bin/sh\n"
            "printf '%s\\n' \"Cloning into '/src'...\" >&2\n"
            "printf '%s\\n' 'error: RPC failed; curl 55 Failed sending data to the peer' >&2\n"
            "printf '%s\\n' \"fatal: could not fetch e878c9db from promisor remote\" >&2\n"
            "exit 128\n",
        )
        result = self.run_retry("git clone /src")
        # Three attempts from a 20 s base; the first two retry, the third
        # attempt's failure is final.
        self.assertEqual(result.returncode, 128, result.stderr)
        self.assertIn("attempt 1/3", result.stderr)
        self.assertIn("attempt 2/3", result.stderr)
        self.assertIn("not retrying", result.stderr)
        self.assertEqual(len(self.recorded_sleeps()), 2)
        first, second = self.recorded_sleeps()
        # Back-off doubles from 20 s; the jitter is the shell's PID mod 10.
        self.assertTrue(20 <= first <= 29, first)
        self.assertTrue(40 <= second <= 49, second)

    def test_retry_starts_from_a_cleaned_state(self):
        marker = self.root / "partial"
        marker.write_text("half-fetched", encoding="utf-8")
        self.write_shim(
            "git",
            f"#!/bin/sh\nif [ -e '{marker}' ]; then\n"
            "  printf '%s\\n' 'error: RPC failed; curl 56 OpenSSL SSL_read: Connection reset by peer' >&2\n"
            "  exit 1\nfi\n"
            "printf '%s\\n' 'cloned'\n",
        )
        result = self.run_retry("git clone /src", f"rm -f '{marker}'")
        self.assertEqual(result.returncode, 0, result.stderr)
        # The cleanup ran, so the second attempt saw a clean tree and passed.
        self.assertIn("attempt 1/3", result.stderr)
        self.assertEqual(len(self.recorded_sleeps()), 1)

    def test_integrity_and_pin_failures_are_not_retried(self):
        cases = {
            "EINTEGRITY": "npm error code EINTEGRITY",
            "lockfile refusal": "ERR_PNPM_OUTDATED_LOCKFILE",
            "sha256sum -c": "sha256sum: WARNING: 1 computed checksum did NOT match",
            "pin mismatch": "fatal: could not fetch deadbeef from promisor remote\n"
                            "error: pathspec 'deadbeef' did not match any file(s) known to git",
            "403": "The requested URL returned error: 403",
            "401": "The requested URL returned error: 401",
            "404": "The requested URL returned error: 404",
            "full disk": "npm error nospc ENOSPC: no space left on device",
            "test failure": "not ok 7 - bridge rejects a malformed envelope\n# failed 1 test",
        }
        for label, line in cases.items():
            with self.subTest(case=label):
                self.sleeps.unlink(missing_ok=True)
                self.write_shim(
                    "failing", f"#!/bin/sh\nprintf '%s\\n' '{line}' >&2\nexit 1\n"
                )
                result = self.run_retry("failing")
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn("not retrying", result.stderr)
                self.assertEqual(self.recorded_sleeps(), [], label)

    def test_a_connection_error_beside_a_fatal_cause_is_not_retried(self):
        # The same step can print a drop and then the pin verdict that
        # actually stopped it; the fatal cause must win (WI#559/VK-1).
        self.write_shim(
            "failing",
            "#!/bin/sh\n"
            "printf '%s\\n' 'error: RPC failed; curl 55 Failed sending data to the peer' >&2\n"
            "printf '%s\\n' 'npm error code EINTEGRITY' >&2\n"
            "exit 1\n",
        )
        result = self.run_retry("failing")
        self.assertEqual(result.returncode, 1)
        self.assertIn("not retrying", result.stderr)
        self.assertEqual(self.recorded_sleeps(), [])

    def test_a_failed_download_stops_a_set_eu_caller(self):
        # The helper runs inside a RUN under `set -eu`; its own failure must
        # stop the step. A pipeline member's status is tee's, so this pins
        # that the wrapped command's status is what the caller sees — both
        # for a command that returns non-zero and one that calls exit.
        for body, expected in (("printf 'boom\\n' >&2; exit 7", 7), ("exit 9", 9)):
            with self.subTest(body=body):
                self.sleeps.unlink(missing_ok=True)
                self.write_shim("failing", f"#!/bin/sh\n{body}\n")
                result = self.run_retry("failing")
                self.assertEqual(result.returncode, expected, result.stderr)
                self.assertIn("not retrying", result.stderr)

    def test_a_successful_download_does_not_sleep(self):
        self.write_shim("ok", "#!/bin/sh\nprintf '%s\\n' 'cloned'\n")
        result = self.run_retry("ok")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.recorded_sleeps(), [])

    def test_an_earlier_attempt_does_not_classify_a_later_one(self):
        # Attempt 1 prints a connection error; attempt 2 fails on a pin
        # mismatch. The per-attempt log must keep attempt 1's text from
        # turning attempt 2 into a retry.
        state = self.root / "count"
        self.write_shim(
            "flaky",
            "#!/bin/sh\n"
            f"count=$(( $(cat '{state}' 2>/dev/null || echo 0) + 1 ))\n"
            f"printf '%s' \"$count\" > '{state}'\n"
            "if [ \"$count\" -eq 1 ]; then\n"
            "  printf '%s\\n' 'error: RPC failed; curl 55 Failed sending data to the peer' >&2\n"
            "  exit 1\n"
            "fi\n"
            "printf '%s\\n' 'error: pathspec deadbeef did not match any file(s) known to git' >&2\n"
            "exit 1\n",
        )
        result = self.run_retry("flaky")
        self.assertEqual(result.returncode, 1, result.stderr)
        # Exactly one retry: the second attempt's pin failure ends it.
        self.assertEqual(len(self.recorded_sleeps()), 1)
        self.assertIn("attempt 2/3", result.stderr)
        self.assertIn("not retrying", result.stderr)


if __name__ == "__main__":
    unittest.main()
