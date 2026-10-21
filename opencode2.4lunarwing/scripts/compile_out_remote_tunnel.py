#!/usr/bin/env python3
"""WI#357 control 8 — compile the OpenTunnel remote-access client out of the
pinned opencode build, fail-closed.

Runs in the image builder stage against the checked-out upstream tree (same
style as prepare_reproducible_build.py, which fails the build on drift):

- Historical pin predates the tunnel (v2.0.24): no module, no references, no
  @opentunnel dependencies — log and pass.
- Tunnel present (v2.0.26+): packages/cli/src/services/remote-tunnel.ts is
  replaced with a stub that fails closed (run/ensure die; hostname reports
  no tunnel so pairing degrades gracefully). The stub mirrors the v2.0.26
  export surface, re-verified against the real v2.1.2 tree
  (importers still use exactly run/ensure/hostname); if importers start
  using anything beyond it, or the module
  moves/renames (references remain but the expected path is gone), the build
  FAILS rather than silently shipping a live tunnel.
- After stubbing, no source file may still reference the @opentunnel
  packages (any quoted @opentunnel/* specifier: static, dynamic, require);
  the runtime stage separately asserts the built binary carries no relay
  markers (exposure_policy_assert.py --binary).

Scanned surface (run-79 EI-4): packages/{cli,server,schema,core}/src,
*.ts and *.tsx, with @opentunnel dependencies collected from every
packages/*/package.json. The runtime-stage binary marker scan remains the
backstop if upstream drifts past even this.

usage: compile_out_remote_tunnel.py [upstream repo root]
"""

from __future__ import annotations

import json
import pathlib
import re
import sys

SCAN_ROOTS = ("packages/cli/src", "packages/server/src", "packages/schema/src", "packages/core/src")
MODULE = pathlib.Path("packages/cli/src/services/remote-tunnel.ts")
CONTENT_MARKERS = ("opentunnel", "RemoteTunnel", "remote-tunnel")
# Any quoted @opentunnel/* package specifier: static `from "..."`, dynamic
# `import("...")`, and `require("...")` alike (run-79 EI-4).
OPENTUNNEL_PACKAGE_RE = re.compile(r"[\"']@opentunnel/[^\"']*[\"']")
IMPORTER_USE_RE = re.compile(r"\bRemoteTunnel\.(\w+)")

# The export surface importers may use; anything new fails the build until
# the stub is taught the new shape (drift is loud, never silent).
STUB_EXPORTS = ("run", "ensure", "hostname")

STUB = '''\
export * as RemoteTunnel from "./remote-tunnel"

// LUNARWING WI#357 — OpenTunnel remote access is compiled out of this worker
// image. LunarWing owns every public exposure path (ic/src/tunnel); workers
// must never self-expose, and the hosted opentunnel.xyz relay has no
// self-host option. Every entry point of the real module fails closed:
// run/ensure die with a named error (they only execute in service mode,
// which this image refuses outright), and hostname reports "no tunnel" so
// the pair handler degrades to its local behavior.
import { Effect } from "effect"

const disabled = Effect.die(
  new Error(
    "OpenTunnel remote access is compiled out of this LunarWing worker image (WI#357): " +
      "workers must never self-expose; use the daemon's tunnel instead",
  ),
)

export const run = (_input: {
  readonly route: string
  readonly target: string
  readonly onURL: (url: string | undefined) => void
}) => disabled

export const ensure = () => disabled

export const hostname = (_route: string) => Effect.succeed(undefined as string | undefined)
'''


def fail(message: str) -> "SystemExit":
    print(f"FATAL: {message}", file=sys.stderr)
    return SystemExit(1)


def scan_sources(root: pathlib.Path) -> dict[pathlib.Path, str]:
    files: dict[pathlib.Path, str] = {}
    for relative in SCAN_ROOTS:
        directory = root / relative
        if not directory.is_dir():
            raise fail(f"expected upstream source dir {relative} is missing — layout drift")
        for path in sorted(directory.rglob("*.ts")) + sorted(directory.rglob("*.tsx")):
            files[path] = path.read_text(encoding="utf-8", errors="replace")
    return files


def declared_tunnel_dependencies(root: pathlib.Path) -> list[str]:
    """@opentunnel deps from EVERY workspace manifest, not just the CLI's
    (run-79 EI-4: a moved module would declare its deps elsewhere)."""
    names: set[str] = set()
    for manifest in sorted((root / "packages").glob("*/package.json")):
        try:
            parsed = json.loads(manifest.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            raise fail(f"{manifest} does not parse — cannot audit OpenTunnel dependencies")
        dependencies = {**parsed.get("dependencies", {}), **parsed.get("devDependencies", {})}
        names.update(name for name in dependencies if "opentunnel" in name)
    if not (root / "packages" / "cli" / "package.json").is_file():
        raise fail("expected packages/cli/package.json is missing — layout drift")
    return sorted(names)


def main() -> None:
    root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()
    module = root / MODULE
    sources = scan_sources(root)

    tunnel_deps = declared_tunnel_dependencies(root)

    referencing = {
        path: text
        for path, text in sources.items()
        if path != module and any(marker in text for marker in CONTENT_MARKERS)
    }

    if not module.is_file():
        if referencing or tunnel_deps:
            names = ", ".join(str(path.relative_to(root)) for path in referencing)
            raise fail(
                "OpenTunnel references exist but services/remote-tunnel.ts is gone "
                f"(deps: {tunnel_deps or 'none'}; files: {names or 'none'}) — upstream "
                "moved or renamed the tunnel module; update this guard before bumping"
            )
        print("compile_out_remote_tunnel: pin has no OpenTunnel surface; nothing to stub")
        return

    used = set()
    for text in referencing.values():
        used.update(IMPORTER_USE_RE.findall(text))
    unknown = sorted(used - set(STUB_EXPORTS))
    if unknown:
        raise fail(
            f"importers use RemoteTunnel.{unknown} beyond the stub's surface "
            f"{list(STUB_EXPORTS)} — teach the stub the new shape before building"
        )

    module.write_text(STUB, encoding="utf-8")
    print(f"compile_out_remote_tunnel: stubbed {MODULE} (importers use: {sorted(used) or 'none'})")

    leftovers = [
        str(path.relative_to(root))
        for path, text in scan_sources(root).items()
        if OPENTUNNEL_PACKAGE_RE.search(text)
    ]
    if leftovers:
        raise fail(
            f"@opentunnel imports remain after stubbing: {leftovers} — "
            "the tunnel client would still be bundled"
        )
    if tunnel_deps:
        print(
            f"compile_out_remote_tunnel: @opentunnel deps remain declared {tunnel_deps} "
            "but nothing imports them; the binary marker assertion proves they are not bundled"
        )


if __name__ == "__main__":
    main()
