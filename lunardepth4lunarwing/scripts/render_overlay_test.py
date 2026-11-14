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
            "tensormoon::function_name::tenant_function",
            False,
        )
        self.assertIn("baseURL: 'https://tensormoon.example.test/openai/v1'", rendered)
        self.assertEqual(rendered.count("tensormoon::function_name::tenant_function"), 3)
        self.assertNotIn("baseURLEnv:", rendered)
        self.assertNotIn("AGENT_AUTH_TOKEN", rendered)
        self.assertNotIn("LUNARDEPTH_API_KEY:", rendered)
        self.assertIn("authToken: !!js process.env.LUNARDEPTH_LOCAL_AUTH_TOKEN", rendered)

    def test_tensormoon_id_renders_the_template_dialect_unchanged(self):
        # Installed overlays and their launch-contract digests must not move
        # for a TensorMoon route: only the URL and the model id are substituted.
        template = self.template.read_text(encoding="utf-8")
        rendered = MODULE.render(
            template, "https://tensormoon.example.test/openai/v1", "tensormoon::function_name::tenant_function", False
        )
        self.assertEqual(
            rendered,
            template.replace(MODULE.PLACEHOLDER_URL, "'https://tensormoon.example.test/openai/v1'")
            .replace(MODULE.PLACEHOLDER_MODEL, "tensormoon::function_name::tenant_function"),
        )
        self.assertEqual(rendered.count("    provider: tensormoon\n"), 2)
        self.assertIn("      tensormoon:\n        displayName: TensorMoon\n", rendered)

    def test_tensorzero_id_renders_a_tensorzero_provider_route(self):
        template = self.template.read_text(encoding="utf-8")
        url = "https://tensorzero.example.test/openai/v1"
        rendered = MODULE.render(template, url, "tensorzero::function_name::lunarwing", False)
        self.assertIn(f"baseURL: '{url}'", rendered)
        # Worker row, provider model list and agent-default-model name one id.
        self.assertEqual(rendered.count("tensorzero::function_name::lunarwing"), 3)
        self.assertEqual(rendered.count("    provider: tensorzero\n"), 2)
        self.assertIn("      tensorzero:\n        displayName: TensorZero\n", rendered)
        self.assertNotIn("tensormoon", rendered.lower())
        # The dialect changes the provider naming and nothing else.
        self.assertEqual(
            rendered.replace("tensorzero", "tensormoon").replace("TensorZero", "TensorMoon"),
            MODULE.render(
                template, "https://tensormoon.example.test/openai/v1", "tensormoon::function_name::lunarwing", False
            ),
        )
        self.assertNotIn("baseURLEnv:", rendered)
        self.assertNotIn("LUNARDEPTH_API_KEY:", rendered)
        self.assertIn("apiKeyEnv: LUNARDEPTH_API_KEY", rendered)
        self.assertIn("allowInsecureHTTP: false", rendered)
        self.assertIn(
            "allowInsecureHTTP: true",
            MODULE.render(template, "http://10.0.0.2:3000/openai/v1", "tensorzero::function_name::lunarwing", True),
        )

    def test_a_function_other_than_lunarwing_renders_in_both_dialects(self):
        # The function is the operator's choice: the fleet gateway serves
        # FrontierCODE beside lunarwing, and the lane must not pin either.
        template = self.template.read_text(encoding="utf-8")
        for provider in ("tensormoon", "tensorzero"):
            model = f"{provider}::function_name::FrontierCODE"
            with self.subTest(model=model):
                rendered = MODULE.render(template, "https://example.test/openai/v1", model, False)
                self.assertEqual(rendered.count(model), 3)
                self.assertNotIn("function_name::lunarwing", rendered)
                self.assertEqual(rendered.count(f"    provider: {provider}\n"), 2)
                self.assertIn(f"          - id: {model}\n", rendered)

    def test_model_dialects(self):
        url = "https://example.test/openai/v1"
        # Both gateways take the literal function_name keyword in the middle
        # and the function's own name last; any configured function is valid.
        for model in (
            "tensormoon::function_name::lunarwing",
            "tensormoon::function_name::FrontierCODE",
            "tensorzero::function_name::lunarwing",
            "tensorzero::function_name::FrontierCODE",
            "tensorzero::function_name::my-function_2",
        ):
            MODULE.validate_route(url, model, False)
        for model in (
            "tensorone::function_name::lunarwing",
            "TensorZero::function_name::lunarwing",
            # A middle segment other than the keyword is refused by both gateways.
            "tensorzero::tenant_model::lunarwing",
            "tensorzero::lunarwing_agent::lunarwing",
            "tensormoon::tenant_model::lunarwing",
            "tensormoon::FrontierCODE::lunarwing",
            "tensorzero::model_name::lunarwing",
            "tensorzero::function_name::",
            "tensorzero::function_name::a::b",
            "tensorzero::function_name::lunarwing::variant_name::primary",
            "tensorzero::function_name::has.dot",
            "tensorzero::function_name::has space",
            "tensorzero::lunarwing",
            "xtensorzero::function_name::lunarwing",
            "tensorzero::function_name::lunarwing\n",
        ):
            with self.subTest(model=model), self.assertRaisesRegex(ValueError, "model must"):
                MODULE.validate_route(url, model, False)

    def test_provider_placeholder_drift_is_refused_in_both_dialects(self):
        template = self.template.read_text(encoding="utf-8")
        for drifted in (
            template.replace("    provider: tensormoon\n", "    provider: other\n", 1),
            template.replace("      tensormoon:\n", "      other:\n"),
            template.replace("        displayName: TensorMoon\n", ""),
            template + "\n- id: extra\n  config:\n    provider: tensormoon\n",
        ):
            for model in ("tensormoon::function_name::lunarwing", "tensorzero::function_name::lunarwing"):
                with self.subTest(model=model), self.assertRaisesRegex(ValueError, "reviewed pin"):
                    MODULE.render(drifted, "https://example.test/openai/v1", model, False)

    def test_transport_validation(self):
        MODULE.validate_route("http://127.0.0.1:3000/openai/v1", "tensormoon::function_name::lunarwing", False)
        MODULE.validate_route("http://[::1]:3000/openai/v1", "tensormoon::function_name::lunarwing", False)
        with self.assertRaisesRegex(ValueError, "cleartext HTTP"):
            MODULE.validate_route("http://10.0.0.2/openai/v1", "tensormoon::function_name::lunarwing", False)
        MODULE.validate_route("http://10.0.0.2/openai/v1", "tensormoon::function_name::lunarwing", True)
        with self.assertRaisesRegex(ValueError, "model must"):
            MODULE.validate_route("https://example.test/openai/v1", "other-model", False)
        with self.assertRaisesRegex(ValueError, "API root"):
            MODULE.validate_route("https://example.test/openai/v1/chat/completions", "tensormoon::function_name::lunarwing", False)
        with self.assertRaisesRegex(ValueError, "whitespace or control"):
            MODULE.validate_route("https://example.test/openai/v1\nPLANTED=value", "tensormoon::function_name::lunarwing", False)

    def test_atomic_mode_and_drift_check(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "overlay.yml"
            args = [
                "--template", str(self.template),
                "--base-url", "https://example.test/openai/v1",
                "--model", "tensormoon::function_name::tenant",
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
                "--model", "tensormoon::function_name::tenant",
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

    def test_stored_overlay_is_refused_when_the_route_changes_dialect(self):
        # The task container checks the installed bytes against its own route:
        # an overlay rendered for one gateway dialect never passes for the other.
        with tempfile.TemporaryDirectory() as directory:
            script = Path(__file__).with_name("render_overlay.py")
            command = [
                sys.executable, "-I", str(script),
                "--template", str(self.template),
                "--base-url", "https://example.test/openai/v1",
            ]
            tensorzero = ["--model", "tensorzero::function_name::lunarwing"]
            tensormoon = ["--model", "tensormoon::function_name::lunarwing"]
            printed = subprocess.run([*command, *tensorzero, "--stdout"], check=True, capture_output=True)
            self.assertEqual(printed.stderr, b"")
            staged = Path(directory) / "staged.yml"
            staged.write_bytes(printed.stdout)
            subprocess.run([*command, *tensorzero, "--check", str(staged)], check=True)
            drift = subprocess.run([*command, *tensormoon, "--check", str(staged)], capture_output=True, text=True)
            self.assertNotEqual(drift.returncode, 0)
            self.assertIn("effective overlay drift", drift.stderr)


if __name__ == "__main__":
    unittest.main()
