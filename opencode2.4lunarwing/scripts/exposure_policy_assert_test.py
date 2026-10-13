#!/usr/bin/env python3
"""Self-test for the WI#357 exposure policy checker and binary wrapper.

Drives exposure_policy_assert.py against fixture lane trees (pass and each
failure mode) and exercises opencode-exposure-guard.sh end to end with
/bin/echo standing in for the real opencode binary, so the pass-through
argument contract is asserted without needing the pinned image.
"""

from __future__ import annotations

import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

LANE_ROOT = pathlib.Path(__file__).resolve().parent.parent
ASSERT = LANE_ROOT / "scripts" / "exposure_policy_assert.py"
WRAPPER = LANE_ROOT / "scripts" / "opencode-exposure-guard.sh"


def write_lane(root: pathlib.Path, *, name: str = "opencode2.4lunarwing") -> pathlib.Path:
    lane = root / name
    (lane / "config").mkdir(parents=True)
    (lane / "scripts").mkdir()
    (lane / "config" / "opencode2.json").write_text(
        json.dumps(
            {
                "share": "disabled",
                "update": "disable",
                "permissions": [
                    {"action": "shell", "resource": f"*opencode {sub} *", "effect": "deny"}
                    for sub in ("service", "pair", "serve")
                ],
            }
        ),
        encoding="utf-8",
    )
    (lane / "entrypoint.sh").write_text(
        "# comment naming opencode service mode is prose, not an invocation\n"
        'OPENCODE_SERVE_HOST="${OPENCODE_SERVE_HOST:-127.0.0.1}"\n'
        'OPENCODE_BIN="${OPENCODE_BIN:-/usr/local/bin/opencode}"\n'
        'mkdir -p "$WORKSPACE_ROOT/.opencode"\n'
        'rm -f "$WORKSPACE_ROOT/.opencode/opencode2.json"\n'
        'cp /app/config/opencode2.json "$WORKSPACE_ROOT/.opencode/opencode2.json"\n'
        'export OPENCODE_PASSWORD="${OPENCODE_PASSWORD:-x}"\n'
        '"$OPENCODE_BIN" serve --port "$OPENCODE_SERVE_PORT" \\\n'
        '  --hostname "$OPENCODE_SERVE_HOST"\n',
        encoding="utf-8",
    )
    shutil.copy(WRAPPER, lane / "scripts" / WRAPPER.name)
    return lane


def run_assert(*args: str) -> subprocess.CompletedProcess[str]:
    # The profile is pinned explicitly: inside the image the lane is unpacked
    # at /app, where directory-name inference would select the v1 profile.
    return subprocess.run(
        [sys.executable, str(ASSERT), "--profile", "opencode2", *args],
        capture_output=True,
        text=True,
        timeout=60,
    )


class ExposurePolicyAssertTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.lane = write_lane(self.root)

    def test_real_lane_passes(self) -> None:
        result = run_assert()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("all checks passed", result.stdout)

    def test_fixture_lane_passes(self) -> None:
        result = run_assert("--lane", str(self.lane))
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_missing_share_pin_fails(self) -> None:
        config = json.loads((self.lane / "config" / "opencode2.json").read_text())
        del config["share"]
        (self.lane / "config" / "opencode2.json").write_text(json.dumps(config))
        result = run_assert("--lane", str(self.lane))
        self.assertEqual(result.returncode, 1)
        self.assertIn('must pin "share": "disabled"', result.stderr)

    def test_missing_deny_rule_fails(self) -> None:
        config = json.loads((self.lane / "config" / "opencode2.json").read_text())
        config["permissions"] = config["permissions"][:1]
        (self.lane / "config" / "opencode2.json").write_text(json.dumps(config))
        result = run_assert("--lane", str(self.lane))
        self.assertEqual(result.returncode, 1)
        self.assertIn("must deny shell resources", result.stderr)

    def test_service_invocation_in_entrypoint_fails(self) -> None:
        with (self.lane / "entrypoint.sh").open("a", encoding="utf-8") as handle:
            handle.write('"$OPENCODE_BIN" service start\n')
        result = run_assert("--lane", str(self.lane))
        self.assertEqual(result.returncode, 1)
        self.assertIn("invokes 'opencode service'", result.stderr)

    def test_service_flag_in_entrypoint_fails(self) -> None:
        entrypoint = self.lane / "entrypoint.sh"
        entrypoint.write_text(
            entrypoint.read_text(encoding="utf-8").replace(
                '"$OPENCODE_BIN" serve --port',
                '"$OPENCODE_BIN" serve --service --port',
            ),
            encoding="utf-8",
        )
        result = run_assert("--lane", str(self.lane))
        self.assertEqual(result.returncode, 1)
        self.assertIn("passes --service", result.stderr)

    def test_opencode_bin_pointing_at_real_binary_fails(self) -> None:
        entrypoint = self.lane / "entrypoint.sh"
        entrypoint.write_text(
            entrypoint.read_text(encoding="utf-8").replace(
                ":-/usr/local/bin/opencode}", ":-/usr/local/lib/opencode2/opencode}"
            ),
            encoding="utf-8",
        )
        result = run_assert("--lane", str(self.lane))
        self.assertEqual(result.returncode, 1)
        self.assertIn("OPENCODE_BIN", result.stderr)

    def test_password_mentioned_only_in_comment_fails(self) -> None:
        # run-79 EI-8: a substring check is satisfied by prose; the structural
        # export check is not.
        entrypoint = self.lane / "entrypoint.sh"
        entrypoint.write_text(
            entrypoint.read_text(encoding="utf-8").replace(
                'export OPENCODE_PASSWORD="${OPENCODE_PASSWORD:-x}"\n',
                "# OPENCODE_PASSWORD is set by the operator, trust us\n",
            ),
            encoding="utf-8",
        )
        result = run_assert("--lane", str(self.lane))
        self.assertEqual(result.returncode, 1)
        self.assertIn("export OPENCODE_PASSWORD", result.stderr)

    def test_hostname_on_unrelated_line_only_fails(self) -> None:
        # run-79 EI-8: --hostname must be on the serve invocation itself.
        entrypoint = self.lane / "entrypoint.sh"
        entrypoint.write_text(
            entrypoint.read_text(encoding="utf-8").replace(
                '"$OPENCODE_BIN" serve --port "$OPENCODE_SERVE_PORT" \\\n'
                '  --hostname "$OPENCODE_SERVE_HOST"\n',
                '"$OPENCODE_BIN" serve --port "$OPENCODE_SERVE_PORT"\n'
                'echo "--hostname is documented elsewhere"\n',
            ),
            encoding="utf-8",
        )
        result = run_assert("--lane", str(self.lane))
        self.assertEqual(result.returncode, 1)
        self.assertIn("with --hostname", result.stderr)

    def test_workspace_config_not_rederived_fails(self) -> None:
        entrypoint = self.lane / "entrypoint.sh"
        entrypoint.write_text(
            entrypoint.read_text(encoding="utf-8").replace(
                'cp /app/config/opencode2.json "$WORKSPACE_ROOT/.opencode/opencode2.json"\n',
                'true\n',
            ),
            encoding="utf-8",
        )
        result = run_assert("--lane", str(self.lane))
        self.assertEqual(result.returncode, 1)
        self.assertIn("must materialize the runtime config", result.stderr)

    def test_wrapper_first_token_dispatch_regression_fails(self) -> None:
        # run-79 EI-2 shape: a \$1-only dispatch misses leading global flags.
        wrapper = self.lane / "scripts" / WRAPPER.name
        text = wrapper.read_text(encoding="utf-8")
        start = text.index('want_host=0')
        end = text.index('exec "$REAL_OPENCODE"')
        wrapper.write_text(
            text[:start]
            + 'case "${1:-}" in\n'
            + '  service) refuse "service" "service mode is disabled" ;;\n'
            + '  pair) refuse "pair" "pairing links are disabled" ;;\n'
            + "esac\n\n"
            + text[end:],
            encoding="utf-8",
        )
        result = run_assert("--lane", str(self.lane))
        self.assertEqual(result.returncode, 1)
        self.assertIn("every argv token", result.stderr)

    def test_non_loopback_default_fails(self) -> None:
        entrypoint = self.lane / "entrypoint.sh"
        entrypoint.write_text(
            entrypoint.read_text(encoding="utf-8").replace(":-127.0.0.1}", ":-0.0.0.0}"),
            encoding="utf-8",
        )
        result = run_assert("--lane", str(self.lane))
        self.assertEqual(result.returncode, 1)
        self.assertIn("loopback OPENCODE_SERVE_HOST", result.stderr)

    def test_missing_wrapper_fails(self) -> None:
        (self.lane / "scripts" / WRAPPER.name).unlink()
        result = run_assert("--lane", str(self.lane))
        self.assertEqual(result.returncode, 1)
        self.assertIn("opencode-exposure-guard.sh", result.stderr)

    def test_binary_marker_fails(self) -> None:
        binary = self.root / "opencode"
        binary.write_bytes(b"\x7fELF fake build embedding https://opentunnel.xyz relay")
        result = run_assert("--lane", str(self.lane), "--binary", str(binary))
        self.assertEqual(result.returncode, 1)
        self.assertIn("opentunnel.xyz", result.stderr)

    def test_clean_binary_passes(self) -> None:
        binary = self.root / "opencode"
        binary.write_bytes(b"\x7fELF clean pinned build")
        result = run_assert("--lane", str(self.lane), "--binary", str(binary))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("no OpenTunnel client markers", result.stdout)


