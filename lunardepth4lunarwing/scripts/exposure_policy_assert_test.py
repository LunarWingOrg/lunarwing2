#!/usr/bin/env python3

import importlib.util
import shutil
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("exposure_policy_assert", Path(__file__).with_name("exposure_policy_assert.py"))
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class ExposurePolicyTests(unittest.TestCase):
    def test_current_lane_passes_source_policy(self):
        root = Path(__file__).parents[1]
        MODULE.source_policy(root)

    def test_unconfined_or_keyed_overlay_fails(self):
        root = Path(__file__).parents[1]
        with tempfile.TemporaryDirectory() as directory:
            clone = Path(directory) / "lane"
            shutil.copytree(root, clone, ignore=shutil.ignore_patterns("node_modules", "*.js", "__pycache__"))
            overlay = clone / "config/lunarwing-lane.patch.yml"
            text = overlay.read_text(encoding="utf-8")
            overlay.write_text(text.replace("mode: workspace-write", "mode: danger-full-access"), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "danger-full-access"):
                MODULE.source_policy(clone)
            overlay.write_text(text + "\nLUNARDEPTH_API_KEY: planted\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "LUNARDEPTH_API_KEY"):
                MODULE.source_policy(clone)


if __name__ == "__main__":
    unittest.main()
