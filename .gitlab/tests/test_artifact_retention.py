#!/usr/bin/env python3
"""WI#791: artifact retention values and the gate that keeps them honest."""

import importlib.util
import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location(
    "validate_config", ROOT / ".gitlab/scripts/validate_config.py"
)
config = importlib.util.module_from_spec(spec)
spec.loader.exec_module(config)

BACKSTOP = {"default": {"artifacts": {"expire_in": "7 days"}}}


def retention_errors(merged):
    errors = []
    config.validate_artifact_retention(merged, errors)
    return errors


class ExpiryParsingTests(unittest.TestCase):
    def test_supported_units(self):
        self.assertEqual(config.artifact_expiry_seconds("1 day"), 86400)
        self.assertEqual(config.artifact_expiry_seconds("2 days"), 2 * 86400)
        self.assertEqual(config.artifact_expiry_seconds("90 days"), 90 * 86400)
        self.assertEqual(config.artifact_expiry_seconds("1 year"), 365 * 86400)
        self.assertEqual(config.artifact_expiry_seconds("1 week"), 604800)

    def test_compound_durations_are_summed(self):
        self.assertEqual(config.artifact_expiry_seconds("1 hour 30 minutes"), 5400)
        self.assertEqual(config.artifact_expiry_seconds("1 day 12 hours"), 129600)
        self.assertEqual(config.artifact_expiry_seconds("1 day and 12 hours"), None)

    def test_unbounded_or_unreadable_values_are_rejected(self):
        for value in ("never", None, "", "two days", "2", "0 days", 90):
            with self.subTest(value=value):
                self.assertIsNone(config.artifact_expiry_seconds(value))


class ArtifactRetentionGateTests(unittest.TestCase):
    def test_job_without_expiry_is_rejected(self):
        errors = retention_errors({**BACKSTOP, "job": {"artifacts": {"paths": ["out"]}}})
        self.assertTrue(any("job declares artifacts" in error for error in errors))

    def test_unbounded_expiry_is_rejected_even_with_the_backstop(self):
        merged = {**BACKSTOP, "job": {"artifacts": {"expire_in": "never"}}}
        errors = retention_errors(merged)
        self.assertTrue(any("job declares artifacts" in error for error in errors))

    def test_rootfs_tar_must_expire_within_two_days(self):
        rootfs_paths = (
            "opencode2-rootfs.tar",
            "lunardepth-rootfs.tar",
            "opencode2-rootfs-export",
        )
        for path in rootfs_paths:
            with self.subTest(path=path):
                merged = {
                    **BACKSTOP,
                    "candidate": {"artifacts": {"expire_in": "30 days", "paths": [path]}},
                }
                self.assertTrue(
                    any("rootfs tar" in error for error in retention_errors(merged))
                )
        ok = {
            **BACKSTOP,
            "candidate": {"artifacts": {"expire_in": "2 days", "paths": ["worker-rootfs.tar"]}},
        }
        self.assertEqual(retention_errors(ok), [])

    def test_evidence_must_be_kept_at_least_90_days(self):
        for evidence in config.EVIDENCE_ARTIFACT_NAMES:
            with self.subTest(evidence=evidence):
                merged = {
                    **BACKSTOP,
                    "scan": {"artifacts": {"expire_in": "30 days", "paths": [evidence]}},
                }
                self.assertTrue(
                    any("evidence" in error for error in retention_errors(merged))
                )
                kept = {
                    **BACKSTOP,
                    "scan": {"artifacts": {"expire_in": "90 days", "paths": [evidence]}},
                }
                self.assertEqual(retention_errors(kept), [])

    def test_extends_inherits_a_bounded_expiry(self):
        merged = {
            **BACKSTOP,
            ".template": {"artifacts": {"expire_in": "2 days"}},
            "job": {"extends": ".template", "artifacts": {"paths": ["out"]}},
        }
        self.assertEqual(retention_errors(merged), [])

    def test_truncated_extends_chain_is_refused(self):
        merged = {**BACKSTOP}
        parent = "leaf"
        merged[parent] = {}
        for depth in range(config.EXTENDS_DEPTH_BUDGET + 2):
            name = f".level{depth}"
            merged[name] = {"extends": parent}
            parent = name
        merged["job"] = {"extends": parent}
        errors = retention_errors(merged)
        self.assertTrue(any("extends chain deeper" in error for error in errors))

    def test_missing_default_backstop_is_rejected(self):
        errors = retention_errors({"job": {"artifacts": {"expire_in": "2 days"}}})
        self.assertTrue(any("default:artifacts" in error for error in errors))

    def test_deprecated_global_artifacts_is_a_bounded_backstop(self):
        merged = {**BACKSTOP, "artifacts": {"paths": ["out"]}}
        self.assertTrue(
            any("top-level artifacts" in error for error in retention_errors(merged))
        )
        bounded = {**BACKSTOP, "artifacts": {"expire_in": "1 day"}}
        self.assertEqual(retention_errors(bounded), [])

    def test_retention_longer_than_one_year_is_rejected(self):
        merged = {**BACKSTOP, "job": {"artifacts": {"expire_in": "2 years"}}}
        self.assertTrue(any("one year" in error for error in retention_errors(merged)))


class CommittedRetentionTests(unittest.TestCase):
    def merged_config(self):
        root = config.load_yaml(config.ROOT_CONFIG)
        merged = {}
        for include in root.get("include", []):
            path = config.ROOT / include["local"].lstrip("/")
            merged = config.merge_ci_hashes(merged, config.load_yaml(path))
        return merged

    def test_repository_passes_the_gate(self):
        errors = retention_errors(self.merged_config())
        self.assertEqual(errors, [])

    def test_reviewed_expiry_values(self):
        merged = self.merged_config()
        expected = {
            ".worker-candidate": "2 days",
            "worker-opencode2-candidate": "2 days",
            "worker-lunardepth-candidate": "2 days",
            ".worker-scan": "90 days",
            ".worker-promotion": "1 year",
        }
        for name, expire_in in expected.items():
            with self.subTest(job=name):
                effective = config.resolve_extends(merged[name], merged)
                self.assertEqual(effective["artifacts"]["expire_in"], expire_in)

    def test_default_backstop_is_present(self):
        merged = self.merged_config()
        self.assertEqual(merged["default"]["artifacts"]["expire_in"], "7 days")

    def test_no_rootfs_artifact_outlives_its_pipeline(self):
        merged = self.merged_config()
        for name in merged:
            if name in config.RESERVED_TOP_LEVEL_KEYS:
                continue
            effective = config.resolve_extends(merged[name], merged)
            artifacts = effective.get("artifacts")
            if not isinstance(artifacts, dict):
                continue
            paths = [str(path) for path in artifacts.get("paths", [])]
            if not any("rootfs" in path for path in paths):
                continue
            seconds = config.artifact_expiry_seconds(artifacts.get("expire_in"))
            with self.subTest(job=name):
                self.assertIsNotNone(seconds)
                self.assertLessEqual(seconds, 2 * 86400)


if __name__ == "__main__":
    unittest.main()
