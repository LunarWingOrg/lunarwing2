#!/usr/bin/env python3
"""Self-test for compile_out_remote_tunnel.py (WI#357 control 8).

Builds synthetic upstream trees for the three pin shapes: predating the
tunnel (v2.0.24), carrying it (v2.0.26 shape), and drifted (module moved,
or a new RemoteTunnel export in use). The guard must pass the first, stub
the second, and fail closed on every drift shape.
"""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent / "compile_out_remote_tunnel.py"

SCAN_ROOTS = ("packages/cli/src", "packages/server/src", "packages/schema/src", "packages/core/src")
MODULE = "packages/cli/src/services/remote-tunnel.ts"

REAL_MODULE = (
    'export * as RemoteTunnel from "./remote-tunnel"\n'
    'import type { OpenTunnelError } from "@opentunnel/client/effect"\n'
    "export const run = 1\nexport const ensure = 1\nexport const hostname = 1\n"
)

STUB_MARK = "OpenTunnel remote access is compiled out"


def write_tree(root: pathlib.Path, *, with_tunnel: bool) -> None:
    for relative in SCAN_ROOTS:
        (root / relative).mkdir(parents=True)
    deps = {"effect": "3.0.0"}
    if with_tunnel:
        deps["@opentunnel/client"] = "0.2.0"
        (root / MODULE).parent.mkdir(parents=True, exist_ok=True)
        (root / MODULE).write_text(REAL_MODULE, encoding="utf-8")
        (root / "packages/cli/src/services/service-config.ts").write_text(
            'import { RemoteTunnel } from "./remote-tunnel"\n'
            "if (value === 'true') yield* RemoteTunnel.ensure()\n",
            encoding="utf-8",
        )
        (root / "packages/cli/src/server-process.ts").write_text(
            'import { RemoteTunnel } from "./services/remote-tunnel"\n'
            "yield* Effect.forkScoped(RemoteTunnel.run({ route, target, onURL }))\n",
            encoding="utf-8",
        )
        (root / "packages/cli/src/commands").mkdir(parents=True, exist_ok=True)
        (root / "packages/cli/src/commands/pair.ts").write_text(
            'import { RemoteTunnel } from "../services/remote-tunnel"\n'
            "const h = yield* RemoteTunnel.hostname(route)\n",
            encoding="utf-8",
        )
    (root / "packages/cli").mkdir(parents=True, exist_ok=True)
    (root / "packages/cli/package.json").write_text(
        json.dumps({"name": "opencode", "dependencies": deps}), encoding="utf-8"
    )


def run_guard(root: pathlib.Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPT), str(root)], capture_output=True, text=True, timeout=60
    )


class CompileOutRemoteTunnelTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name) / "opencode"
        self.root.mkdir()

    def test_pin_without_tunnel_passes_untouched(self) -> None:
        write_tree(self.root, with_tunnel=False)
        result = run_guard(self.root)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("nothing to stub", result.stdout)
        self.assertFalse((self.root / MODULE).exists())

    def test_tunnel_module_is_stubbed(self) -> None:
        write_tree(self.root, with_tunnel=True)
        result = run_guard(self.root)
        self.assertEqual(result.returncode, 0, result.stderr)
        stubbed = (self.root / MODULE).read_text(encoding="utf-8")
        self.assertIn(STUB_MARK, stubbed)
        self.assertNotIn("@opentunnel", stubbed)
        for name in ("run", "ensure", "hostname"):
            self.assertIn(f"export const {name}", stubbed)
        self.assertIn("'ensure', 'hostname', 'run'", result.stdout)

    def test_moved_module_fails_closed(self) -> None:
        write_tree(self.root, with_tunnel=True)
        (self.root / MODULE).unlink()  # upstream renamed the module
        result = run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("moved or renamed", result.stderr)

    def test_new_importer_surface_fails_closed(self) -> None:
        write_tree(self.root, with_tunnel=True)
        with (self.root / "packages/cli/src/server-process.ts").open("a", encoding="utf-8") as f:
            f.write("yield* RemoteTunnel.restart()\n")
        result = run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("restart", result.stderr)

    def test_dependency_without_module_fails_closed(self) -> None:
        write_tree(self.root, with_tunnel=False)
        manifest = json.loads((self.root / "packages/cli/package.json").read_text())
        manifest["dependencies"]["@opentunnel/client"] = "0.2.0"
        (self.root / "packages/cli/package.json").write_text(json.dumps(manifest))
        result = run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("moved or renamed", result.stderr)

    def test_missing_scan_root_fails_closed(self) -> None:
        write_tree(self.root, with_tunnel=False)
        for child in (self.root / "packages/server/src").iterdir():
            child.unlink()
        import shutil

        shutil.rmtree(self.root / "packages/server/src")
        result = run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("layout drift", result.stderr)

    # ── run-79 EI-4: the old scan left these drift shapes passing ──────────

    def test_module_moved_into_core_fails_closed(self) -> None:
        # Old SCAN_ROOTS ignored packages/core/src, so a marker-bearing file
        # there with the module gone audited as "no OpenTunnel surface".
        write_tree(self.root, with_tunnel=False)
        (self.root / "packages/core/src/remote").mkdir(parents=True)
        (self.root / "packages/core/src/remote/tunnel.ts").write_text(
            'import { Client } from "@opentunnel/client"\n'
            "export const ensure = () => Client.layer()\n",
            encoding="utf-8",
        )
        result = run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("moved or renamed", result.stderr)

    def test_tsx_importer_fails_closed(self) -> None:
        write_tree(self.root, with_tunnel=False)
        (self.root / "packages/cli/src/tunnel-panel.tsx").write_text(
            "// settings surface that would expose RemoteTunnel\n", encoding="utf-8"
        )
        result = run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("moved or renamed", result.stderr)

    def test_dynamic_import_leftover_fails_closed(self) -> None:
        # Old OPENTUNNEL_PACKAGE_RE matched only `from "@opentunnel/…"`, so a
        # dynamic import survived stubbing; the leftover check had no test.
        write_tree(self.root, with_tunnel=True)
        (self.root / "packages/cli/src/lazy-tunnel.ts").write_text(
            'const client = await import("@opentunnel/client")\n'
            "export const load = () => client\n",
            encoding="utf-8",
        )
        result = run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("imports remain after stubbing", result.stderr)
        self.assertIn("lazy-tunnel.ts", result.stderr)

    def test_dependency_declared_outside_cli_fails_closed(self) -> None:
        # Old dep collection read only packages/cli/package.json.
        write_tree(self.root, with_tunnel=False)
        (self.root / "packages/core").mkdir(parents=True, exist_ok=True)
        (self.root / "packages/core/package.json").write_text(
            json.dumps({"name": "@opencode/core", "dependencies": {"@opentunnel/client": "0.2.0"}}),
            encoding="utf-8",
        )
        result = run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("moved or renamed", result.stderr)


if __name__ == "__main__":
    unittest.main()
