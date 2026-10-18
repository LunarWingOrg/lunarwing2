#!/usr/bin/env python3
"""Self-test for the WI#357 exposure policy checker and binary wrapper.

Drives exposure_policy_assert.py against fixture lane trees (pass and each
failure mode) and exercises opencode-exposure-guard.sh end to end with
/bin/echo standing in for the real opencode binary, so the pass-through
argument contract is asserted without needing the pinned image.
"""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

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
        'SERVICE_CONFIG_DIR="${OPENCODE_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/opencode}"\n'
        'mkdir -p "$SERVICE_CONFIG_DIR"\n'
        'rm -rf -- "$SERVICE_CONFIG_DIR/service.json"\n'
        "printf '%s\\n' '{\"disabled\": true}' > \"$SERVICE_CONFIG_DIR/service.json\"\n"
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

    def test_implicit_service_disable_must_be_executable_before_dispatch(self) -> None:
        entrypoint = self.lane / "entrypoint.sh"
        original = entrypoint.read_text(encoding="utf-8")
        write = "printf '%s\\n' '{\"disabled\": true}' > \"$SERVICE_CONFIG_DIR/service.json\""
        for replacement in ("", "# " + write, write.replace("true", "false"),
                            'case "$MODE" in\n' + write,
                            write.replace("service.json", "wrong.json")):
            with self.subTest(replacement=replacement):
                entrypoint.write_text(original.replace(write, replacement), encoding="utf-8")
                result = run_assert("--lane", str(self.lane))
                self.assertEqual(result.returncode, 1)
                self.assertIn("must write service.json with disabled=true", result.stderr)

    def test_service_reset_must_remove_plants_before_writing(self) -> None:
        # FB-6: a bare redirect hangs on a planted FIFO, crashes on a
        # directory, and follows a planted symlink onto the agent's file; the
        # reset must remove whatever sits at the path first.
        entrypoint = self.lane / "entrypoint.sh"
        original = entrypoint.read_text(encoding="utf-8")
        removal = 'rm -rf -- "$SERVICE_CONFIG_DIR/service.json"'
        for replacement in ("", "# " + removal, removal.replace("service.json", "wrong.json"),
                            removal.replace(" -rf", "")):
            with self.subTest(replacement=replacement):
                entrypoint.write_text(original.replace(removal, replacement), encoding="utf-8")
                result = run_assert("--lane", str(self.lane))
                self.assertEqual(result.returncode, 1)
                self.assertIn("must remove a planted service.json", result.stderr)

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
        # WI#427: v2.0.32 can enable remote access via pair --remote --yes
        # (or -y), including in non-interactive sessions. Refuse all forms.
        for form in (
            ("pair",),
            ("pair", "--remote"),
            ("pair", "--remote", "--yes"),
            ("--print-logs", "pair", "--remote", "-y"),
        ):
            with self.subTest(form=form):
                result = self.run_wrapper(*form)
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
            self.assertIn("explicit service commands are disabled", result.stderr)

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


