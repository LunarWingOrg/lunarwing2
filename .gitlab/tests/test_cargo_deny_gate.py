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
        fragments = (
            "ls-files",
            "ic/deny.toml",
            "--workspace --all-features",
            "check advisories bans",
            "pebble4lunarwing/upstream/Cargo.lock",
            '"$audit_bin" audit',
            '--file "$repo_root/$override_lock"',
            '--db "$db_dir"',
            "--no-fetch --stale",
            "-D unsound -D yanked",
        )
        for fragment in fragments:
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

    def test_ci_installs_cargo_audit_before_the_sweep(self):
        self.assertEqual(
            config.ADVISORY_GATE_SCRIPTS["satellite-advisory-sweep"],
            [
                "bash .gitlab/scripts/install-cargo-deny.sh",
                "bash .gitlab/scripts/install-cargo-audit.sh",
                "bash .gitlab/scripts/check-satellite-advisories.sh",
            ],
        )


class PebbleOverrideAuditTests(unittest.TestCase):
    """WI#626: the manifest-less override uses the existing DB and fails closed."""

    OVERRIDE = "pebble4lunarwing/upstream/Cargo.lock"

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = pathlib.Path(temporary.name)
        self.root = self.base / "repo"
        self.root.mkdir()
        self.stub_bin = self.base / "bin"
        self.stub_bin.mkdir()
        self.cargo_home = self.base / "cargo"
        self.db_dir = self.cargo_home / "advisory-dbs/advisory-db-test"
        self.db_dir.mkdir(parents=True)
        self.audit_args = self.base / "audit-args"
        lock = self.root / self.OVERRIDE
        lock.parent.mkdir(parents=True)
        lock.write_text("# test lockfile\n", encoding="utf-8")
        self._stub("git", self.GIT_STUB)
        self._stub("taskset", '#!/bin/sh\nshift 2\nexec "$@"\n')
        self._stub("cargo-audit", self.CARGO_AUDIT_STUB)

    GIT_STUB = """#!/bin/sh
case " $* " in
    *" rev-parse --show-toplevel "*)
        printf '%s\\n' "$FAKE_ROOT"
        ;;
    *" ls-files "*)
        printf '%s\\n' 'pebble4lunarwing/upstream/Cargo.lock'
        ;;
    *" rev-parse HEAD "*)
        printf '%s\\n' '0123456789abcdef0123456789abcdef01234567'
        ;;
    *" log -1 --format=%cs "*)
        printf '%s\\n' '2026-11-12'
        ;;
    *)
        echo "unexpected git invocation: $*" >&2
        exit 2
        ;;
esac
"""

    CARGO_AUDIT_STUB = """#!/bin/sh
printf '%s\\n' "$*" > "$AUDIT_ARGS"
if [ "${AUDIT_MODE:-clean}" = "fail" ]; then
    echo 'error[unsound]: xattr functions mark unwritten bytes as initialized'
    echo 'ID: RUSTSEC-2026-0486'
    echo 'crate: rustix 1.1.4'
    exit 1
fi
echo 'advisories ok'
"""

    def _stub(self, name, text):
        path = self.stub_bin / name
        path.write_text(text, encoding="utf-8")
        path.chmod(0o755)

    def run_sweep(self, mode="clean", cargo_home=None):
        env = dict(os.environ)
        env.update(
            {
                "PATH": f"{self.stub_bin}:{os.environ['PATH']}",
                "FAKE_ROOT": str(self.root),
                "AUDIT_ARGS": str(self.audit_args),
                "AUDIT_MODE": mode,
                "CARGO_HOME": str(cargo_home or self.cargo_home),
            }
        )
        return subprocess.run(
            ["bash", str(SWEEP)],
            cwd=self.root,
            capture_output=True,
            text=True,
            env=env,
            timeout=120,
        )

    def test_clean_override_uses_shared_db_without_fetching(self):
        result = self.run_sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(f"OK   {self.OVERRIDE} (cargo-audit)", result.stdout)
        self.assertIn("advisory-db commit: 0123456789abcdef", result.stdout)
        args = self.audit_args.read_text(encoding="utf-8")
        self.assertIn(f"audit --file {self.root}/{self.OVERRIDE}", args)
        self.assertIn(f"--db {self.db_dir}", args)
        self.assertIn("--no-fetch --stale", args)
        self.assertIn("-D unsound -D yanked", args)
        self.assertNotIn("--ignore", args)

    def test_advisory_failure_names_id_and_crate(self):
        result = self.run_sweep(mode="fail")
        output = result.stdout + result.stderr
        self.assertEqual(result.returncode, 1, output)
        self.assertIn(f"FAIL {self.OVERRIDE} (cargo-audit)", output)
        self.assertIn("RUSTSEC-2026-0486", output)
        self.assertIn("rustix 1.1.4", output)

    def test_missing_shared_db_fails_before_audit(self):
        result = self.run_sweep(cargo_home=self.base / "empty-cargo")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("left no advisory database", result.stderr)
        self.assertFalse(self.audit_args.exists())


if __name__ == "__main__":
    unittest.main()
