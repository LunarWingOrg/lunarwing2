#!/usr/bin/env python3

import importlib.util
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

MODULE_PATH = Path(__file__).with_name("render_overlay.py")
SPEC = importlib.util.spec_from_file_location("render_overlay", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class OverlayTests(unittest.TestCase):
    def setUp(self):
        self.template = Path(__file__).parents[1] / "config" / "lunarwing-lane.patch.yml"

    def test_renders_literal_route_without_secret_material(self):
        rendered = MODULE.render(
            self.template.read_text(encoding="utf-8"),
            "https://tensormoon.example.test/openai/v1",
            "tensormoon::tenant_model::lunarwing",
            False,
        )
        self.assertIn("baseURL: 'https://tensormoon.example.test/openai/v1'", rendered)
        self.assertEqual(rendered.count("tensormoon::tenant_model::lunarwing"), 3)
        self.assertNotIn("baseURLEnv:", rendered)
        self.assertNotIn("AGENT_AUTH_TOKEN", rendered)
        self.assertNotIn("LUNARDEPTH_API_KEY:", rendered)
        self.assertIn("authToken: !!js process.env.LUNARDEPTH_LOCAL_AUTH_TOKEN", rendered)

    def test_transport_validation(self):
        MODULE.validate_route("http://127.0.0.1:3000/openai/v1", "tensormoon::local::lunarwing", False)
        MODULE.validate_route("http://[::1]:3000/openai/v1", "tensormoon::local::lunarwing", False)
        with self.assertRaisesRegex(ValueError, "cleartext HTTP"):
            MODULE.validate_route("http://10.0.0.2/openai/v1", "tensormoon::local::lunarwing", False)
        MODULE.validate_route("http://10.0.0.2/openai/v1", "tensormoon::local::lunarwing", True)
        with self.assertRaisesRegex(ValueError, "model must"):
            MODULE.validate_route("https://example.test/openai/v1", "other-model", False)
        with self.assertRaisesRegex(ValueError, "API root"):
            MODULE.validate_route("https://example.test/openai/v1/chat/completions", "tensormoon::x::lunarwing", False)
        with self.assertRaisesRegex(ValueError, "whitespace or control"):
            MODULE.validate_route("https://example.test/openai/v1\nPLANTED=value", "tensormoon::x::lunarwing", False)

    def test_atomic_mode_and_drift_check(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "overlay.yml"
            args = [
                "--template", str(self.template),
                "--base-url", "https://example.test/openai/v1",
                "--model", "tensormoon::tenant::lunarwing",
            ]
            original_argv = list(os.sys.argv)
            try:
                os.sys.argv = ["render_overlay.py", *args, "--output", str(output)]
                MODULE.main()
                self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o444)
                os.sys.argv = ["render_overlay.py", *args, "--check", str(output)]
                MODULE.main()
                os.chmod(output, 0o644)
                output.write_text(output.read_text(encoding="utf-8").replace("tenant", "other"), encoding="utf-8")
                with self.assertRaisesRegex(ValueError, "drift"):
                    MODULE.main()
            finally:
                os.sys.argv = original_argv

    def test_stdout_mode_matches_the_installed_file_byte_for_byte(self):
        # mt-admin runs the renderer inside the worker image with --stdout and
        # stores the bytes; the task container then runs --check on that file.
        # The two modes must agree exactly, in a fresh interpreter in isolated
        # mode, which is how mt-admin starts it.
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "overlay.yml"
            script = Path(__file__).with_name("render_overlay.py")
            args = [
                "--template", str(self.template),
                "--base-url", "https://example.test/openai/v1",
                "--model", "tensormoon::tenant::lunarwing",
            ]
            printed = subprocess.run(
                [sys.executable, "-I", str(script), *args, "--stdout"],
                check=True, capture_output=True,
            )
            self.assertEqual(printed.stderr, b"")
            subprocess.run([sys.executable, "-I", str(script), *args, "--output", str(output)], check=True)
            self.assertEqual(printed.stdout, output.read_bytes())
            staged = Path(directory) / "staged.yml"
            staged.write_bytes(printed.stdout)
            subprocess.run([sys.executable, "-I", str(script), *args, "--check", str(staged)], check=True)
            both = subprocess.run(
                [sys.executable, "-I", str(script), *args, "--stdout", "--output", str(output)],
                capture_output=True,
            )
            self.assertNotEqual(both.returncode, 0)
            self.assertEqual(both.stdout, b"")


if __name__ == "__main__":
    unittest.main()
