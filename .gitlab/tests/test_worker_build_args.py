#!/usr/bin/env python3
"""Exercise the CI wrapper digest contract without building/publishing images."""

import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".gitlab/scripts/build-worker-image.sh"


class WorkerBuildArgsTests(unittest.TestCase):
    def setUp(self):
        # PATH shims need an exec-capable filesystem (some hosts mount /tmp noexec).
        temporary = tempfile.TemporaryDirectory(dir=ROOT)
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        # Match the slim ci-config tool surface; neither git nor awk is
        # installed there. Keep the PATH closed so this cannot contact a real
        # builder or registry even if the production script changes.
        for name in ("sh", "sha256sum", "cut", "tr", "base64", "mkdir", "grep", "tee", "rm"):
            tool = shutil.which(name)
            self.assertIsNotNone(tool, f"required tool {name!r} not found")
            (self.bin / name).symlink_to(tool)
        # The retry backoff must not slow the hermetic suite. Record each
        # requested delay so the backoff curve and its ceiling stay pinned.
        self.sleeps = self.root / "sleeps.txt"
        self.write_shim(
            "sleep",
            f"#!/bin/sh\nprintf '%s\\n' \"$1\" >> '{self.sleeps}'\nexit 0\n",
        )
        self.log = self.root / "build-calls.jsonl"
        self.script = self.root / SCRIPT.name
        shutil.copy(SCRIPT, self.script)
        shutil.copytree(
            ROOT / "opencode2.4lunarwing", self.root / "opencode2.4lunarwing",
            ignore=shutil.ignore_patterns("node_modules", "__pycache__"),
        )
        self.wrapper = self.root / "opencode2.4lunarwing/scripts/opencode-exposure-guard.sh"
        self.write_shim("git", "#!/bin/sh\nprintf '%s\\n' 1700000000\n")
        # Only metadata_digest() uses awk; emulate that extraction in Python.
        self.write_shim("awk", f"""#!{sys.executable}
import json
import sys
with open(sys.argv[-1]) as source:
    print(json.load(source)["containerimage.digest"])
""")
        # BUILDCTL_MODE steers failure injection: registry-auth, snapshot and
        # genuine build/package failures all stay inside this hermetic shim.
        self.write_shim("buildctl-daemonless.sh", f"""#!{sys.executable}
import json
import os
import pathlib
import sys
args = sys.argv[1:]
with open(os.environ["BUILD_CALLS"], "a") as log:
    log.write(json.dumps(args) + "\\n")
state = pathlib.Path(os.environ["BUILDCTL_STATE"])
count = int(state.read_text().strip()) + 1 if state.exists() else 1
state.write_text(str(count))
mode = os.environ.get("BUILDCTL_MODE", "ok")
if mode == "fail_always" or (mode == "fail_once" and count == 1):
    print("#33 [auth] registry.invalid/example/worker/worker-opencode2:pull,push token for registry.invalid", file=sys.stderr)
    print("ERROR: failed to push registry.invalid/example/worker/worker-opencode2:tag: failed to authorize: failed to fetch oauth token: unexpected status from POST request to https://git.lunarwing.invalid/jwt/auth: 429 Too Many Requests: Retry later", file=sys.stderr)
    sys.exit(1)
if mode in ("snapshot_fail_always", "snapshot_fail_once") and (mode == "snapshot_fail_always" or count == 1):
    print(os.environ.get("BUILDCTL_SNAPSHOT_SIGNATURE", "W: Tried to start delayed item http://snapshot.debian.org/archive/debian/20261024T011300Z but failed"), file=sys.stderr)
    if os.environ.get("BUILDCTL_SNAPSHOT_KILL") == "1":
        print("#34 [runtime 2/17] RUN apt-get update against snapshot.debian.org", file=sys.stderr)
        print("ERROR: failed to solve: process \\"/bin/sh -c apt-get update\\" did not complete successfully: exit code: 137", file=sys.stderr)
    else:
        print("ERROR: failed to solve: process \\"/bin/sh -c apt-get update\\" did not complete successfully: exit code: 1", file=sys.stderr)
    sys.exit(1)
if mode in ("fail_package", "fail_version"):
    print("W: Tried to start delayed item http://snapshot.debian.org/archive/debian/20261024T011300Z but failed", file=sys.stderr)
    if mode == "fail_package":
        print("E: Unable to locate package lunarwing-fixture", file=sys.stderr)
    else:
        print("E: Version '1.2.3' for 'lunarwing-fixture' was not found", file=sys.stderr)
    sys.exit(100)
if mode == "fail_compile":
    print("ERROR: failed to solve: process \\"/bin/sh -c make\\" did not complete successfully: exit code: 2", file=sys.stderr)
    sys.exit(1)
metadata = pathlib.Path(args[args.index("--metadata-file") + 1])
metadata.write_text(json.dumps({{"containerimage.digest": "sha256:" + "0" * 64}}))
for index, argument in enumerate(args):
    if argument == "--output" and args[index + 1].startswith("type=tar,dest="):
        destination = pathlib.Path(args[index + 1].split("dest=", 1)[1])
        destination.write_bytes(b"fixture rootfs tar")
""")
        self.env = {
            **os.environ,
            "PATH": str(self.bin),
            "HOME": str(self.root / "home"),
            "BUILD_CALLS": str(self.log),
            "BUILDCTL_STATE": str(self.root / "buildctl-count"),
            "CI_COMMIT_SHA": "1" * 40,
            "CI_JOB_ID": "2",
            "CI_PIPELINE_ID": "3",
            "CI_PROJECT_DIR": str(self.root),
            "CI_REGISTRY": "registry.invalid",
            "CI_REGISTRY_IMAGE": "registry.invalid/example/worker",
            "CI_REGISTRY_USER": "fixture",
            "CI_REGISTRY_PASSWORD": "fixture",
        }

    def write_shim(self, name, source):
        path = self.bin / name
        path.unlink(missing_ok=True)
        path.write_text(source, encoding="utf-8")
        path.chmod(0o755)

    def run_build(self):
        self.log.unlink(missing_ok=True)
        self.sleeps.unlink(missing_ok=True)
        return subprocess.run(
            ["sh", str(self.script), "opencode2"], cwd=self.root, env=self.env,
            capture_output=True, text=True, timeout=30,
        )

    def recorded_sleeps(self) -> list[int]:
        if not self.sleeps.exists():
            return []
        return [int(line) for line in self.sleeps.read_text().split()]

    def test_checkout_digest_reaches_candidate_and_clean_rebuild(self):
        # Changing the checkout catches hardcoded/stale digests; inspect the
        # actual buildctl argv, so dropping the build-arg forwarding is caught.
        for revision in ("first", "second"):
            with self.subTest(revision=revision):
                self.wrapper.write_text(f"#!/bin/sh\n# {revision} wrapper\n", encoding="utf-8")
                digest = hashlib.sha256(self.wrapper.read_bytes()).hexdigest()
                result = self.run_build()
                self.assertEqual(result.returncode, 0, result.stderr)
                calls = [json.loads(line) for line in self.log.read_text().splitlines()]
                self.assertEqual(len(calls), 2)
                self.assertNotIn("--no-cache", calls[0])
                self.assertIn("--no-cache", calls[1])
                for args in calls:
                    options = [args[i + 1] for i, arg in enumerate(args) if arg == "--opt"]
                    anchors = [arg for arg in options if arg.startswith("build-arg:LUNARWING_WRAPPER_SHA256=")]
                    self.assertEqual(anchors, [f"build-arg:LUNARWING_WRAPPER_SHA256={digest}"])

    def test_missing_wrapper_fails_before_build(self):
        self.wrapper.unlink()
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot hash", result.stderr)
        self.assertFalse(self.log.exists())

    def test_failed_hash_command_fails_even_with_valid_looking_output(self):
        self.write_shim("sha256sum", "#!/bin/sh\nprintf '%064d  -\\n' 0\nexit 1\n")
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot hash", result.stderr)
        self.assertFalse(self.log.exists())

    def test_malformed_wrapper_digest_fails_before_build(self):
        for digest in ("", "a" * 63, "a" * 65, "g" * 64):
            with self.subTest(digest=digest):
                self.write_shim("sha256sum", f"#!/bin/sh\nprintf '%s  -\\n' '{digest}'\n")
                result = self.run_build()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("invalid wrapper sha256", result.stderr)
                self.assertFalse(self.log.exists())

    def test_transient_registry_auth_push_failure_is_retried(self):
        # Pipeline 5984: a jwt/auth 429 at the push step must not fail the job.
        self.env["BUILDCTL_MODE"] = "fail_once"
        result = self.run_build()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        # Candidate attempt (failed push) + candidate retry + clean rebuild.
        self.assertEqual(len(calls), 3)
        self.assertNotIn("--no-cache", calls[0])
        self.assertNotIn("--no-cache", calls[1])
        self.assertIn("--no-cache", calls[2])
        self.assertIn("transient registry-auth error", result.stderr)
        # The retry message must report the current max-attempts count.
        self.assertIn("/8", result.stderr)

    def test_persistent_transient_failure_exhausts_retries(self):
        self.env["BUILDCTL_MODE"] = "fail_always"
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        # Initial attempt plus seven retries; the clean rebuild never runs.
        self.assertEqual(len(calls), 8)
        for args in calls:
            self.assertNotIn("--no-cache", args)
        self.assertIn("not retrying", result.stderr)

    def test_push_backoff_doubles_from_45s_across_eight_attempts(self):
        # Pipeline 6252's sustained 19-minute burst outlasted the previous
        # 6-attempt/~15-minute window. The curve must keep its 45 s base and
        # its doubling across exactly 8 attempts: the seven retry rungs
        # 45+90+180+360+720+1440+2880 sum to 5715 s (~95.25 min) of
        # sleeping, inside the candidate job's 8 h timeout. A wider curve
        # would grow past that budget, so pin the rungs. The jitter is
        # CI_JOB_ID % 30 (run-205 NQ-2) and this suite pins CI_JOB_ID=2,
        # so the rungs assert exactly (run-205 NQ-1: the old PID-jitter
        # envelope bound failed about one run in five — the bases alone
        # sum to 5715 s, not 95*60, so any PID with $$ % 15 >= 12 broke
        # the total bound).
        self.env["BUILDCTL_MODE"] = "fail_always"
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        delays = self.recorded_sleeps()
        expected_bases = [45, 90, 180, 360, 720, 1440, 2880]
        jitter = int(self.env["CI_JOB_ID"]) % 30
        self.assertEqual(
            delays,
            [base + jitter for base in expected_bases],
            f"jitter {jitter} from CI_JOB_ID={self.env['CI_JOB_ID']}",
        )
        self.assertLessEqual(sum(expected_bases) + 7 * 29, 8 * 3600)

    def test_transient_snapshot_signature_is_retried(self):
        self.env["BUILDCTL_MODE"] = "snapshot_fail_once"
        self.env["BUILDCTL_SNAPSHOT_SIGNATURE"] = (
            "Failed to fetch http://snapshot.debian.org/archive/debian/20261024T011300Z "
            "Connection failed"
        )
        result = self.run_build()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        # Candidate failure + candidate retry + clean rebuild.
        self.assertEqual(len(calls), 3)
        self.assertNotIn("--no-cache", calls[0])
        self.assertNotIn("--no-cache", calls[1])
        self.assertIn("--no-cache", calls[2])
        self.assertIn(
            "retrying after a transient snapshot.debian.org failure (attempt 1/3)",
            result.stderr,
        )
        self.assertEqual(self.recorded_sleeps(), [122])

    def test_snapshot_run_137_is_retried(self):
        self.env["BUILDCTL_MODE"] = "snapshot_fail_once"
        self.env["BUILDCTL_SNAPSHOT_SIGNATURE"] = "W: mirror progress stalled"
        self.env["BUILDCTL_SNAPSHOT_KILL"] = "1"
        result = self.run_build()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.log.read_text().splitlines()
        self.assertEqual(len(calls), 3)
        self.assertIn("transient snapshot.debian.org failure", result.stderr)
        self.assertEqual(self.recorded_sleeps(), [122])

    def test_persistent_snapshot_failure_uses_smaller_budget(self):
        self.env["BUILDCTL_MODE"] = "snapshot_fail_always"
        self.env["BUILDCTL_SNAPSHOT_SIGNATURE"] = "Some index files failed to download"
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(len(calls), 3)
        for args in calls:
            self.assertNotIn("--no-cache", args)
        self.assertEqual(self.recorded_sleeps(), [122, 302])
        self.assertIn("image build attempt 3/3", result.stderr)
        self.assertIn("not retrying", result.stderr)
        # Both ladders, including maximum jitter on all nine sleeps, fit easily.
        self.assertLessEqual(5715 + 420 + 9 * 29, 8 * 3600)

    def test_package_failures_are_not_retried_despite_snapshot_warning(self):
        for mode in ("fail_package", "fail_version"):
            with self.subTest(mode=mode):
                self.env["BUILDCTL_MODE"] = mode
                result = self.run_build()
                self.assertEqual(result.returncode, 100)
                self.assertEqual(len(self.log.read_text().splitlines()), 1)
                self.assertEqual(self.recorded_sleeps(), [])
                self.assertIn("not retrying", result.stderr)

    def test_non_transient_build_failure_is_not_retried(self):
        self.env["BUILDCTL_MODE"] = "fail_compile"
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        calls = self.log.read_text().splitlines()
        self.assertEqual(len(calls), 1)
        self.assertIn("not retrying", result.stderr)


if __name__ == "__main__":
    unittest.main()
