#!/usr/bin/env python3
"""Regressions for the cargo-deny yanked-gate fix (goals 2.1.4.0 item 20).

cargo-deny run from inside ic/ source-replaces crates.io with ic/vendor/, so
the yanked = "deny" policy degraded to per-crate warning[index-failure] and
the job still exited 0 — pipeline 4811's trace even lost its verdict to the
4 MB log cap those diagnostics caused. These tests pin the unvendored
wrapper contract so the gate cannot silently regress.

CargoDenyRetryTests pin the pipeline #5294 hardening: transient network
diagnostics (index-failure storms from a shared runner egress IP, or a
failed advisory-db fetch) are retried, a run is accepted only when it
completes with zero index-failure diagnostics, and a run that reached a
real audit verdict (error[...]) is never retried.
"""

import importlib.util
import os
import pathlib
import subprocess
import tempfile
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("validate_config", ROOT / ".gitlab/scripts/validate_config.py")
config = importlib.util.module_from_spec(spec)
spec.loader.exec_module(config)

WRAPPER = ROOT / ".gitlab/scripts/cargo-deny-check.sh"


class CargoDenyGateTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        (self.root / ".gitlab/scripts").mkdir(parents=True)
        self.wrapper = self.root / ".gitlab/scripts/cargo-deny-check.sh"
        self.wrapper.write_text(WRAPPER.read_text(encoding="utf-8"), encoding="utf-8")

    def errors(self, merged, root=None):
        errors = []
        with mock.patch.object(config, "ROOT", root or self.root):
            config.validate_cargo_deny_gate(merged, errors)
        return errors

    def test_current_ci_and_wrapper_pass(self):
        self.assertEqual(self.errors({"cargo-deny": {"script": ["bash .gitlab/scripts/cargo-deny-check.sh"]}}), [])

    def test_vendored_invocation_is_rejected(self):
        legacy = {
            "cargo-deny": {
                "script": [
                    "PATH=\"$CI_PROJECT_DIR/.ci-bin:$PATH\" bash .gitlab/scripts/cargo-ci.sh deny check"
                ]
            }
        }
        self.assertTrue(any("cargo-deny-check.sh" in error for error in self.errors(legacy)))

    def test_missing_wrapper_is_rejected(self):
        self.wrapper.unlink()
        self.assertTrue(any("cannot read cargo-deny wrapper" in error for error in self.errors({})))

    def test_wrapper_losing_guards_is_rejected(self):
        contract = {
            "cargo-deny": {"script": ["bash .gitlab/scripts/cargo-deny-check.sh"]}
        }
        for fragment, replacement in (
            ("--manifest-path", ""),
            ("--hide-inclusion-graph", ""),
            ("--workspace --all-features", ""),
            ("advisory-db commit", ""),
            ('grep -q "index-failure"', "true"),
        ):
            with self.subTest(fragment=fragment):
                text = WRAPPER.read_text(encoding="utf-8")
                self.assertIn(fragment, text)
                self.wrapper.write_text(text.replace(fragment, replacement), encoding="utf-8")
                self.assertTrue(any(fragment in error for error in self.errors(contract)))
                self.wrapper.write_text(text, encoding="utf-8")

    def test_wrapper_runs_from_repo_root_not_ic(self):
        text = WRAPPER.read_text(encoding="utf-8")
        self.assertIn('(cd "$repo_root"', text)
        self.assertNotIn('cd "$repo_root/ic"', text)


SWEEP = ROOT / ".gitlab/scripts/check-satellite-advisories.sh"


