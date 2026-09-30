#!/usr/bin/env python3
"""Regression checks for the required gates that failed in pipeline #4679."""

import copy
import importlib.util
import json
import pathlib
import tempfile
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("validate_config", ROOT / ".gitlab/scripts/validate_config.py")
config = importlib.util.module_from_spec(spec)
spec.loader.exec_module(config)


class CaveponyReleaseTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.files = {
            "ic/registry/tools/cavepony-tool.json": json.dumps({"version": "0.4.3"}),
            "ic/tools-src/cavepony/cavepony-tool.capabilities.json": json.dumps({"version": "0.4.3"}),
            "ic/tools-src/cavepony/Cargo.toml": '[package]\nname = "cavepony-tool"\nversion = "0.4.3"\n',
            "ic/tools-src/cavepony/Cargo.lock": 'version = 4\n[[package]]\nname = "cavepony-tool"\nversion = "0.4.3"\n',
        }
        for relative, text in self.files.items():
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        self.jobs = {"cavepony-wasm": {"variables": {"CAVEPONY_EXPECTED_VERSION": "0.4.3"}}}

    def errors(self):
        errors = []
        with mock.patch.object(config, "ROOT", self.root):
            config.validate_cavepony_version(self.jobs, errors)
        return errors

    def test_consistent_release_passes(self):
        self.assertEqual(self.errors(), [])

    def test_each_stale_version_fails(self):
        for relative, text in self.files.items():
            with self.subTest(file=relative):
                path = self.root / relative
                path.write_text(text.replace("0.4.3", "0.4.2"), encoding="utf-8")
                self.assertTrue(any(relative in error for error in self.errors()))
                path.write_text(text, encoding="utf-8")
        self.jobs["cavepony-wasm"]["variables"]["CAVEPONY_EXPECTED_VERSION"] = "0.4.2"
        self.assertEqual(len(self.errors()), 4)

    def test_missing_or_malformed_metadata_fails(self):
        for relative, text in self.files.items():
            with self.subTest(file=relative):
                path = self.root / relative
                path.unlink()
                self.assertTrue(any(relative in error for error in self.errors()))
                path.write_text("not valid JSON or TOML", encoding="utf-8")
                self.assertTrue(any(relative in error for error in self.errors()))
                path.write_text(text, encoding="utf-8")

    def test_missing_or_invalid_ci_expectation_fails(self):
        for version in (None, "", "0.4", 43):
            with self.subTest(version=version):
                self.jobs["cavepony-wasm"]["variables"]["CAVEPONY_EXPECTED_VERSION"] = version
                self.assertTrue(self.errors())

    def test_missing_or_duplicate_local_package_fails(self):
        relative = "ic/tools-src/cavepony/Cargo.lock"
        path = self.root / relative
        text = self.files[relative]
        for invalid in (text.replace("cavepony-tool", "other-tool"), text + text[text.index("[[package]]"):]):
            with self.subTest(lock=invalid):
                path.write_text(invalid, encoding="utf-8")
                self.assertTrue(self.errors())


class FullValidationTests(unittest.TestCase):
    def setUp(self):
        self.defaults = config.load_yaml(ROOT / ".gitlab/ci/defaults.yml")
        self.merged = {}
        for include in config.EXPECTED_INCLUDES:
            self.merged.update(config.load_yaml(ROOT / include.lstrip("/")))

    def test_checked_in_rules_and_jobs_match_context_contract(self):
        errors = []
        config.validate_release_gates(self.merged, self.defaults, errors)
        config.validate_cavepony_version(self.merged, errors)
        config.validate_test_shapes(self.merged, errors)
        self.assertEqual(errors, [])

    def test_versioned_integration_mrs_get_full_validation(self):
        for target in ("master", "aragents/kimil00p/v2.1.4.0"):
            with self.subTest(target=target):
                self.assertTrue(config.matches_full_validation_context({"source": "merge_request_event", "target": target}))
        self.assertFalse(config.matches_full_validation_context({"source": "merge_request_event", "target": "feature/other"}))

    def test_scratch_branch_full_validation_is_opt_in(self):
        context = {"source": "push", "branch": "cmc/g1/v2.1.4.0"}
        self.assertFalse(config.matches_full_validation_context(context))
        self.assertTrue(config.matches_full_validation_context({**context, "full_validation": "1"}))
        self.assertFalse(config.matches_full_validation_context({**context, "full_validation": "0"}))

    def test_removing_integration_mr_rule_is_rejected(self):
        defaults = copy.deepcopy(self.defaults)
        rules = defaults[".full-validation-context"]["rules"]
        rules[:] = [rule for rule in rules if "CI_MERGE_REQUEST_TARGET_BRANCH_NAME =~" not in rule.get("if", "")]
        errors = []
        config.validate_release_gates(self.merged, defaults, errors)
        self.assertTrue(any("canonical predicate" in error for error in errors))

    def test_unlocked_cavepony_build_is_rejected(self):
        self.merged["cavepony-wasm"]["script"] = [command.replace(" --locked", "") for command in self.merged["cavepony-wasm"]["script"]]
        errors = []
        config.validate_test_shapes(self.merged, errors)
        self.assertTrue(any("cavepony-wasm" in error for error in errors))

    def test_full_validation_does_not_widen_worker_publication(self):
        errors = []
        config.validate_worker_publication(self.merged, errors)
        self.assertEqual(errors, [])
        for template in (".worker-candidate", ".worker-scan", ".worker-promotion"):
            for condition in config.rule_conditions(self.merged[template]):
                with self.subTest(template=template, condition=condition):
                    if condition:
                        self.assertIn('CI_COMMIT_REF_PROTECTED == "true"', condition)
                        self.assertIn('CI_PIPELINE_SOURCE == "push"', condition)
                        self.assertNotIn("LUNARWING_CI_FULL_VALIDATION", condition)


if __name__ == "__main__":
    unittest.main()
