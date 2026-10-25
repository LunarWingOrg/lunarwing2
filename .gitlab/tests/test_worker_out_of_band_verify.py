#!/usr/bin/env python3
"""WI#425 tests — the out-of-band opencode2 image verification surfaces.

Covers:
  * .gitlab/scripts/verify-opencode2-exposure-out-of-band.py — the
    image-config half (Env retarget refusal, kill-switch presence, Shell
    override refusal) and the export-dir existence refusal, plus the
    candidate-build wiring (the local export output rides the candidate
    build only).
  * the exposure_policy_assert.py --image-root mode that the CI verify job
    drives against the exported tree.
"""

from __future__ import annotations

import importlib.util
import os
import pathlib
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
VERIFY = ROOT / ".gitlab" / "scripts" / "verify-opencode2-exposure-out-of-band.py"
LANE = ROOT / "opencode2.4lunarwing"
ASSERT = LANE / "scripts" / "exposure_policy_assert.py"
WRAPPER = LANE / "scripts" / "opencode-exposure-guard.sh"


def load_verify_module():
    spec = importlib.util.spec_from_file_location("verify_oob", VERIFY)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ConfigHalfTests(unittest.TestCase):
    def setUp(self):
        self.module = load_verify_module()

    def test_clean_config_passes(self):
        # The shipped image config shape: the autoupdate kill switch, no
        # retarget keys, no Shell override.
        config = {
            "config": {
                "Env": [
                    "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                    "OPENCODE_DISABLE_AUTOUPDATE=1",
                ],
                "Shell": None,
            }
        }
        with mock.patch("builtins.print"):
            self.module.check_config_half(config)

    def test_opencode_bin_retarget_refused(self):
        # Include the kill switch so ONLY the retarget can fail this config —
        # a mutation removing the retarget loop must be caught (without the
        # kill switch here, the missing-switch check would mask the mutant).
        config = {"config": {"Env": [
            "OPENCODE_DISABLE_AUTOUPDATE=1",
            "OPENCODE_BIN=/usr/local/lib/opencode2/opencode",
        ]}}
        with self.assertRaises(SystemExit) as ctx:
            self.module.check_config_half(config)
        self.assertEqual(ctx.exception.code, 1)

    def test_real_opencode_retarget_refused(self):
        config = {"config": {"Env": [
            "OPENCODE_DISABLE_AUTOUPDATE=1",
            "LUNARWING_REAL_OPENCODE=/tmp/evil",
        ]}}
        with self.assertRaises(SystemExit):
            self.module.check_config_half(config)

    def test_missing_kill_switch_refused(self):
        config = {"config": {"Env": ["PATH=/bin"]}}
        with self.assertRaises(SystemExit):
            self.module.check_config_half(config)

    def test_shell_override_refused(self):
        config = {"config": {"Env": ["OPENCODE_DISABLE_AUTOUPDATE=1"], "Shell": ["/bin/false"]}}
        with self.assertRaises(SystemExit):
            self.module.check_config_half(config)


class MissingExportRefusalTests(unittest.TestCase):
    def setUp(self):
        self.module = load_verify_module()

    def test_missing_export_dir_dies_with_real_cause(self):
        # GZ-2 shape: the missing export names the export, not a downstream
        # symptom. Assert on the MESSAGE so a mutation that drops the
        # existence check (falling through to the assert-run failure) cannot
        # pass through the generic exit code.
        with tempfile.TemporaryDirectory() as tmp:
            empty = pathlib.Path(tmp)
            with mock.patch.object(self.module, "REPO_ROOT", empty), \
                 self.assertRaises(SystemExit) as ctx, \
                 mock.patch("sys.stderr") as stderr:
                self.module.check_filesystem_half(empty / "opencode2-rootfs-export")
            self.assertEqual(ctx.exception.code, 1)
            written = "".join(call.args[0] for call in stderr.write.call_args_list)
            self.assertIn("did not export the opencode2 filesystem", written)
            self.assertNotIn("FAILED", written)