class CargoDenyRetryTests(unittest.TestCase):
    """Pipeline #5294 hardening: the wrapper retries transient network
    diagnostics but never weakens the yanked/advisory verdicts.

    The wrapper is executed for real against PATH stubs: git/taskset/timeout
    passthroughs and a cargo-deny stub whose per-attempt behavior is scripted
    through a modes file (one line per attempt).
    """

    CARGO_DENY_STUB = """#!/bin/sh
state_dir=$STUB_STATE
count_file="$state_dir/count"
n=1
if [ -f "$count_file" ]; then
    n=$(( $(cat "$count_file") + 1 ))
fi
printf '%s\\n' "$n" > "$count_file"
behavior=$(sed -n "${n}p" "$state_dir/modes")
case "$behavior" in
    ok)
        echo "advisories ok, bans ok, licenses ok, sources ok"
        exit 0
        ;;
    indexfail)
        echo "warning[index-failure]: could not reach index.crates.io"
        echo "advisories ok, bans ok, licenses ok, sources ok"
        exit 0
        ;;
    advisory)
        echo "error[vulnerability]: RUSTSEC-0000-0000 test vulnerability"
        exit 1
        ;;
    fetchfail)
        echo "ERROR: unable to fetch advisory database"
        exit 1
        ;;
    *)
        echo "unexpected stub behavior on attempt $n" >&2
        exit 2
        ;;
esac
"""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = pathlib.Path(temporary.name)
        self.fake_root = self.base / "repo"
        self.fake_root.mkdir()
        self.stub_bin = self.base / "bin"
        self.stub_bin.mkdir()
        self.state = self.base / "state"
        self.state.mkdir()
        self._stub(
            "git",
            '#!/bin/sh\nif [ "$1" = "rev-parse" ]; then printf "%s\\n" "$FAKE_ROOT"; fi\nexit 0\n',
        )
        self._stub("taskset", '#!/bin/sh\nshift 2\nexec "$@"\n')
        self._stub("timeout", '#!/bin/sh\nshift\nexec "$@"\n')
        self._stub("cargo-deny", self.CARGO_DENY_STUB)

    def _stub(self, name, text):
        path = self.stub_bin / name
        path.write_text(text, encoding="utf-8")
        path.chmod(0o755)

    def run_wrapper(self, modes):
        (self.state / "count").write_text("0\n", encoding="utf-8")
        (self.state / "modes").write_text("\n".join(modes) + "\n", encoding="utf-8")
        env = dict(os.environ)
        env.update(
            {
                "PATH": f"{self.stub_bin}:{os.environ['PATH']}",
                "FAKE_ROOT": str(self.fake_root),
                "STUB_STATE": str(self.state),
                # No advisory-dbs here, so the wrapper skips its db-commit line.
                "CARGO_HOME": str(self.base / "cargo"),
            }
        )
        return subprocess.run(
            ["bash", str(WRAPPER)],
            capture_output=True,
            text=True,
            env=env,
            timeout=120,
        )

    def attempts(self):
        return int((self.state / "count").read_text(encoding="utf-8"))

    def test_transient_index_failure_then_clean_run_is_accepted(self):
        result = self.run_wrapper(["indexfail", "ok"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("was inconclusive", result.stderr)
        self.assertIn("advisories ok", result.stdout)
        self.assertEqual(self.attempts(), 2)

    def test_persistent_index_failure_still_fails(self):
        result = self.run_wrapper(["indexfail", "indexfail", "indexfail"])
        self.assertEqual(result.returncode, 1)
        self.assertIn("yanked-crate check was skipped", result.stderr)
        self.assertEqual(self.attempts(), 3)

    def test_real_verdict_is_never_retried(self):
        result = self.run_wrapper(["advisory"])
        self.assertEqual(result.returncode, 1)
        self.assertIn("error[vulnerability]", result.stdout)
        self.assertNotIn("was inconclusive", result.stderr)
        self.assertEqual(self.attempts(), 1)

    def test_fetch_failure_then_clean_run_is_accepted(self):
        result = self.run_wrapper(["fetchfail", "ok"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("was inconclusive", result.stderr)
        self.assertEqual(self.attempts(), 2)

    def test_clean_run_needs_no_retry(self):
        result = self.run_wrapper(["ok"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("was inconclusive", result.stderr)
        self.assertEqual(self.attempts(), 1)


class SatelliteAdvisorySweepTests(unittest.TestCase):
    """The v2.1.5.0 audit's every-lockfile claim must be a CI gate (A5)."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        (self.root / ".gitlab/scripts").mkdir(parents=True)
        self.wrapper = self.root / ".gitlab/scripts/check-satellite-advisories.sh"
        self.wrapper.write_text(SWEEP.read_text(encoding="utf-8"), encoding="utf-8")

    def errors(self, merged):
        errors = []
        with mock.patch.object(config, "ROOT", self.root):
            config.validate_satellite_advisory_sweep(merged, errors)
        return errors

    def test_current_ci_and_wrapper_pass(self):
        merged = {"satellite-advisory-sweep": {"script": ["bash .gitlab/scripts/check-satellite-advisories.sh"]}}
        self.assertEqual(self.errors(merged), [])

    def test_missing_job_or_invocation_is_rejected(self):
        self.assertTrue(any("check-satellite-advisories.sh" in error for error in self.errors({})))
        merged = {"satellite-advisory-sweep": {"script": ["bash .gitlab/scripts/cargo-deny-check.sh"]}}
        self.assertTrue(any("check-satellite-advisories.sh" in error for error in self.errors(merged)))

    def test_missing_wrapper_is_rejected(self):
        self.wrapper.unlink()
        merged = {"satellite-advisory-sweep": {"script": ["bash .gitlab/scripts/check-satellite-advisories.sh"]}}
        self.assertTrue(any("cannot read satellite advisory sweep" in error for error in self.errors(merged)))

    def test_wrapper_losing_guards_is_rejected(self):
        contract = {"satellite-advisory-sweep": {"script": ["bash .gitlab/scripts/check-satellite-advisories.sh"]}}
        for fragment in ("ls-files", "ic/deny.toml", "--workspace --all-features", "check advisories bans"):
            with self.subTest(fragment=fragment):
                text = SWEEP.read_text(encoding="utf-8")
                self.assertIn(fragment, text)
                # IS-1: retaining advisories while dropping bans must fail.
                replacement = "check advisories" if fragment == "check advisories bans" else ""
                self.wrapper.write_text(text.replace(fragment, replacement), encoding="utf-8")
                self.assertTrue(any(fragment in error for error in self.errors(contract)))
                self.wrapper.write_text(text, encoding="utf-8")

    def test_sweep_is_a_full_validation_job(self):
        self.assertIn("satellite-advisory-sweep", config.FULL_VALIDATION_JOBS)


if __name__ == "__main__":
    unittest.main()
