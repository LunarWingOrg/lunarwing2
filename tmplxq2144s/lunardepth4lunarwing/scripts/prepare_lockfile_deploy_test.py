#!/usr/bin/env python3

import importlib.util
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "prepare_lockfile_deploy", Path(__file__).with_name("prepare_lockfile_deploy.py"),
)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class PrepareLockfileDeployTests(unittest.TestCase):
    def test_replaces_the_complete_legacy_deploy_block(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "build.ts"
            path.write_text(f"before\n{MODULE.LEGACY_DEPLOY}after\n", encoding="utf-8")
            MODULE.prepare(path)
            transformed = path.read_text(encoding="utf-8")
            self.assertIn(MODULE.LOCKFILE_DEPLOY, transformed)
            self.assertNotIn("'--legacy'", transformed)
            self.assertNotIn("auto-install-peers", transformed)
            self.assertIn("'--frozen-lockfile'", transformed)
            self.assertIn("minimumReleaseAgeStrict=true", transformed)
            self.assertIn("inject-workspace-packages=true", transformed)
            with self.assertRaisesRegex(ValueError, "already transformed"):
                MODULE.prepare(path)

    def test_refuses_unreviewed_source_drift(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "build.ts"
            path.write_text("      'deploy', '--legacy', '--prod',\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "deploy block is absent"):
                MODULE.prepare(path)
            self.assertEqual(path.read_text(encoding="utf-8"), "      'deploy', '--legacy', '--prod',\n")


if __name__ == "__main__":
    unittest.main()
