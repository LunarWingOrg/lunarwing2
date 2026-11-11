#!/usr/bin/env python3

import contextlib
import importlib.util
import io
import json
import pathlib
import tempfile
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".gitlab/scripts/refresh-secret-history-baseline.py"
SPEC = importlib.util.spec_from_file_location("secret_history_baseline", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
baseline = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(baseline)


def fingerprint(digit: str, path: str) -> str:
    return f"{digit * 40}:{path}:generic-api-key:1"


class BaselineTests(unittest.TestCase):
    def write_report(self, directory: str, findings: list[dict]) -> pathlib.Path:
        report = pathlib.Path(directory) / "report.json"
        report.write_text(json.dumps(findings), encoding="utf-8")
        return report

    def test_only_sorted_commit_bound_fingerprints_are_written(self):
        first = fingerprint("0", "fixture.txt")
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
            "Fingerprint": fingerprint("0", "fixture.txt"),
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

    def test_refresh_preserves_existing_and_requires_vendor_acceptance(self):
        existing = {fingerprint("0", "old-fixture.txt")}
        vendor = fingerprint("1", "ic/vendor/example-crate/tests/fixture.txt")
        plan = baseline.plan_refresh(existing, [vendor], set(), set())
        self.assertEqual(plan.pinned, sorted(existing))
        self.assertEqual(plan.unclassified, {vendor})
        self.assertFalse(plan.accepted_vendor)

        plan = baseline.plan_refresh(existing, [vendor], {vendor}, set())
        self.assertEqual(plan.pinned, sorted(existing | {vendor}))
        self.assertEqual(plan.accepted_vendor, {vendor})
        self.assertFalse(plan.unclassified)
        self.assertEqual(baseline.vendor_crate(vendor), "example-crate")

    def test_new_non_vendor_finding_is_not_written_without_acceptance(self):
        existing = {fingerprint("0", "old-fixture.txt")}
        unclassified = fingerprint("1", "new-fixture.txt")
        plan = baseline.plan_refresh(existing, [unclassified], set(), set())
        self.assertEqual(plan.unclassified, {unclassified})
        with tempfile.TemporaryDirectory() as temporary:
            output = pathlib.Path(temporary) / "history.ignore"
            baseline.write_baseline(output, plan.pinned)
            self.assertEqual(baseline.read_fingerprint_file(output), existing)

    def test_explicit_acceptance_allows_classified_non_vendor_finding(self):
        classified = fingerprint("1", "classified-fixture.txt")
        plan = baseline.plan_refresh(set(), [classified], {classified}, set())
        self.assertEqual(plan.pinned, [classified])
        self.assertEqual(plan.accepted, {classified})
        self.assertFalse(plan.unclassified)

    def test_pending_finding_is_never_written_even_when_accepted(self):
        pending = fingerprint("1", "pending-fixture.txt")
        plan = baseline.plan_refresh(set(), [pending], {pending}, {pending})
        self.assertEqual(plan.pending, {pending})
        self.assertEqual(plan.refused, {pending})
        self.assertFalse(plan.accepted)
        with tempfile.TemporaryDirectory() as temporary:
            output = pathlib.Path(temporary) / "history.ignore"
            baseline.write_baseline(output, plan.pinned)
            self.assertFalse(baseline.read_fingerprint_file(output))

    def test_pending_finding_already_in_baseline_is_rejected(self):
        pending = fingerprint("1", "pending-fixture.txt")
        with self.assertRaisesRegex(ValueError, "already present"):
            baseline.plan_refresh({pending}, [], set(), {pending})

    def test_committed_baseline_pins_no_pending_finding(self):
        # Only the refresh helper refuses a pending row. A hand edit of the
        # baseline bypasses it, so the committed files are checked here, on
        # every merge request.
        pinned = baseline.read_fingerprint_file(ROOT / ".gitlab/gitleaks-history-ignore")
        pending = baseline.read_fingerprint_file(ROOT / ".gitlab/gitleaks-history-pending")
        self.assertTrue(pinned)
        self.assertFalse(
            pinned & pending,
            "a finding awaiting disposition is pinned in the history baseline",
        )

    def test_acceptance_file_requires_commit_bound_fingerprints(self):
        with tempfile.TemporaryDirectory() as temporary:
            accepted = pathlib.Path(temporary) / "accepted.ignore"
            accepted.write_text("fixture.txt:generic-api-key:1\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "commit-bound"):
                baseline.read_fingerprint_file(accepted)

    def run_main(self, root, scanned, accepted=None, draft=None):
        output = root / "history.ignore"
        pending = root / "pending.ignore"
        arguments = [
            "refresh-secret-history-baseline.py",
            "--repo",
            str(root),
            "--output",
            str(output),
            "--pending",
            str(pending),
            "--cache",
            str(root / "scan-cache.json"),
        ]
        if accepted is not None:
            arguments.extend(["--accept", str(accepted)])
        if draft is not None:
            arguments.extend(["--draft-accept", str(draft)])
        stdout = io.StringIO()
        stderr = io.StringIO()
        with (
            mock.patch.object(baseline.sys, "argv", arguments),
            mock.patch.object(baseline, "validate_mirror"),
            mock.patch.object(baseline, "mirror_refs_key", return_value=json.dumps(scanned)),
            mock.patch.object(baseline, "scan", return_value=scanned),
            mock.patch.object(
                baseline.subprocess,
                "run",
                return_value=mock.Mock(stdout="v8.30.1\n"),
            ),
            contextlib.redirect_stdout(stdout),
            contextlib.redirect_stderr(stderr),
        ):
            status = baseline.main()
        return status, baseline.read_fingerprint_file(output), stdout.getvalue(), stderr.getvalue()

    def test_cli_requires_acceptance_and_never_accepts_pending(self):
        existing = {fingerprint("0", "old-fixture.txt")}
        unclassified = fingerprint("1", "new-fixture.txt")
        pending = fingerprint("2", "pending-fixture.txt")
        vendor = fingerprint("3", "ic/vendor/example-crate/tests/fixture.txt")
        unmatched = fingerprint("4", "not-in-scan.txt")
        scanned = [unclassified, pending, vendor]
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            baseline.write_baseline(root / "history.ignore", sorted(existing))
            (root / "pending.ignore").write_text(f"{pending}\n", encoding="utf-8")
            status, pinned, stdout, stderr = self.run_main(root, scanned)
            self.assertEqual(status, 1)
            self.assertEqual(pinned, existing)
            self.assertNotIn("example-crate", stdout)
            self.assertIn(unclassified, stderr)
            self.assertIn(vendor, stderr)
            self.assertIn(pending, stderr)
            self.assertIn("example-crate / generic-api-key (1):", stderr)

            accepted = root / "accepted.ignore"
            accepted.write_text(
                f"{unclassified}\n{pending}\n{vendor}\n{unmatched}\n",
                encoding="utf-8",
            )
            status, pinned, stdout, stderr = self.run_main(root, scanned, accepted)
            self.assertEqual(status, 1)
            self.assertEqual(pinned, existing | {vendor, unclassified})
            self.assertNotIn(pending, pinned)
            self.assertIn("accepted classified vendor", stdout)
            self.assertIn("example-crate / generic-api-key (1):", stdout)
            self.assertIn(f"refused although accepted: {pending}", stderr)
            self.assertIn(f"matched no scanned row: {unmatched}", stderr)

    def test_cli_groups_vendor_rows_and_writes_a_draft_without_pending(self):
        existing = {fingerprint("0", "old-fixture.txt")}
        first = fingerprint("1", "ic/vendor/example-crate/tests/first.txt")
        second = fingerprint("2", "ic/vendor/example-crate/tests/second.txt")
        private_key = f"{'3' * 40}:ic/vendor/example-crate/tests/key.txt:private-key:1"
        other_crate = fingerprint("4", "ic/vendor/other-crate/tests/fixture.txt")
        non_vendor = fingerprint("5", "new-fixture.txt")
        pending = fingerprint("6", "pending-fixture.txt")
        scanned = [first, second, private_key, other_crate, non_vendor, pending]
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            baseline.write_baseline(root / "history.ignore", sorted(existing))
            (root / "pending.ignore").write_text(f"{pending}\n", encoding="utf-8")
            draft = root / "draft.ignore"
            status, pinned, stdout, stderr = self.run_main(root, scanned, draft=draft)
            self.assertEqual(status, 1)
            self.assertEqual(pinned, existing)
            self.assertEqual(
                baseline.read_fingerprint_file(draft),
                {first, second, private_key, other_crate, non_vendor},
            )
            self.assertNotIn(pending, baseline.read_fingerprint_file(draft))
            self.assertIn("example-crate / generic-api-key (2):", stderr)
            self.assertIn("example-crate / private-key (1):", stderr)
            self.assertIn("other-crate / generic-api-key (1):", stderr)
            self.assertIn("wrote 5 unclassified fingerprints to draft", stdout)

    def test_scan_cache_reuses_one_scan_only_while_refs_are_unchanged(self):
        scanned = [fingerprint("1", "ic/vendor/example-crate/tests/fixture.txt")]
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            cache = root / "scan-cache.json"
            with (
                mock.patch.object(baseline, "mirror_refs_key", return_value="refs-a"),
                mock.patch.object(baseline, "scan", return_value=scanned) as scanner,
            ):
                first = baseline.scan_with_cache(root, ["gitleaks"], "v8.30.1", cache)
                stdout = io.StringIO()
                with contextlib.redirect_stdout(stdout):
                    second = baseline.scan_with_cache(root, ["gitleaks"], "v8.30.1", cache)
                self.assertEqual(first, scanned)
                self.assertEqual(second, scanned)
                self.assertEqual(scanner.call_count, 1)
                self.assertIn("reusing 1 redacted fingerprints", stdout.getvalue())
                self.assertIn("unchanged mirror refs", stdout.getvalue())

            with (
                mock.patch.object(baseline, "mirror_refs_key", return_value="refs-b"),
                mock.patch.object(baseline, "scan", return_value=scanned) as scanner,
            ):
                baseline.scan_with_cache(root, ["gitleaks"], "v8.30.1", cache)
                self.assertEqual(scanner.call_count, 1)
            self.assertEqual(
                baseline.load_scan_cache(cache, "refs-b", ["different"], "v8.30.1"),
                None,
            )
            cache_text = cache.read_text(encoding="utf-8")
            self.assertNotIn("Secret", cache_text)
            self.assertNotIn("Match", cache_text)

    def test_cli_status_separates_known_pending_from_new_findings(self):
        existing = {fingerprint("0", "old-fixture.txt")}
        unclassified = fingerprint("1", "new-fixture.txt")
        pending = fingerprint("2", "pending-fixture.txt")
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            baseline.write_baseline(root / "history.ignore", sorted(existing))
            (root / "pending.ignore").write_text(f"{pending}\n", encoding="utf-8")

            # Only the known pending row is in history: nothing new, status 0.
            status, pinned, _, stderr = self.run_main(root, [pending])
            self.assertEqual(status, 0)
            self.assertEqual(pinned, existing)
            self.assertIn(f"left unpinned: {pending}", stderr)

            # A new unclassified finding next to it must still be loud.
            status, pinned, _, stderr = self.run_main(root, [pending, unclassified])
            self.assertEqual(status, 1)
            self.assertEqual(pinned, existing)
            self.assertIn(unclassified, stderr)

            # Asking to accept the pending row is refused, and says so.
            accepted = root / "accepted.ignore"
            accepted.write_text(f"{pending}\n", encoding="utf-8")
            status, pinned, _, stderr = self.run_main(root, [pending], accepted)
            self.assertEqual(status, 1)
            self.assertEqual(pinned, existing)
            self.assertIn(f"refused although accepted: {pending}", stderr)


if __name__ == "__main__":
    unittest.main()
