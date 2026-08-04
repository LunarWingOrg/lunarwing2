#!/usr/bin/env python3
"""Non-destructive scanner/parser tests using synthetic canaries only."""

import importlib.util
import io
import json
import tempfile
import unittest
from argparse import Namespace
from contextlib import redirect_stdout
from pathlib import Path

MODULE = Path(__file__).parents[1] / "live-tests" / "darkirc_h3_analyzer.py"
SPEC = importlib.util.spec_from_file_location("darkirc_h3_analyzer", MODULE)
analyzer = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(analyzer)


class AnalyzerTests(unittest.TestCase):
    def test_extracts_json_and_toml_secret_fields(self):
        secret_a = "synthetic-private-key-A-123456"
        secret_b = "synthetic-private-key-B-654321"
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "pending.json").write_text(json.dumps({"secret": {"local_private": secret_a}}))
            (root / "contacts.toml").write_text(f'my_dm_chacha_secret = "{secret_b}"\n')
            self.assertEqual(set(analyzer.extract_file(root / "pending.json")), {secret_a})
            self.assertEqual(set(analyzer.extract_file(root / "contacts.toml")), {secret_b})

    def test_scanner_reports_exact_canary_only(self):
        secret = b"synthetic-private-key-C-abcdef"
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            clean = root / "clean"
            leaked = root / "leaked"
            clean.write_bytes(b"public data")
            leaked.write_bytes(b"prefix " + secret + b" suffix")
            self.assertEqual(analyzer.scan_paths([secret], [clean]), (1, 0))
            self.assertEqual(analyzer.scan_paths([secret], [leaked]), (1, 1))

    def test_public_shape_rejects_secret_keys_recursively(self):
        self.assertTrue(analyzer.public_shape_safe({"public_key": "ok", "nested": [1, 2]}))
        self.assertFalse(analyzer.public_shape_safe({"nested": {"local_private": "no"}}))

    def test_public_artifact_requires_exact_known_schema(self):
        artifact = {key: None for key in analyzer.OFFER_KEYS}
        artifact["artifact_role"] = "initiator_offer"
        self.assertTrue(analyzer.public_artifact_safe(artifact))
        artifact["unexpected"] = "value"
        self.assertFalse(analyzer.public_artifact_safe(artifact))

    def test_scan_cli_returns_failure_without_printing_canary(self):
        secret = "synthetic-private-key-D-fedcba"
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            canaries = root / "canaries.json"
            capture = root / "capture"
            canaries.write_text(json.dumps([secret]))
            capture.write_text(secret)
            output = io.StringIO()
            with redirect_stdout(output):
                rc = analyzer.cmd_scan(Namespace(canaries=str(canaries), surface=[f"stderr={capture}"]))
            self.assertEqual(rc, 1)
            self.assertIn("surface=stderr status=FAIL", output.getvalue())
            self.assertNotIn(secret, output.getvalue())

    def test_proc_coverage_requires_every_marker(self):
        with tempfile.TemporaryDirectory() as raw:
            capture = Path(raw) / "proc.bin"
            capture.write_bytes(
                b"H3PROC pid=1 exe=/usr/bin/bash\n"
                b"lunarwing-mt-admin.sh\x00\n"
                b"H3PROC pid=2 exe=/release/lunarwing-darkirc-key-helper\n"
                b"H3PROC pid=3 exe=/release/darkirc\n"
            )
            args = Namespace(
                capture=[str(capture)],
                expect_cmdline=["lunarwing-mt-admin.sh"],
                expect_exe=["lunarwing-darkirc-key-helper", "darkirc"],
            )
            self.assertEqual(analyzer.cmd_verify_proc(args), 0)
            args.expect_exe.append("missing-process")
            with self.assertRaises(SystemExit):
                analyzer.cmd_verify_proc(args)

    def test_proc_coverage_does_not_accept_daemon_argument_as_executable(self):
        with tempfile.TemporaryDirectory() as raw:
            capture = Path(raw) / "proc.bin"
            capture.write_bytes(
                b"H3PROC pid=1 exe=/release/lunarwing-darkirc-key-helper\n"
                b"--binary-path\x00/release/darkirc\x00"
            )
            args = Namespace(
                capture=[str(capture)],
                expect_cmdline=[],
                expect_exe=["lunarwing-darkirc-key-helper", "darkirc"],
            )
            with self.assertRaises(SystemExit):
                analyzer.cmd_verify_proc(args)


if __name__ == "__main__":
    unittest.main()