class ExposureGuardWrapperTests(unittest.TestCase):
    def run_wrapper(self, *args: str) -> subprocess.CompletedProcess[str]:
        env = dict(os.environ)
        env["LUNARWING_REAL_OPENCODE"] = "/bin/echo"
        return subprocess.run(
            ["sh", str(WRAPPER), *args], capture_output=True, text=True, env=env, timeout=60
        )

    def test_service_refused(self) -> None:
        result = self.run_wrapper("service", "set", "remote", "true")
        self.assertEqual(result.returncode, 77)
        self.assertIn("refused 'opencode service'", result.stderr)

    def test_pair_refused(self) -> None:
        result = self.run_wrapper("pair")
        self.assertEqual(result.returncode, 77)
        self.assertIn("refused 'opencode pair'", result.stderr)

    def test_leading_global_flag_bypass_forms_refused(self) -> None:
        # run-79 EI-2 / run-84 EU-2: the v2 CLI takes global/root flags before
        # the subcommand, so a $1-only dispatch misses these.
        for form in (
            ("--print-logs", "service", "install"),
            ("--print-logs", "serve", "--hostname", "0.0.0.0"),
            ("--log-level", "DEBUG", "service"),
            ("--log-level=DEBUG", "pair"),
            ("--auto", "pair"),
        ):
            result = self.run_wrapper(*form)
            self.assertEqual(result.returncode, 77, (form, result.stderr))
            self.assertIn("refused", result.stderr)

    def test_service_flag_refused(self) -> None:
        # run-79 EI-3: serve --service is service mode via a flag.
        for form in (
            ("serve", "--service"),
            ("serve", "--service=true"),
            ("--service",),
            ("run", "--service", "prompt"),
        ):
            result = self.run_wrapper(*form)
            self.assertEqual(result.returncode, 77, (form, result.stderr))
            self.assertIn("service mode is disabled", result.stderr)

    def test_every_hostname_occurrence_is_checked(self) -> None:
        # EU-2: last-value-wins parsing means a second --hostname 0.0.0.0
        # behind a loopback first value still binds all interfaces.
        for form in (
            ("serve", "--hostname", "127.0.0.1", "--hostname", "0.0.0.0"),
            ("--hostname", "0.0.0.0", "serve"),
            ("web", "--hostname", "0.0.0.0"),
            ("--hostname=0.0.0.0",),
        ):
            result = self.run_wrapper(*form)
            self.assertEqual(result.returncode, 77, (form, result.stderr))
            self.assertIn("loopback only", result.stderr)

    def test_dangling_hostname_value_refused(self) -> None:
        result = self.run_wrapper("serve", "--hostname")
        self.assertEqual(result.returncode, 77)
        self.assertIn("missing value", result.stderr)

    def test_non_loopback_serve_refused(self) -> None:
        for form in (("serve", "--hostname", "0.0.0.0"), ("serve", "--hostname=10.0.0.5")):
            result = self.run_wrapper(*form)
            self.assertEqual(result.returncode, 77, form)
            self.assertIn("loopback only", result.stderr)

    def test_loopback_serve_passes_through(self) -> None:
        for form in (
            ("serve", "--port", "4096", "--hostname", "127.0.0.1"),
            ("serve", "--hostname=::1"),
            ("serve", "--hostname", "localhost"),
            ("serve", "--hostname", "[::1]"),
            ("serve", "--port", "4096"),
            ("--hostname=127.0.0.1", "serve", "--port", "4096"),
        ):
            result = self.run_wrapper(*form)
            self.assertEqual(result.returncode, 0, (form, result.stderr))
            self.assertEqual(result.stdout.strip(), " ".join(form))

    def test_other_commands_pass_through(self) -> None:
        # /bin/echo interprets --version itself, so that case asserts only
        # that the wrapper did not refuse; the rest check the exact argv.
        result = self.run_wrapper("--version")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("refused", result.stderr)
        for form in (
            ("run", "fix", "the", "tests"),
            ("run", "restart the service and pair the devices"),
            ("acp",),
            (),
        ):  # no echo flags
            result = self.run_wrapper(*form)
            self.assertEqual(result.returncode, 0, (form, result.stderr))
            self.assertEqual(result.stdout.strip(), " ".join(form))


if __name__ == "__main__":
    unittest.main()
