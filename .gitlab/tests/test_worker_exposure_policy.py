#!/usr/bin/env python3
"""Regression checks for the worker exposure policy guard (WI#357, control 4).

The guard pins "workers must never self-expose" for the opencode lanes in
plain CI: lane config pins, entrypoint invariants, the binary wrapper, the
bridge session ruleset (including deny-after-allow ordering), and the
Dockerfile wiring. These tests run the real script against the real tree
(pass) and against fixture trees with each guard surface broken (fail).
"""

import os
import pathlib
import shutil
import stat
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".gitlab" / "scripts" / "check-worker-exposure-policy.sh"

FAKE_GIT = """#!/usr/bin/env bash
if [[ "$*" == "rev-parse --show-toplevel" ]]; then
    echo "$FAKE_ROOT"
    exit 0
fi
echo "fake git: unexpected args: $*" >&2
exit 1
"""

EXECUTOR_RULESET = """
function sessionPermissions() {
  return [
    { action: "shell", resource: "*", effect: "allow" },
    { action: "shell", resource: "*opencode service *", effect: "deny" },
    { action: "shell", resource: "*opencode pair *", effect: "deny" },
    { action: "shell", resource: "*opencode serve *", effect: "deny" },
  ]
}
"""

DOCKERFILE_LINES = (
    "COPY --from=builder /out/bin/opencode /usr/local/lib/opencode2/opencode\n",
    "RUN install -D -m 0755 /app/scripts/opencode-exposure-guard.sh /usr/local/bin/opencode \\\n"
    "    && python3 /app/scripts/exposure_policy_assert.py --lane /app --profile opencode2 --binary /usr/local/lib/opencode2/opencode\n",
    "RUN python3 /build/compile_out_remote_tunnel.py /build/opencode\n",
)


class WorkerExposurePolicyGuardTests(unittest.TestCase):
    def setUp(self):
        # Fixture trees must live on an exec-capable filesystem: /tmp is
        # mounted noexec on some dev hosts, and bash silently skips a
        # non-executable PATH shim, which turns the fake git into the real
        # one. The repo's own tree is exec-capable everywhere we run.
        temporary = tempfile.TemporaryDirectory(dir=ROOT)
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        script_dir = self.root / ".gitlab" / "scripts"
        script_dir.mkdir(parents=True)
        shutil.copy(SCRIPT, script_dir / SCRIPT.name)
        # The guard drives the lane's own checker; both lanes travel.
        shutil.copytree(ROOT / "opencode2.4lunarwing", self.root / "opencode2.4lunarwing")
        shutil.copytree(
            ROOT / "opencode4lunarwing",
            self.root / "opencode4lunarwing",
            ignore=shutil.ignore_patterns("kimi-cli", "node_modules", "__pycache__"),
        )
        (self.root / "opencode2.4lunarwing" / "Dockerfile").write_text(
            "".join(DOCKERFILE_LINES), encoding="utf-8"
        )
        (self.root / "opencode2.4lunarwing" / "scripts" / "opencode_task_executor.ts").write_text(
            EXECUTOR_RULESET, encoding="utf-8"
        )
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        shim = bin_dir / "git"
        shim.write_text(FAKE_GIT, encoding="utf-8")
        shim.chmod(shim.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    def run_guard(self, root: pathlib.Path) -> subprocess.CompletedProcess:
        env = dict(os.environ)
        env["PATH"] = f"{root / 'bin'}{os.pathsep}{env['PATH']}"
        env["FAKE_ROOT"] = str(root)
        return subprocess.run(
            ["bash", str(root / ".gitlab" / "scripts" / SCRIPT.name)],
            capture_output=True,
            text=True,
            env=env,
            cwd=root,
            timeout=120,
        )

    def test_real_tree_passes(self):
        result = subprocess.run(
            ["bash", str(SCRIPT)], capture_output=True, text=True, cwd=ROOT, timeout=120
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("worker exposure policy guard: ok", result.stdout)

    def test_fixture_passes(self):
        result = self.run_guard(self.root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_missing_share_pin_fails(self):
        config = self.root / "opencode2.4lunarwing" / "config" / "opencode2.json"
        import json

        data = json.loads(config.read_text())
        del data["share"]
        config.write_text(json.dumps(data))
        result = self.run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn('must pin "share": "disabled"', result.stderr)

    def test_dockerfile_without_wrapper_install_fails(self):
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        dockerfile.write_text(
            dockerfile.read_text().replace(
                "RUN install -D -m 0755 /app/scripts/opencode-exposure-guard.sh /usr/local/bin/opencode",
                "RUN true",
            ),
            encoding="utf-8",
        )
        result = self.run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("no longer installs the exposure-guard wrapper", result.stderr)

    def test_dockerfile_without_compile_out_fails(self):
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        dockerfile.write_text(
            dockerfile.read_text().replace("compile_out_remote_tunnel.py", "nothing.py"),
            encoding="utf-8",
        )
        result = self.run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("compile-out", result.stderr)

    def test_executor_missing_deny_fails(self):
        executor = self.root / "opencode2.4lunarwing" / "scripts" / "opencode_task_executor.ts"
        executor.write_text(
            EXECUTOR_RULESET.replace(
                '    { action: "shell", resource: "*opencode pair *", effect: "deny" },\n', ""
            ),
            encoding="utf-8",
        )
        result = self.run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("lost the shell deny for 'opencode pair'", result.stderr)

    def test_executor_deny_before_allow_fails(self):
        executor = self.root / "opencode2.4lunarwing" / "scripts" / "opencode_task_executor.ts"
        lines = EXECUTOR_RULESET.splitlines(keepends=True)
        allow = next(line for line in lines if '"shell", resource: "*"' in line)
        denies = [line for line in lines if "opencode" in line and "deny" in line]
        reordered = "\n".join(denies + [allow]) + "\n"
        executor.write_text(reordered, encoding="utf-8")
        result = self.run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("ordering regressed", result.stderr)


if __name__ == "__main__":
    unittest.main()
