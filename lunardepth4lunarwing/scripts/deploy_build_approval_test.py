#!/usr/bin/env python3

import importlib.util
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "deploy_build_approval", Path(__file__).with_name("deploy_build_approval.py"),
)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class DeployBuildApprovalTests(unittest.TestCase):
    def test_absolute_approval_is_scoped_and_restored(self):
        with tempfile.TemporaryDirectory() as temporary:
            source_root = Path(temporary)
            workspace = source_root / "pnpm-workspace.yaml"
            original = f"allowBuilds:\n{MODULE.RELATIVE}  esbuild: true\n"
            workspace.write_text(original, encoding="utf-8")
            MODULE.add(workspace, source_root)
            transformed = workspace.read_text(encoding="utf-8")
            expected = MODULE.deploy_approval(source_root)
            self.assertIn(expected, transformed)
            self.assertIn("file:///", expected)
            self.assertNotIn("dangerouslyAllowAllBuilds", transformed)
            MODULE.remove(workspace, source_root)
            self.assertEqual(workspace.read_text(encoding="utf-8"), original)

    def test_duplicate_or_unreviewed_approval_fails_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            source_root = Path(temporary)
            workspace = source_root / "pnpm-workspace.yaml"
            workspace.write_text(MODULE.RELATIVE, encoding="utf-8")
            MODULE.add(workspace, source_root)
            with self.assertRaisesRegex(ValueError, "already deployed"):
                MODULE.add(workspace, source_root)
            workspace.write_text("allowBuilds: {}\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "approval is absent"):
                MODULE.remove(workspace, source_root)


if __name__ == "__main__":
    unittest.main()
