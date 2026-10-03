#!/usr/bin/env python3
"""Regressions for the cargo-deny yanked-gate fix (goals 2.1.4.0 item 20).

cargo-deny run from inside ic/ source-replaces crates.io with ic/vendor/, so
the yanked = "deny" policy degraded to per-crate warning[index-failure] and
the job still exited 0 — pipeline 4811's trace even lost its verdict to the
4 MB log cap those diagnostics caused. These tests pin the unvendored
wrapper contract so the gate cannot silently regress.
"""

import importlib.util
import pathlib
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
        for fragment in ("ls-files", "ic/deny.toml", "--workspace --all-features", "check advisories"):
            with self.subTest(fragment=fragment):
                text = SWEEP.read_text(encoding="utf-8")
                self.assertIn(fragment, text)
                self.wrapper.write_text(text.replace(fragment, ""), encoding="utf-8")
                self.assertTrue(any(fragment in error for error in self.errors(contract)))
                self.wrapper.write_text(text, encoding="utf-8")

    def test_sweep_is_a_full_validation_job(self):
        self.assertIn("satellite-advisory-sweep", config.FULL_VALIDATION_JOBS)


if __name__ == "__main__":
    unittest.main()