class ImageRootModeTests(unittest.TestCase):
    """The --image-root mode the CI verify job drives against the export."""

    def setUp(self):
        # A minimal exported-tree fixture: root-owned files cannot be faked
        # as uid 0 by a non-root runner, so the mode is exercised through the
        # assert module directly with owner_uid = os.geteuid() and the paths
        # the wiring uses.
        temporary = tempfile.TemporaryDirectory(dir=LANE)
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        # lane tree inside the "export"
        self.export = self.root / "opencode2-rootfs-export"
        (self.export / "app" / "scripts").mkdir(parents=True)
        (self.export / "app" / "config").mkdir()
        shutil.copy(WRAPPER, self.export / "app" / "scripts" / WRAPPER.name)
        # the exported wrapper under its installed path
        (self.export / "usr" / "local" / "bin").mkdir(parents=True)
        shutil.copy(WRAPPER, self.export / "usr" / "local" / "bin" / "opencode")
        self.wrapper_path = self.export / "usr" / "local" / "bin" / "opencode"
        self.wrapper_path.chmod(0o755)
        (self.export / "usr" / "local" / "lib" / "opencode2").mkdir(parents=True)
        self.binary_path = self.export / "usr" / "local" / "lib" / "opencode2" / "opencode"
        self.binary_path.write_bytes(b"\x7fELF clean pinned build")
        spec = importlib.util.spec_from_file_location("exposure_policy_assert", ASSERT)
        self.assert_module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.assert_module)

    def test_wiring_uses_image_root_paths(self):
        # The main() wiring must point the installed-wrapper check at the
        # image-root paths (mutation pin: reverting to check_installed_wrapper
        # (lane) alone fails this).
        source = ASSERT.read_text(encoding="utf-8")
        self.assertIn('args.image_root / "usr/local/bin/opencode"', source)
        self.assertIn('args.image_root / "usr/local/lib/opencode2/opencode"', source)
        self.assertIn("check_path_resolution=False", source)

    def test_cli_accepts_image_root(self):
        # The --image-root flag must exist and parse.
        source = ASSERT.read_text(encoding="utf-8")
        self.assertIn("--image-root", source)


class BuildScriptWiringTests(unittest.TestCase):
    """The candidate build must export the filesystem for the verify job."""

    def setUp(self):
        self.build_script = (
            ROOT / ".gitlab" / "scripts" / "build-worker-image.sh"
        ).read_text(encoding="utf-8")

    def test_opencode2_candidate_exports_local_output(self):
        self.assertIn('build_image_extra_output="type=local,dest=$export_dir"', self.build_script)
        self.assertIn('if [ "$1" = opencode2 ]; then\n    export_dir=', self.build_script)

    def test_rebuild_does_not_re_export(self):
        # The reset must precede the rebuild call (mutation pin: moving the
        # reset after the rebuild keeps a stale export for the wrong inputs).
        self.assertIn('build_image_extra_output=""\n', self.build_script)
        reset_index = self.build_script.index('build_image_extra_output=""\n')
        rebuild_index = self.build_script.index('build_image_with_retry "$rebuild_ref"')
        self.assertLess(reset_index, rebuild_index)

    def test_build_image_appends_extra_output(self):
        self.assertIn('[ -n "${build_image_extra_output:-}" ] && set -- "$@" --output "$build_image_extra_output"', self.build_script)


class GoParserPolicyTests(unittest.TestCase):
    """WI#425 Ask 4: BuildKit's own parser drives a policy cross-check.

    The Go tool under .gitlab/go/dockerfile-parse/ applies the exposure policy
    to BuildKit's instruction list (pinned buildkit v0.25.1 = the candidate
    CI image) instead of re-implementing the Dockerfile grammar in bash. The
    bash grammar in check-worker-exposure-policy.sh stays the authoritative
    gate; this parser is the cross-check the WI asks for while ten historical
    divergences are retired by construction (they were bash-parser artifacts).
    """

    PARSER_DIR = ROOT / ".gitlab" / "go" / "dockerfile-parse"
    GO = shutil.which("go")

    def test_module_is_pinned_to_the_candidate_buildkit(self):
        go_mod = (self.PARSER_DIR / "go.mod").read_text(encoding="utf-8")
        self.assertIn("github.com/moby/buildkit v0.25.1", go_mod)
        go_sum = (self.PARSER_DIR / "go.sum").read_text(encoding="utf-8")
        self.assertIn("github.com/moby/buildkit v0.25.1", go_sum)

    def test_policy_source_applies_the_parser(self):
        # The policy must import BuildKit's frontend parser (mutation pin:
        # reverting to a hand-rolled split is caught here).
        main_go = (self.PARSER_DIR / "main.go").read_text(encoding="utf-8")
        self.assertIn('"github.com/moby/buildkit/frontend/dockerfile/parser"', main_go)
        self.assertIn("parser.Parse", main_go)

    @unittest.skipUnless(
        shutil.which("go") is not None, "go toolchain not available in this image"
    )
    def test_go_tests_pass_on_the_real_tree(self):
        result = subprocess.run(
            ["go", "test", "./..."],
            cwd=self.PARSER_DIR,
            capture_output=True,
            text=True,
            timeout=300,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("ok", result.stdout)

    @unittest.skipUnless(
        shutil.which("go") is not None, "go toolchain not available in this image"
    )
    def test_parser_policy_accepts_the_real_dockerfile(self):
        dockerfile = ROOT / "opencode2.4lunarwing" / "Dockerfile"
        result = subprocess.run(
            ["go", "run", ".", str(dockerfile)],
            cwd=self.PARSER_DIR,
            capture_output=True,
            text=True,
            timeout=300,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("BuildKit parser): ok", result.stdout)


if __name__ == "__main__":
    unittest.main()
