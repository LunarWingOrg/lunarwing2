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
# BuildKit lines from job 204104 (pipeline 8578, trace lines 1092–1096),
# with GitLab's timestamps/stream prefixes removed as in the wrapper's log.
JOB_204104_TRACE = pathlib.Path(__file__).with_name("fixtures") / "job-204104-buildkit.log"


class WorkerBuildArgsTests(unittest.TestCase):
    def setUp(self):
        # PATH shims need an exec-capable filesystem (some hosts mount /tmp noexec).
        temporary = tempfile.TemporaryDirectory(dir=ROOT)
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        # Keep the PATH closed so this cannot contact a real builder or
        # registry even if the production script changes.
        for name in ("sh", "sha256sum", "cut", "tr", "base64", "mkdir", "awk", "grep", "tee", "rm"):
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
        # BUILDCTL_MODE steers failure injection without contacting a builder
        # or registry. Each failure mode emits the job-trace signature that the
        # production retry classifier must accept or reject.
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
if mode == "fail_push_trace_once" and count == 1:
    print(os.environ["PUSH_TRACE_LINE"], file=sys.stderr)
    sys.exit(1)
if mode == "fail_snapshot_always" or (mode == "fail_snapshot_once" and count == 1):
    print("#12 [builder 3/13] RUN apt-get update && apt-get install", file=sys.stderr)
    print("E: Failed to fetch https://snapshot.debian.org/archive/debian/20261024T011300Z/pool/example.deb  504  Gateway Timeout", file=sys.stderr)
    sys.exit(1)
if mode == "fail_snapshot_killed_once" and count == 1:
    print("#12 [runtime 2/17] RUN apt-get update", file=sys.stderr)
    print("Killed", file=sys.stderr)
    # Real-trace shape from pipeline 7174, job 156769: BuildKit prints the
    # whole RUN command and escapes its inner double quotes as \".
    print('error: failed to solve: process "/bin/sh -c set -eu;     rm -f /etc/apt/sources.list.d/debian.sources;     printf \\'%s\\\\n\\'       \\\\"deb http://snapshot.debian.org/archive/debian/${{DEBIAN_SNAPSHOT}}/ trixie main\\\\"       > /etc/apt/sources.list;     timeout 240 apt-get update" did not complete successfully: exit code: 137', file=sys.stderr)
    sys.exit(1)
if mode == "fail_snapshot_timeout_once" and count == 1:
    print('ERROR: failed to solve: process "/bin/sh -c timeout 600 apt-get install from snapshot.debian.org" did not complete successfully: exit code: 124', file=sys.stderr)
    sys.exit(1)
if mode == "fail_compile_killed_after_snapshot_header":
    print('#7 [runtime 2/18] RUN set -eu; printf "deb http://snapshot.debian.org/archive/debian/..."', file=sys.stderr)
    print('ERROR: failed to solve: process "/bin/sh -c bun build --compile ..." did not complete successfully: exit code: 137', file=sys.stderr)
    sys.exit(1)
if mode == "fail_compile_after_snapshot_delay_warning":
    print("#12 [builder 3/13] RUN apt-get update", file=sys.stderr)
    print("W: Tried to start delayed item http://snapshot.debian.org/archive/debian/20261024T011300Z but failed", file=sys.stderr)
    print('ERROR: failed to solve: process "/bin/sh -c make" did not complete successfully: exit code: 2', file=sys.stderr)
    sys.exit(1)
if mode == "fail_download" and os.environ["DOWNLOAD_FAIL_CALL"] in ("all", str(count)):
    sys.stderr.write(os.environ["DOWNLOAD_TRACE"])
    sys.exit(1)
if mode == "fail_compile":
    print("ERROR: failed to solve: process \\"/bin/sh -c make\\" did not complete successfully: exit code: 2", file=sys.stderr)
    sys.exit(1)
