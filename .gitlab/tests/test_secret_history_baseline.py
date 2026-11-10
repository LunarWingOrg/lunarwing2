#!/usr/bin/env python3

import importlib.util
import json
import pathlib
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".gitlab/scripts/refresh-secret-history-baseline.py"
SPEC = importlib.util.spec_from_file_location("secret_history_baseline", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
baseline = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(baseline)


class BaselineTests(unittest.TestCase):
    def write_report(self, directory: str, findings: list[dict]) -> pathlib.Path:
        report = pathlib.Path(directory) / "report.json"
        report.write_text(json.dumps(findings), encoding="utf-8")
        return report

    def test_only_sorted_commit_bound_fingerprints_are_written(self):
        first = f"{'0' * 40}:fixture.txt:generic-api-key:1"
        second = f"{'1' * 40}:other.txt:private-key:2"
        findings = [
            {
                "Fingerprint": second,
                "Secret": "REDACTED",
                "Match": "matched content is not baseline material",
            },
            {"Fingerprint": first, "Secret": "REDACTED"},
            {"Fingerprint": first, "Secret": "REDACTED"},
        ]
        with tempfile.TemporaryDirectory() as temporary:
            pinned = baseline.fingerprints(self.write_report(temporary, findings))
            self.assertEqual(pinned, [first, second])
            output = pathlib.Path(temporary) / "history.ignore"
            baseline.write_baseline(output, pinned)
            content = output.read_text(encoding="utf-8")
        self.assertIn(first, content)
        self.assertIn(second, content)
        self.assertNotIn("matched content", content)
        self.assertNotIn("REDACTED", content)

    def test_unredacted_report_is_rejected(self):
        finding = {
            "Fingerprint": f"{'0' * 40}:fixture.txt:generic-api-key:1",
            "Secret": "not-redacted",
        }
        with tempfile.TemporaryDirectory() as temporary:
            report = self.write_report(temporary, [finding])
            with self.assertRaisesRegex(ValueError, "unredacted"):
                baseline.fingerprints(report)

    def test_path_only_fingerprint_is_rejected(self):
        finding = {
            "Fingerprint": "fixture.txt:generic-api-key:1",
            "Secret": "REDACTED",
        }
        with tempfile.TemporaryDirectory() as temporary:
            report = self.write_report(temporary, [finding])
            with self.assertRaisesRegex(ValueError, "commit-bound"):
                baseline.fingerprints(report)


if __name__ == "__main__":
    unittest.main()
