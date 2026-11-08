#!/usr/bin/env python3
"""Regression coverage for WI#566's two-pass current-tree secret scan."""

import os
import pathlib
import shlex
import shutil
import stat
import subprocess
import tempfile
import tomllib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".gitlab/scripts/scan-secrets.sh"
CONFIG = ROOT / ".gitlab/gitleaks-vendor.toml"
IGNORE = ROOT / ".gitleaksignore"

GITLEAKS_STUB = """#!/usr/bin/env bash
set -euo pipefail
count=0
if [[ -f "$STUB_STATE/count" ]]; then
    count=$(<"$STUB_STATE/count")
fi
count=$((count + 1))
printf '%s\\n' "$count" > "$STUB_STATE/count"
printf '%s\\n' "$*" >> "$STUB_STATE/calls"
if ((count == 1)); then
    [[ ! -e ic/vendor ]] || exit 91
    [[ "$*" == "dir --redact=100 --no-banner --no-color ." ]] || exit 92
    exit "${STUB_PRIMARY_RESULT:-0}"
fi
if ((count == 2)); then
    [[ -f ic/vendor/crate/lib.rs ]] || exit 93
    [[ -f .gitlab/gitleaks-vendor.toml ]] || exit 94
    expected="dir --redact=100 --no-banner --no-color --config $PWD/.gitlab/gitleaks-vendor.toml ic/vendor"
    [[ "$*" == "$expected" ]] || exit 95
    exit "${STUB_VENDOR_RESULT:-0}"
fi
exit 96
"""


class VendorConfigTests(unittest.TestCase):
    def test_only_fixture_heavy_default_rules_are_disabled(self):
        config = tomllib.loads(CONFIG.read_text(encoding="utf-8"))
        self.assertTrue(config["extend"]["useDefault"])
        self.assertEqual(
            set(config["extend"]["disabledRules"]),
            {"generic-api-key", "pkcs12-file", "private-key"},
        )
        self.assertNotIn("allowlists", config)

    def test_vendor_suppressions_are_exact_fingerprints(self):
        entries = [
            line
            for line in IGNORE.read_text(encoding="utf-8").splitlines()
            if line.startswith("ic/vendor/")
        ]
        self.assertTrue(entries)
        for entry in entries:
            path, rule, line_number = entry.rsplit(":", 2)
            with self.subTest(entry=entry):
                self.assertTrue(path.startswith("ic/vendor/"))
                self.assertFalse(set(path) & set("*?[]"))
                self.assertTrue(rule)
                self.assertTrue(line_number.isdigit())


@unittest.skipUnless(
    shutil.which("git") and shutil.which("bash"), "git/bash not available"
)
class ScanSecretsTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = pathlib.Path(temporary.name)
        self.root = self.base / "repo"
        (self.root / ".gitlab").mkdir(parents=True)
        (self.root / "ic/vendor/crate").mkdir(parents=True)
        shutil.copy(CONFIG, self.root / ".gitlab/gitleaks-vendor.toml")
        shutil.copy(IGNORE, self.root / ".gitleaksignore")
        (self.root / "ic/vendor/crate/lib.rs").write_text(
            "pub fn fixture() {}\n", encoding="utf-8"
        )
        (self.root / "README.md").write_text("fixture\n", encoding="utf-8")
        self.run_git("init", "-q")
        self.run_git("config", "user.name", "CI Test")
        self.run_git("config", "user.email", "ci-test@example.invalid")
        self.run_git("add", ".")
        self.run_git(
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-qm",
            "fixture",
        )
        self.state = self.base / "state"
        self.state.mkdir()
        self.bin = self.base / "bin"
        self.bin.mkdir()
        stub = self.bin / "gitleaks"
        stub.write_text(GITLEAKS_STUB, encoding="utf-8")
        stub.chmod(stub.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    def run_git(self, *args):
        subprocess.run(
            ["git", *args],
            cwd=self.root,
            check=True,
            capture_output=True,
            text=True,
            timeout=30,
        )

    def run_scan(self, primary_result=0, vendor_result=0):
        env = dict(os.environ)
        env.pop("CI_COMMIT_SHA", None)
        env.update(
            {
                "PATH": f"{self.bin}{os.pathsep}{env['PATH']}",
                "STUB_STATE": str(self.state),
                "STUB_PRIMARY_RESULT": str(primary_result),
                "STUB_VENDOR_RESULT": str(vendor_result),
            }
        )
        return subprocess.run(
            ["bash", str(SCRIPT)],
            cwd=self.root,
            env=env,
            capture_output=True,
            text=True,
            timeout=60,
        )

    def calls(self):
        return [
            shlex.split(line)
            for line in (self.state / "calls")
            .read_text(encoding="utf-8")
            .splitlines()
        ]

    def test_primary_excludes_vendor_and_second_pass_scans_it(self):
        result = self.run_scan()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.calls()
        self.assertEqual(len(calls), 2)
        self.assertEqual(
            calls[0], ["dir", "--redact=100", "--no-banner", "--no-color", "."]
        )
        self.assertEqual(calls[1][-1], "ic/vendor")
        config_arg = calls[1][calls[1].index("--config") + 1]
        self.assertEqual(config_arg.rsplit("/", 1)[-1], CONFIG.name)

    def test_vendor_finding_fails_the_job(self):
        result = self.run_scan(vendor_result=1)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(len(self.calls()), 2)

    def test_primary_finding_stops_before_vendor_pass(self):
        result = self.run_scan(primary_result=1)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(len(self.calls()), 1)


if __name__ == "__main__":
    unittest.main()
