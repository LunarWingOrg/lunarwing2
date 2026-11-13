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
import stat
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

    def test_console_watches_opt_in_in_config_fails(self) -> None:
        # WI#609 AAV-1: either key turns the Console event watches flag on.
        config_path = self.lane / "config" / "opencode2.json"
        base = json.loads(config_path.read_text())
        for experimental in (
            {"console_watches": True},
            {"all_flags": True},
            {"console_watches": "true"},
            {"all_flags": None},
            {"todo": False, "console_watches": True},
        ):
            with self.subTest(experimental=experimental):
                config_path.write_text(json.dumps({**base, "experimental": experimental}))
                result = run_assert("--lane", str(self.lane))
                self.assertEqual(result.returncode, 1)
                self.assertIn("must not set experimental flags", result.stderr)

    def test_unrelated_or_explicitly_off_experimental_settings_pass(self) -> None:
        config_path = self.lane / "config" / "opencode2.json"
        base = json.loads(config_path.read_text())
        for experimental in (
            {},
            {"console_watches": False, "all_flags": False},
            {"todo": True},
        ):
            with self.subTest(experimental=experimental):
                config_path.write_text(json.dumps({**base, "experimental": experimental}))
                result = run_assert("--lane", str(self.lane))
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("leaves the Console event watches flag off", result.stdout)

    def test_wrapper_without_a_live_flag_pin_fails(self) -> None:
        # WI#609 AAV-1: the pin has to be a statement, and the last word on
        # OPENCODE_FLAGS before the real binary starts.
        wrapper = self.lane / "scripts" / WRAPPER.name
        text = wrapper.read_text(encoding="utf-8")
        pin = 'export OPENCODE_FLAGS="-console_watches"\n'
        launch = 'exec "$REAL_OPENCODE" "$@"'
        self.assertEqual(text.count(pin + launch), 1)
        for label, replacement in (
            ("removed", launch),
            ("only a comment", "# " + pin + launch),
            ("after the exec", launch + "\n" + pin),
            ("unset again", pin + "unset OPENCODE_FLAGS\n" + launch),
            ("overwritten", pin + 'OPENCODE_FLAGS="all"\n' + launch),
            ("a different flag", 'export OPENCODE_FLAGS="-schedules"\n' + launch),
            ("turned on", 'export OPENCODE_FLAGS="console_watches"\n' + launch),
        ):
            with self.subTest(label=label):
                wrapper.write_text(text.replace(pin + launch, replacement), encoding="utf-8")
                result = run_assert("--lane", str(self.lane))
                self.assertEqual(result.returncode, 1)
                self.assertIn("as the last OPENCODE_FLAGS statement", result.stderr)

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
        # WI#427: v2.0.32+ (verified still true at v2.1.2) can enable remote access via pair --remote --yes
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

    def test_self_upgrade_refused(self) -> None:
        # !1735 review KG-1 / !1699 JE-2: `opencode upgrade --method curl`
        # (alias `update`) downloads and runs https://opencode.ai/v2/install,
        # which puts an unmodified, tunnel-capable release at
        # $HOME/.opencode/bin/opencode — outside the wrapper and the
        # compile-out guard. Refuse the subcommand and its alias.
        for form in (
            ("upgrade",),
            ("update",),
            ("upgrade", "--method", "curl"),
            ("--print-logs", "update", "-m", "curl", "2.0.40"),
        ):
            with self.subTest(form=form):
                result = self.run_wrapper(*form)
                self.assertEqual(result.returncode, 77, (form, result.stderr))
                self.assertIn("self-upgrade", result.stderr)

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

    def test_console_watches_flag_is_forced_off_for_the_real_binary(self) -> None:
        # WI#609 AAV-1: whatever OPENCODE_FLAGS the caller holds, the real
        # binary starts with exactly "-console_watches" and its argv intact.
        with tempfile.TemporaryDirectory() as temporary:
            fake = pathlib.Path(temporary) / "opencode"
            fake.write_text(
                "#!/bin/sh\n"
                "printf 'flags=%s\\n' \"${OPENCODE_FLAGS-<unset>}\"\n"
                "printf 'arg=%s\\n' \"$@\"\n",
                encoding="utf-8",
            )
            fake.chmod(0o755)
            for inherited in (None, "", "all", "console_watches", "schedules,console_watches",
                              "-console_watches,console_watches"):
                for form in (("serve", "--port", "4096", "--hostname", "127.0.0.1"), ("run", "x")):
                    with self.subTest(inherited=inherited, form=form):
                        env = dict(os.environ)
                        env["LUNARWING_REAL_OPENCODE"] = str(fake)
                        env.pop("OPENCODE_FLAGS", None)
                        if inherited is not None:
                            env["OPENCODE_FLAGS"] = inherited
                        result = subprocess.run(
                            ["sh", str(WRAPPER), *form],
                            capture_output=True, text=True, env=env, timeout=60,
                        )
                        self.assertEqual(result.returncode, 0, result.stderr)
                        self.assertEqual(
                            result.stdout.splitlines(),
                            ["flags=-console_watches", *(f"arg={arg}" for arg in form)],
                        )

    def test_refusals_come_before_the_real_binary_and_its_environment(self) -> None:
        # The pin sits right before the exec, so a refused command still
        # never reaches the real binary.
        with tempfile.TemporaryDirectory() as temporary:
            marker = pathlib.Path(temporary) / "ran"
            fake = pathlib.Path(temporary) / "opencode"
            fake.write_text(f"#!/bin/sh\n: > '{marker}'\n", encoding="utf-8")
            fake.chmod(0o755)
            env = dict(os.environ)
            env["LUNARWING_REAL_OPENCODE"] = str(fake)
            result = subprocess.run(
                ["sh", str(WRAPPER), "pair", "--remote", "--yes"],
                capture_output=True, text=True, env=env, timeout=60,
            )
            self.assertEqual(result.returncode, 77)
            self.assertFalse(marker.exists())


class ImageFilesystemDependencyTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.lane = write_lane(self.root)
        self.dependencies = self.lane / "node_modules"
        self.package = self.dependencies / "@opencode" / "client"
        self.package.mkdir(parents=True)
        (self.package / "index.js").write_text("module.exports = {};\n", encoding="utf-8")
        for name in ("health_server.py", "agent_comm_protocol.json"):
            (self.lane / name).write_text("{}\n", encoding="utf-8")
        (self.lane / "entrypoint-split.sh").write_text("# split bridge\n", encoding="utf-8")
        for path in (
            self.lane / "entrypoint.sh",
            self.lane / "entrypoint-split.sh",
            self.lane / "health_server.py",
            self.lane / "agent_comm_protocol.json",
            self.lane / "scripts",
            *(self.lane / "scripts").rglob("*"),
            self.lane / "config",
            *(self.lane / "config").rglob("*"),
        ):
            path.chmod(0o555 if path.is_dir() else 0o444)
        spec = importlib.util.spec_from_file_location("exposure_policy_assert", ASSERT)
        if spec is None or spec.loader is None:  # pragma: no cover - import shape
            raise AssertionError("cannot load exposure_policy_assert")
        self.assert_module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.assert_module)

    def check(
        self,
        *,
        foreign: pathlib.Path | None = None,
        metadata: dict[str, dict[str, int]] | None = None,
    ) -> list[str]:
        self.assert_module.failures.clear()
        real_lstat = pathlib.Path.lstat

        def root_owned_lstat(path: pathlib.Path):
            fields = list(real_lstat(path))
            fields[4] = 10003 if path == foreign else 0
            fields[5] = 10003 if path == foreign else 0
            return os.stat_result(fields)

        with mock.patch.object(pathlib.Path, "lstat", root_owned_lstat):
            self.assert_module.check_image_filesystem(
                self.lane,
                image_root=self.root if metadata is not None else None,
                image_metadata=metadata,
            )
        return list(self.assert_module.failures)

    def tar_metadata(self) -> dict[str, dict[str, int]]:
        records = {}
        for path in (self.lane, *self.lane.rglob("*")):
            records[path.relative_to(self.root).as_posix()] = {
                "mode": path.lstat().st_mode,
                "uid": 0,
                "gid": 0,
            }
        return records

    def test_dependency_tree_passes(self) -> None:
        self.assertEqual(self.check(), [])

    def test_setgid_dependency_directory_fails(self) -> None:
        self.dependencies.chmod(0o2755)
        failures = self.check()
        self.assertTrue(
            any("privilege or write bits 2755" in failure for failure in failures),
            failures,
        )

    def test_dependency_privilege_and_write_bits_fail(self) -> None:
        target = self.package / "index.js"
        for mode in (0o4755, 0o775, 0o757):
            with self.subTest(mode=oct(mode)):
                target.chmod(mode)
                failures = self.check()
                self.assertTrue(
                    any(f"privilege or write bits {mode:o}" in failure for failure in failures),
                    failures,
                )

    def test_non_root_dependency_fails(self) -> None:
        failures = self.check(foreign=self.package)
        self.assertTrue(any("owned by root:root" in failure for failure in failures), failures)

    def test_dependency_symlink_fails(self) -> None:
        (self.dependencies / "client-link").symlink_to(self.package / "index.js")
        failures = self.check()
        self.assertTrue(
            any("regular file or directory" in failure for failure in failures), failures,
        )

    def test_missing_dependency_tree_fails(self) -> None:
        shutil.rmtree(self.dependencies)
        failures = self.check()
        self.assertTrue(any("missing or unreadable" in failure for failure in failures), failures)

    def test_tar_metadata_does_not_require_dependency_materialization(self) -> None:
        metadata = self.tar_metadata()
        shutil.rmtree(self.dependencies)
        self.assertEqual(self.check(metadata=metadata), [])

    def test_tar_metadata_bad_dependency_mode_fails(self) -> None:
        metadata = self.tar_metadata()
        key = self.dependencies.relative_to(self.root).as_posix()
        metadata[key]["mode"] = stat.S_IFDIR | 0o2755
        shutil.rmtree(self.dependencies)
        failures = self.check(metadata=metadata)
        self.assertTrue(
            any("privilege or write bits 2755" in failure for failure in failures), failures,
        )


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
        # KG-3: the image env carries the autoupdate kill switch; default the
        # fixture to the shipped shape so existing tests don't trip it.
        environ = {
            "PATH": os.pathsep.join(str(p) for p in path),
            "LUNARWING_WRAPPER_SHA256": hashlib.sha256(WRAPPER.read_bytes()).hexdigest(),
            "OPENCODE_DISABLE_AUTOUPDATE": "1",
        }
        if env:
            environ.update(env)
        with mock.patch.dict(os.environ, environ):
            self.assert_module.check_installed_wrapper(
                self.lane,
                self.wrapper_path,
                self.binary_path,
                owner_uid=os.geteuid(),
                parent_root=self.root,
            )
        return list(self.assert_module.failures)

    def altered_owner(self, target: pathlib.Path):
        real_lstat = pathlib.Path.lstat

        def lstat(path: pathlib.Path):
            metadata = real_lstat(path)
            if path != target:
                return metadata
            fields = list(metadata)
            fields[4] = os.geteuid() + 1
            return os.stat_result(fields)

        return mock.patch.object(pathlib.Path, "lstat", lstat)

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

    def test_missing_autoupdate_kill_switch_fails(self) -> None:
        # !1735 review KG-3 / !1699 JE-3: the updater ignores the lane
        # config's "update": "disable" pin (readPolicy reads only the global
        # config dir, never OPENCODE_CONFIG), so the image env must carry
        # the kill switch the updater actually honours. Without it the
        # interactive TUI's background check GETs opencode.ai/update/api/...
        # An empty value and an unset key both trip the != "1" guard.
        for missing in ("",):
            with self.subTest(missing=missing):
                failures = self.check(
                    self.bin_dir, env={"OPENCODE_DISABLE_AUTOUPDATE": missing}
                )
                self.assertEqual(len(failures), 1, failures)
                self.assertIn("OPENCODE_DISABLE_AUTOUPDATE", failures[0])

    def test_opencode_bin_env_override_fails_and_scans_the_target(self) -> None:
        # WI#425 GX-2: the entrypoint defaults OPENCODE_BIN to the wrapper, so
        # an image ENV retargeting it at the real binary makes the bridge
        # spawn that directly — past every argv scan the wrapper owns. The
        # value at the override target is marker-scanned too, so the report
        # says what sits there.
        alt = self.root / "altbin" / "opencode.real"
        alt.parent.mkdir()
        alt.write_bytes(b"\x7fELF tampered build embedding https://opentunnel.xyz relay")
        failures = self.check(self.bin_dir, env={"OPENCODE_BIN": str(alt)})
        self.assertTrue(any("OPENCODE_BIN" in f for f in failures), failures)
        self.assertTrue(any("opentunnel" in f for f in failures), failures)

    def test_opencode_bin_env_override_at_the_wrapper_path_passes(self) -> None:
        failures = self.check(
            self.bin_dir, env={"OPENCODE_BIN": str(self.wrapper_path)}
        )
        self.assertEqual(failures, [])

    def test_owner_only_executable_wrapper_fails(self) -> None:
        # WI#425 GX-3: the exec-bit check used to test owner-exec only, so a
        # root-owned 0744 wrapper passed the build gate while the runtime
        # agent uid (10003) could not execute it — a bricked lane that also
        # fell back to whatever PATH resolved instead. All three execute
        # bits are now required.
        self.wrapper_path.chmod(0o744)
        failures = self.check(self.bin_dir)
        self.assertEqual(len(failures), 1, failures)
        self.assertIn("GX-3", failures[0])

    def test_any_missing_execute_bit_fails(self) -> None:
        # WI#425 GX-3: every missing exec bit must fail — the old
        # S_IXUSR-only check passed 0o744 (owner-exec only), which bricks the
        # runtime agent uid; the other forms pin that group-only or
        # other-only exec cannot sneak through either.
        for mode in (0o744, 0o745, 0o754, 0o655, 0o755 & ~0o111):
            with self.subTest(mode=oct(mode)):
                self.wrapper_path.chmod(mode)
                failures = self.check(self.bin_dir)
                self.assertEqual(len(failures), 1, (oct(mode), failures))
                self.assertIn("GX-3", failures[0])
                self.wrapper_path.chmod(0o755)

    def test_non_executable_wrapper_keeps_its_own_reason(self) -> None:
        # Run-106 GJ-3(c) regression: a 0644 wrapper must keep the executable
        # reason (now the GX-3 text), never fall through to the misleading
        # "earlier PATH dir shadows" message.
        self.wrapper_path.chmod(0o644)
        failures = self.check(self.bin_dir)
        self.assertEqual(len(failures), 1, failures)
        self.assertIn("GX-3", failures[0])
        self.assertNotIn("shadows", failures[0])

    def test_real_binary_owner_and_mode_verified(self) -> None:
        # WI#425 GX-2: the real binary behind the wrapper must be owned by
        # the same uid and not group/other-writable, or a post-scan RUN (or
        # a root-capable agent path) could swap in a tunnel-bearing build.
        # A mode change alone must fail; the marker scan is not enough.
        self.binary_path.chmod(0o766)
        failures = self.check(self.bin_dir)
        self.assertTrue(any("real binary" in f and "other-writable" in f for f in failures), failures)

    def test_real_binary_group_write_and_privilege_bits_fail(self) -> None:
        for mode in (0o775, 0o4755, 0o2755):
            with self.subTest(mode=oct(mode)):
                self.binary_path.chmod(mode)
                failures = self.check(self.bin_dir)
                self.assertTrue(
                    any("real binary" in failure for failure in failures), failures,
                )
                self.binary_path.chmod(0o755)

    def test_wrapper_privilege_bits_fail(self) -> None:
        for mode in (0o4755, 0o2755):
            with self.subTest(mode=oct(mode)):
                self.wrapper_path.chmod(mode)
                failures = self.check(self.bin_dir)
                self.assertTrue(any("setuid/setgid" in failure for failure in failures), failures)
                self.wrapper_path.chmod(0o755)

    def test_group_writable_parent_fails(self) -> None:
        self.bin_dir.chmod(0o775)
        failures = self.check(self.bin_dir)
        self.assertTrue(
            any("parent" in failure and str(self.bin_dir) in failure for failure in failures),
            failures,
        )
        self.bin_dir.chmod(0o755)

    def test_special_real_binary_is_refused_without_reading_it(self) -> None:
        self.binary_path.unlink()
        os.mkfifo(self.binary_path)
        with mock.patch.object(self.assert_module, "check_binary") as read_binary:
            failures = self.check(self.bin_dir)
        read_binary.assert_not_called()
        self.assertTrue(any("regular file" in failure for failure in failures), failures)

    def test_real_binary_wrong_owner_fails(self) -> None:
        with self.altered_owner(self.binary_path):
            failures = self.check(self.bin_dir)
        self.assertTrue(
            any("real binary" in failure and "owned" in failure for failure in failures),
            failures,
        )

    def test_writable_parent_of_wrapper_fails(self) -> None:
        self.bin_dir.chmod(0o777)
        failures = self.check(self.bin_dir)
        self.assertTrue(any("parent" in f and str(self.bin_dir) in f for f in failures), failures)
        self.bin_dir.chmod(0o755)

    def test_writable_parent_of_real_binary_fails(self) -> None:
        self.binary_path.parent.chmod(0o777)
        failures = self.check(self.bin_dir)
        self.assertTrue(
            any(str(self.binary_path.parent) in failure for failure in failures),
            failures,
        )

    def test_writable_grandparent_fails(self) -> None:
        self.root.chmod(0o777)
        failures = self.check(self.bin_dir)
        self.assertTrue(any(str(self.root) in failure for failure in failures), failures)
        self.root.chmod(0o755)

    def test_sticky_parent_still_requires_a_trusted_owner(self) -> None:
        self.bin_dir.chmod(0o1777)
        with self.altered_owner(self.bin_dir):
            failures = self.check(self.bin_dir)
        self.assertTrue(
            any(str(self.bin_dir) in failure and "owned" in failure for failure in failures),
            failures,
        )

    def test_sticky_parent_with_trusted_owner_passes(self) -> None:
        self.bin_dir.chmod(0o1777)
        self.assertEqual(self.check(self.bin_dir), [])

    def test_missing_parent_fails_with_real_cause(self) -> None:
        self.binary_path.unlink()
        self.binary_path.parent.rmdir()
        failures = self.check(self.bin_dir)
        self.assertTrue(
            any("parent" in failure and "missing or unreadable" in failure
                for failure in failures),
            failures,
        )

    def test_parent_walk_never_leaves_the_fixture_root(self) -> None:
        real_lstat = pathlib.Path.lstat

        def bounded_lstat(path: pathlib.Path):
            if path != self.root and self.root not in path.parents:
                raise AssertionError(f"parent walk escaped {self.root}: {path}")
            return real_lstat(path)

        with mock.patch.object(pathlib.Path, "lstat", bounded_lstat):
            self.assertEqual(self.check(self.bin_dir), [])

    def test_missing_real_binary_fails_with_real_cause(self) -> None:
        # GZ-2 shape on the new paths: an unreadable/missing real binary
        # names the real cause instead of a mode complaint.
        self.binary_path.unlink()
        failures = self.check(self.bin_dir)
        self.assertTrue(any("real binary" in f and "missing or unreadable" in f for f in failures), failures)

    def test_image_filesystem_path_wires_the_check_in(self) -> None:
        # The scenarios above call the function directly (GD-3: the image
        # build is the only place --image-filesystem runs, and it is a
        # protected-pipeline gate) — pin the main() wiring so the check
        # cannot be dropped from the shipped invocation unnoticed. The
        # --image-root branch (WI#425 out-of-band verification in CI) points
        # the same check at an unpacked BuildKit export, with no live PATH.
        source = ASSERT.read_text(encoding="utf-8")
        wiring = (
            "    if args.image_filesystem:\n"
            "        check_image_filesystem(\n"
            "            lane, image_root=args.image_root, image_metadata=image_metadata\n"
            "        )\n"
            "        if is_v2:\n"
            "            if args.image_root is not None:\n"
            "                check_installed_wrapper(\n"
            "                    lane,\n"
            "                    args.image_root / \"usr/local/bin/opencode\",\n"
            "                    args.image_root / \"usr/local/lib/opencode2/opencode\",\n"
            "                    check_path_resolution=False,\n"
            "                    parent_root=args.image_root,\n"
            "                    environ=image_environment,\n"
            "                    image_root=args.image_root,\n"
            "                    image_metadata=image_metadata,\n"
            "                )\n"
            "            else:\n"
            "                check_installed_wrapper(lane)\n"
        )
        self.assertIn(wiring, source)


if __name__ == "__main__":
    unittest.main()