class InstalledWrapperTests(unittest.TestCase):
    """Run-103 GD-2 pins: --image-filesystem must verify the shipped wrapper.

    A RUN in the final stage between the wrapper-install scan and the final
    assertion can replace the wrapper (or the real binary) while every text
    check still passes. The filesystem check refuses each replacement shape.
    Paths are injected so the scenarios run against temporary trees.
    """

    def setUp(self) -> None:
        # shutil.which needs the wrapper executable; /tmp is noexec on some
        # dev hosts, so the fixture must live on the exec-capable repo tree
        # (same shape as the CI guard suite's fixtures).
        temporary = tempfile.TemporaryDirectory(dir=LANE_ROOT)
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.lane = write_lane(self.root)
        self.bin_dir = self.root / "bin"
        self.bin_dir.mkdir()
        self.wrapper_path = self.bin_dir / "opencode"
        shutil.copy(WRAPPER, self.wrapper_path)
        self.wrapper_path.chmod(0o755)
        self.binary_path = self.root / "lib" / "opencode2" / "opencode"
        self.binary_path.parent.mkdir(parents=True)
        self.binary_path.write_bytes(b"\x7fELF clean pinned build")
        spec = importlib.util.spec_from_file_location("exposure_policy_assert", ASSERT)
        if spec is None or spec.loader is None:  # pragma: no cover - import shape
            raise AssertionError("cannot load exposure_policy_assert")
        self.assert_module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.assert_module)

    def check(self, *path: pathlib.Path, env: dict[str, str] | None = None) -> list[str]:
        self.assert_module.failures.clear()
        # Run-106 GJ-3(a): the build passes the checkout's wrapper digest as
        # the LUNARWING_WRAPPER_SHA256 build arg; the fixture stands in for
        # the checkout with the lane's committed wrapper bytes.
        environ = {
            "PATH": os.pathsep.join(str(p) for p in path),
            "LUNARWING_WRAPPER_SHA256": hashlib.sha256(WRAPPER.read_bytes()).hexdigest(),
        }
        if env:
            environ.update(env)
        with mock.patch.dict(os.environ, environ):
            self.assert_module.check_installed_wrapper(
                self.lane,
                self.wrapper_path,
                self.binary_path,
                owner_uid=os.geteuid(),
            )
        return list(self.assert_module.failures)

    def test_as_built_passes(self) -> None:
        self.assertEqual(self.check(self.bin_dir), [])

    def test_wrapper_replaced_by_the_real_binary_fails(self) -> None:
        self.wrapper_path.write_bytes(self.binary_path.read_bytes())
        failures = self.check(self.bin_dir)
        self.assertEqual(len(failures), 1, failures)
        self.assertIn("byte-identical", failures[0])

    def test_wrapper_symlink_fails(self) -> None:
        self.wrapper_path.unlink()
        self.wrapper_path.symlink_to(WRAPPER)
        failures = self.check(self.bin_dir)
        self.assertEqual(len(failures), 1, failures)
        self.assertIn("regular file", failures[0])

    def test_wrapper_group_writable_fails(self) -> None:
        self.wrapper_path.chmod(0o775)
        failures = self.check(self.bin_dir)
        self.assertEqual(len(failures), 1, failures)
        self.assertIn("group- or other-writable", failures[0])

    def test_real_binary_with_tunnel_marker_fails(self) -> None:
        self.binary_path.write_bytes(
            b"\x7fELF tampered build embedding https://opentunnel.xyz relay"
        )
        failures = self.check(self.bin_dir)
        self.assertEqual(len(failures), 1, failures)
        self.assertIn("opentunnel", failures[0])

    def test_earlier_path_dir_shadowing_the_wrapper_fails(self) -> None:
        shadow = self.root / "shadow"
        shadow.mkdir()
        decoy = shadow / "opencode"
        shutil.copy("/bin/echo", decoy)
        decoy.chmod(0o755)
        failures = self.check(shadow, self.bin_dir)
        self.assertEqual(len(failures), 1, failures)
        self.assertIn("shadows", failures[0])

    def test_missing_wrapper_fails(self) -> None:
        self.wrapper_path.unlink()
        failures = self.check(self.bin_dir)
        self.assertEqual(len(failures), 1, failures)
        self.assertIn("missing or unreadable", failures[0])

    def test_non_executable_wrapper_fails_with_its_own_reason(self) -> None:
        # Run-106 GJ-3(c): a root-owned 0644 wrapper used to be refused with
        # the misleading "earlier PATH dir shadows" text (which skips
        # non-executables); the executable bit now has its own failure.
        self.wrapper_path.chmod(0o644)
        failures = self.check(self.bin_dir)
        self.assertEqual(len(failures), 1, failures)
        self.assertIn("must be executable", failures[0])

    def test_real_opencode_env_override_fails_and_scans_the_target(self) -> None:
        # Run-106 GJ-3(b): the wrapper honours LUNARWING_REAL_OPENCODE, so an
        # ENV retargeting it at a marker-bearing binary used to pass while
        # check_binary scanned only the fixed default path.
        alt = self.root / "alt" / "opencode.real"
        alt.parent.mkdir()
        alt.write_bytes(b"\x7fELF tampered build embedding https://opentunnel.xyz relay")
        failures = self.check(self.bin_dir, env={"LUNARWING_REAL_OPENCODE": str(alt)})
        self.assertTrue(any("LUNARWING_REAL_OPENCODE" in f for f in failures), failures)
        self.assertTrue(any("opentunnel" in f for f in failures), failures)

    def test_real_opencode_env_override_at_the_default_path_passes(self) -> None:
        failures = self.check(
            self.bin_dir, env={"LUNARWING_REAL_OPENCODE": str(self.binary_path)}
        )
        self.assertEqual(failures, [])

    def test_wrapper_and_source_rewritten_identically_fails(self) -> None:
        # Run-106 GJ-3(a): the byte-identity reference ships inside the
        # image, so a root RUN rewriting both /usr/local/bin/opencode AND
        # /app/scripts/opencode-exposure-guard.sh identically passed GD-2's
        # byte-identity check. The checkout digest catches this drift while
        # the anchor and checker stay intact; an adversarial Dockerfile can
        # still replace either (run-109 GW-3).
        tampered = WRAPPER.read_bytes() + b"\n# rewritten in-image\n"
        self.wrapper_path.write_bytes(tampered)
        self.wrapper_path.chmod(0o755)
        (self.lane / "scripts" / "opencode-exposure-guard.sh").write_bytes(tampered)
        failures = self.check(self.bin_dir)
        self.assertEqual(len(failures), 1, failures)
        self.assertIn("LUNARWING_WRAPPER_SHA256", failures[0])

    def test_missing_anchor_fails(self) -> None:
        # A dropped ARG declaration or a builder that does not forward the
        # build arg must fail the image build, not silently skip the anchor.
        failures = self.check(self.bin_dir, env={"LUNARWING_WRAPPER_SHA256": ""})
        self.assertEqual(len(failures), 1, failures)
        self.assertIn("missing", failures[0])

    def test_image_filesystem_path_wires_the_check_in(self) -> None:
        # The scenarios above call the function directly (GD-3: the image
        # build is the only place --image-filesystem runs, and it is a
        # protected-pipeline gate) — pin the main() wiring so the check
        # cannot be dropped from the shipped invocation unnoticed.
        source = ASSERT.read_text(encoding="utf-8")
        wiring = (
            "    if args.image_filesystem:\n"
            "        check_image_filesystem(lane)\n"
            "        if is_v2:\n"
            "            check_installed_wrapper(lane)\n"
        )
        self.assertIn(wiring, source)


if __name__ == "__main__":
    unittest.main()