if mode == "fail_package":
    print("E: Unable to locate package definitely-not-a-real-package", file=sys.stderr)
    sys.exit(100)
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

    def run_with_download_trace(self, trace, fail_call="all"):
        pathlib.Path(self.env["BUILDCTL_STATE"]).unlink(missing_ok=True)
        self.env.update(BUILDCTL_MODE="fail_download", DOWNLOAD_FAIL_CALL=fail_call, DOWNLOAD_TRACE=trace)
        return self.run_build()

    @staticmethod
    def run_error(step, command, code=1):
        return f'#{step} ERROR: process "{command}" did not complete successfully: exit code: {code}\n'

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
                    self.assertIn("--progress=plain", args)
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
        self.assertIn("transient registry push error", result.stderr)
        # The retry message must report the current max-attempts count.
        self.assertIn("/8", result.stderr)

    def test_registry_push_transient_statuses_are_retried(self):
        reasons = {
            "499": "status code 499: Client Closed Request",
            "502": "Bad Gateway",
            "503": "Service Unavailable",
            "504": "Gateway Timeout",
        }
        for method in ("PUT", "PATCH"):
            for status, reason in reasons.items():
                with self.subTest(method=method, status=status):
                    (self.root / "buildctl-count").unlink(missing_ok=True)
                    self.env["BUILDCTL_MODE"] = "fail_push_trace_once"
                    self.env["PUSH_TRACE_LINE"] = (
                        "ERROR: failed to push registry.invalid/example/worker/worker-opencode2:tag: "
                        f"unexpected status from {method} request to "
                        "https://registry.invalid/v2/example/worker/worker-opencode2/"
                        f"blobs/uploads/fixture: {status} {reason}"
                    )
                    result = self.run_build()
                    self.assertEqual(result.returncode, 0, result.stderr)
                    calls = self.log.read_text().splitlines()
                    self.assertEqual(len(calls), 3)
                    self.assertIn("transient registry push error", result.stderr)
                    self.assertEqual(self.recorded_sleeps(), [47])

    def test_job_178891_registry_499_trace_is_retried(self):
        self.env["CI_REGISTRY"] = "registry.git.lunarwing.org"
        self.env["BUILDCTL_MODE"] = "fail_push_trace_once"
        self.env["PUSH_TRACE_LINE"] = (
            "2026-11-14T21:37:43.409091Z 01O #52 ERROR: failed to push "
            "registry.git.lunarwing.org/lunarwing/lunarwing2/worker-opencode2:"
            "fcb07a8e8abd3d101db013754236818b0aa99cd1-7855-178891: "
            "unexpected status from PUT request to "
            "https://registry.git.lunarwing.org/v2/lunarwing/lunarwing2/"
            "worker-opencode2/blobs/uploads/fixture: "
            "499 status code 499: Client Closed Request"
        )
        result = self.run_build()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.log.read_text().splitlines()
        self.assertEqual(len(calls), 3)
        self.assertIn("transient registry push error", result.stderr)
        self.assertEqual(self.recorded_sleeps(), [47])

    def test_registry_push_refusal_statuses_are_not_retried(self):
        reasons = {
            "401": "Unauthorized",
            "403": "Forbidden",
            "404": "Not Found",
        }
        for method in ("PUT", "PATCH"):
            for status, reason in reasons.items():
                with self.subTest(method=method, status=status):
                    (self.root / "buildctl-count").unlink(missing_ok=True)
                    self.env["BUILDCTL_MODE"] = "fail_push_trace_once"
                    self.env["PUSH_TRACE_LINE"] = (
                        "ERROR: failed to push registry.invalid/example/worker/worker-opencode2:tag: "
                        f"unexpected status from {method} request to "
                        "https://registry.invalid/v2/example/worker/worker-opencode2/"
                        f"blobs/uploads/fixture: {status} {reason}"
                    )
                    result = self.run_build()
                    self.assertNotEqual(result.returncode, 0)
                    calls = self.log.read_text().splitlines()
                    self.assertEqual(len(calls), 1)
                    self.assertIn("not retrying", result.stderr)
                    self.assertNotIn("transient registry push error", result.stderr)
                    self.assertEqual(self.recorded_sleeps(), [])

    def test_registry_push_transient_status_on_other_host_is_not_retried(self):
        self.env["BUILDCTL_MODE"] = "fail_push_trace_once"
        self.env["PUSH_TRACE_LINE"] = (
            "ERROR: failed to push registry.invalid/example/worker/worker-opencode2:tag: "
            "unexpected status from PUT request to "
            "https://registry.other.invalid/v2/example/worker/worker-opencode2/"
            "blobs/uploads/fixture: 502 Bad Gateway"
        )
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(self.log.read_text().splitlines()), 1)
        self.assertIn("not retrying", result.stderr)
        self.assertNotIn("transient registry push error", result.stderr)
        self.assertEqual(self.recorded_sleeps(), [])

    def test_registry_push_status_prefix_is_not_retried(self):
        self.env["BUILDCTL_MODE"] = "fail_push_trace_once"
        self.env["PUSH_TRACE_LINE"] = (
            "ERROR: failed to push registry.invalid/example/worker/worker-opencode2:tag: "
            "unexpected status from PUT request to "
            "https://registry.invalid/v2/example/worker/worker-opencode2/"
            "blobs/uploads/fixture: 5020 Bad Gateway"
        )
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(self.log.read_text().splitlines()), 1)
        self.assertIn("not retrying", result.stderr)
        self.assertNotIn("transient registry push error", result.stderr)
        self.assertEqual(self.recorded_sleeps(), [])

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

    def test_non_transient_build_failure_is_not_retried(self):
        self.env["BUILDCTL_MODE"] = "fail_compile"
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        calls = self.log.read_text().splitlines()
        self.assertEqual(len(calls), 1)
        self.assertIn("not retrying", result.stderr)

    def test_transient_snapshot_failure_is_retried(self):
        self.env["BUILDCTL_MODE"] = "fail_snapshot_once"
        result = self.run_build()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(len(calls), 3)
        self.assertNotIn("--no-cache", calls[1])
        self.assertIn("--no-cache", calls[2])
        self.assertIn("transient snapshot.debian.org failure (attempt 1/3)", result.stderr)
        self.assertEqual(self.recorded_sleeps(), [122])

    def test_snapshot_sigkill_during_apt_run_is_retried(self):
        # Pipeline 7174's trace names the snapshot RUN step with "exit code:
        # 137" but buildctl itself exits 1, and BuildKit escapes the inner
        # double quotes of the printed RUN command as \". The stub prints that
        # real shape, so the classifier must match the escaped trace text,
        # not the exit status and not a bare-quote scan.
        self.env["BUILDCTL_MODE"] = "fail_snapshot_killed_once"
        result = self.run_build()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.log.read_text().splitlines()
        self.assertEqual(len(calls), 3)
        self.assertIn("transient snapshot.debian.org failure (attempt 1/3)", result.stderr)

    def test_snapshot_timeout_during_apt_run_is_retried(self):
        self.env["BUILDCTL_MODE"] = "fail_snapshot_timeout_once"
        result = self.run_build()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.log.read_text().splitlines()
        self.assertEqual(len(calls), 3)
        self.assertIn("transient snapshot.debian.org failure (attempt 1/3)", result.stderr)
        self.assertEqual(self.recorded_sleeps(), [122])

    def test_compile_sigkill_with_snapshot_step_header_is_not_retried(self):
        # VK-1: BuildKit prints every Dockerfile step header, including the
        # snapshot source setup. That unrelated text must not classify a bun
        # compile killed with 137 as an apt/snapshot failure.
        self.env["BUILDCTL_MODE"] = "fail_compile_killed_after_snapshot_header"
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        calls = self.log.read_text().splitlines()
        self.assertEqual(len(calls), 1)
        self.assertIn("not retrying", result.stderr)
        self.assertNotIn("transient snapshot.debian.org", result.stderr)

    def test_snapshot_delay_warning_alone_is_not_retried(self):
        # apt prints "W: Tried to start delayed item … snapshot.debian.org"
        # while merely slow, even on a step that later succeeds; that warning
        # must not turn a later genuine failure into a snapshot retry.
        self.env["BUILDCTL_MODE"] = "fail_compile_after_snapshot_delay_warning"
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        calls = self.log.read_text().splitlines()
        self.assertEqual(len(calls), 1)
        self.assertIn("not retrying", result.stderr)
        self.assertNotIn("transient snapshot.debian.org", result.stderr)

    def test_persistent_snapshot_failure_exhausts_own_budget(self):
        self.env["BUILDCTL_MODE"] = "fail_snapshot_always"
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(len(calls), 3)
        for args in calls:
            self.assertNotIn("--no-cache", args)
        self.assertEqual(self.recorded_sleeps(), [122, 302])
        self.assertIn("failure (attempt 1/3)", result.stderr)
        self.assertIn("failure (attempt 2/3)", result.stderr)
        self.assertIn("not retrying", result.stderr)

    def test_package_resolution_failure_is_not_retried(self):
        self.env["BUILDCTL_MODE"] = "fail_package"
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        calls = self.log.read_text().splitlines()
        self.assertEqual(len(calls), 1)
        self.assertIn("not retrying", result.stderr)
        self.assertNotIn("transient snapshot.debian.org", result.stderr)

    def test_job_204104_promisor_drop_is_retried(self):
        trace = JOB_204104_TRACE.read_text(encoding="utf-8")
        result = self.run_with_download_trace(trace, "1")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        # Candidate attempt (dropped) + candidate retry + clean rebuild.
        self.assertEqual(len(calls), 3)
        self.assertNotIn("--no-cache", calls[1])
        self.assertIn("--no-cache", calls[2])
        self.assertIn("transient download failure (attempt 1/3)", result.stderr)
        self.assertNotIn("not retrying", result.stderr)
        self.assertEqual(self.recorded_sleeps(), [62])

    def test_job_204104_promisor_failure_without_rpc_error_is_not_retried(self):
        trace = JOB_204104_TRACE.read_text(encoding="utf-8")
        result = self.run_with_download_trace(
            "".join(line for line in trace.splitlines(keepends=True) if "RPC failed" not in line)
        )
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(len(self.log.read_text().splitlines()), 1)
        self.assertEqual(self.recorded_sleeps(), [])

    def npm_exhausted_trace(self):
        return (
            "#14 3.001 npm error code ECONNRESET\n"
            "#14 3.002 worker-download-retry: transient download retries exhausted (exit 1)\n"
            + self.run_error(14, "/bin/sh -c wdr_retry 'npm ci' '-' 'npm ci' && npm test")
        )

    def test_exhausted_npm_download_is_retried_on_both_builds(self):
        for fail_call, expected in (("1", [False, False, True]), ("2", [False, True, True])):
            with self.subTest(fail_call=fail_call):
                result = self.run_with_download_trace(self.npm_exhausted_trace(), fail_call)
                self.assertEqual(result.returncode, 0, result.stderr)
                calls = [json.loads(line) for line in self.log.read_text().splitlines()]
                self.assertEqual(["--no-cache" in args for args in calls], expected)
                self.assertIn("transient download failure (attempt 1/3)", result.stderr)
                self.assertEqual(self.recorded_sleeps(), [62])

    def test_persistent_download_failure_exhausts_own_budget(self):
        result = self.run_with_download_trace(self.npm_exhausted_trace())
        self.assertNotEqual(result.returncode, 0)
        calls = self.log.read_text().splitlines()
        # Three download attempts, then the candidate gives up; the clean
        # rebuild never runs.
        self.assertEqual(len(calls), 3)
        self.assertEqual(self.recorded_sleeps(), [62, 62])
        self.assertIn("failure (attempt 1/3)", result.stderr)
        self.assertIn("failure (attempt 2/3)", result.stderr)
        self.assertIn("not retrying", result.stderr)

    def test_recovered_download_does_not_retry_later_failing_test(self):
        for test_step in (14, 18):
            with self.subTest(same_step=test_step == 14):
                command = "/bin/bash -o pipefail -c set -eu; "
                if test_step == 14:
                    command += "wdr_retry 'pnpm install' '-' 'pnpm install --frozen-lockfile'; "
                trace = (
                    "#14 0.681 error: RPC failed; curl 55 Failed sending data to the peer\n"
                    "#14 0.681 worker-download-retry: attempt 1/3 failed; retrying in 20s\n"
                    "#14 29.90 Done installing\n"
                    f"#{test_step} 31.25 FAIL worker.test.ts > expected a supported response, got ECONNRESET\n"
                    + self.run_error(test_step, command + "pnpm exec vitest run")
                )
                result = self.run_with_download_trace(trace)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertEqual(len(self.log.read_text().splitlines()), 1)
                self.assertIn("not retrying", result.stderr)
                self.assertEqual(self.recorded_sleeps(), [])

    def test_retry_marker_must_be_terminal_and_belong_to_the_failing_step(self):
        trace = self.npm_exhausted_trace()
        progress, error = trace.rstrip("\n").rsplit("\n", 1)
        cases = {
            "marker in another step": progress.replace("#14 ", "#7 ") + "\n" + error + "\n",
            "mismatched exit": trace.replace("exit code: 1", "exit code: 2"),
            "later output": progress + "\n#14 5.000 FAIL npm test\n" + error + "\n",
            "second failing step": trace + self.run_error(18, "/bin/sh -c make"),
        }
        for label, log in cases.items():
            with self.subTest(case=label):
                result = self.run_with_download_trace(log)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertEqual(len(self.log.read_text().splitlines()), 1)
                self.assertEqual(self.recorded_sleeps(), [])

    def test_pin_and_integrity_failures_are_not_retried(self):
        # WI#862 point 4: a pin mismatch, EINTEGRITY, a sha256sum -c
        # mismatch, a failing test in an install-and-test step, and a 403
        # must each end the job on the first attempt.
        cases = {
            "pin mismatch": "error: pathspec 'deadbeef' did not match any file(s) known to git",
            "EINTEGRITY": "npm error code EINTEGRITY\nnpm error sha512-abc integrity checksum failed",
            "checksum": "example.rpm: FAILED\nsha256sum: WARNING: 1 computed checksum did NOT match",
            "failing test": "not ok 7 - lunarwing bridge rejects a malformed envelope\n# failed 1 test",
            "403": "fatal: unable to access 'https://git.lunarwing.org/lunarwing/lunardepth.git/': The requested URL returned error: 403",
        }
        for label, line in cases.items():
            with self.subTest(case=label):
                trace = "\n".join(f"#14 30.0 {part}" for part in line.splitlines()) + "\n"
                trace += self.run_error(14, "/bin/sh -c wdr_retry 'npm ci' '-' 'npm ci' && npm test")
                result = self.run_with_download_trace(trace)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(len(self.log.read_text().splitlines()), 1)
                self.assertIn("not retrying", result.stderr)
                self.assertNotIn("transient download failure", result.stderr)
                self.assertEqual(self.recorded_sleeps(), [])

    def test_compile_error_is_not_retried(self):
        self.env["BUILDCTL_MODE"] = "fail_compile"
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(self.log.read_text().splitlines()), 1)
        self.assertIn("not retrying", result.stderr)
        self.assertNotIn("transient download failure", result.stderr)

    def test_every_worker_dockerfile_apt_call_has_a_time_limit(self):
        for lane in (
            "kimicode4lunarwing",
            "opencode4lunarwing",
            "opencode2.4lunarwing",
            "pebble4lunarwing",
        ):
            with self.subTest(lane=lane):
                lines = (ROOT / lane / "Dockerfile").read_text().splitlines()
                apt_lines = [
                    line
                    for line in lines
                    if "apt-get" in line and not line.lstrip().startswith("#")
                ]
                self.assertEqual(len(apt_lines), 8)
                for line in apt_lines:
                    self.assertIn("Acquire::https::Timeout=60", line)
                    if " update" in line:
                        self.assertIn("timeout -k 30 240 apt-get -o ", line)
                        self.assertIn("--error-on=any", line)
                        expected_retries = 3 if line.startswith("      ") else 0
                        self.assertIn(f"Acquire::Retries={expected_retries}", line)
                    elif " install" in line:
                        # Base and runtime installs keep the 600 s limit; the
                        # WITH_TOOLCHAINS install (six-space continuation) gets
                        # 1800 s because build-essential/clang are far larger.
                        if line.startswith("      "):
                            self.assertIn("timeout -k 30 1800 apt-get -o ", line)
                        else:
                            self.assertIn("timeout -k 30 600 apt-get -o ", line)
                        if "Acquire::Retries=0" in line:
                            self.assertIn("--no-install-recommends apt;", line)
                        else:
                            self.assertIn("Acquire::Retries=3", line)
                    else:
                        self.fail(f"unrecognized apt command in {lane}: {line}")


if __name__ == "__main__":
    unittest.main()
