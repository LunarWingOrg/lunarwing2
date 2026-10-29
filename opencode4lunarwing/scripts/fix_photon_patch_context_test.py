#!/usr/bin/env python3
"""Self-test for fix_photon_patch_context.py (item-28).

Builds synthetic upstream trees carrying the three pin shapes: the pinned
buggy patch (the current v1.18.25 / v2.1.1 upstream), the already-fixed
patch (upstream commit 61845b0), and a drifted patch that no longer
matches either form. The guard must repair the first, refuse the second,
and fail closed on every drift shape.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent / "fix_photon_patch_context.py"
PATCH_REL = "patches/@silvia-odwyer%2Fphoton-node@0.3.4.patch"

# The exact hunk-10 context lines as they live in the pinned upstream patch
# (one stray leading space on each context line).
BUGGY_HUNK_TAIL = (
    "      ;\n",
    "  };\n",
    "  \n",
    "-const path = require('path').join(__dirname, 'photon_rs_bg.wasm');\n",
    "+// Allow opencode's Bun compiled binary to point photon-node at its embedded wasm asset.\n",
    "+const path = globalThis.__OPENCODE_PHOTON_WASM_PATH || require('path').join(__dirname, 'photon_rs_bg.wasm');\n",
    " const bytes = require('fs').readFileSync(path);\n",
)
FIXED_HUNK_TAIL = (
    "     ;\n",
    " };\n",
    " \n",
    "-const path = require('path').join(__dirname, 'photon_rs_bg.wasm');\n",
    "+// Allow opencode's Bun compiled binary to point photon-node at its embedded wasm asset.\n",
    "+const path = globalThis.__OPENCODE_PHOTON_WASM_PATH || require('path').join(__dirname, 'photon_rs_bg.wasm');\n",
    " const bytes = require('fs').readFileSync(path);\n",
)
HUNK_HEADER = "@@ -4509,7 +4510,8 @@ module.exports.__wbindgen_init_externref_table = function() {\n"


def patch_text(hunk_tail: tuple[str, ...]) -> str:
    return HUNK_HEADER + "".join(hunk_tail)


def write_patch(root: pathlib.Path, text: str) -> None:
    p = root / PATCH_REL
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(text, encoding="utf-8")


def run_script(root: pathlib.Path) -> tuple[int, str, str]:
    proc = subprocess.run(
        [sys.executable, str(SCRIPT), str(root)],
        capture_output=True, text=True, check=False,
    )
    return proc.returncode, proc.stdout, proc.stderr


class FixPhotonPatchContextTests(unittest.TestCase):
    def test_repairs_buggy_hunk_in_place(self) -> None:
        """The pinned upstream patch is repaired to the fixed context."""
        with tempfile.TemporaryDirectory() as d:
            root = pathlib.Path(d)
            write_patch(root, patch_text(BUGGY_HUNK_TAIL))
            rc, out, err = run_script(root)
            self.assertEqual(rc, 0, err or out)
            after = (root / PATCH_REL).read_text(encoding="utf-8")
            self.assertEqual(after, patch_text(FIXED_HUNK_TAIL))

    def test_fixed_patch_refuses_to_rerun(self) -> None:
        """Upstream landing 61845b0 makes the in-tree repair redundant."""
        with tempfile.TemporaryDirectory() as d:
            root = pathlib.Path(d)
            write_patch(root, patch_text(FIXED_HUNK_TAIL))
            rc, out, err = run_script(root)
            self.assertNotEqual(rc, 0)
            self.assertIn("already carries the fixed whitespace", err)

    def test_drifted_patch_fails_closed(self) -> None:
        """An unrecognised patch shape fails the build rather than passing."""
        with tempfile.TemporaryDirectory() as d:
            root = pathlib.Path(d)
            drifted = tuple(
                ("XX" + line[2:]) if line[:2] in ("  ", "  \n") else line
                for line in BUGGY_HUNK_TAIL
            )
            write_patch(root, patch_text(drifted))
            rc, out, err = run_script(root)
            self.assertNotEqual(rc, 0)
            self.assertIn("could not locate the buggy hunk-10", err)

    def test_missing_patch_fails_closed(self) -> None:
        """A checkout that lost the patch file fails the build."""
        with tempfile.TemporaryDirectory() as d:
            root = pathlib.Path(d)
            (root / "patches").mkdir()
            rc, out, err = run_script(root)
            self.assertNotEqual(rc, 0)
            self.assertIn("not found", err)


if __name__ == "__main__":
    unittest.main()
