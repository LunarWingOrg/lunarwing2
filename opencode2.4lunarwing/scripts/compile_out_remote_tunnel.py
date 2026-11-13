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
  export surface, re-verified against the real v2.2.7 tree
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

Second job, same stage and same fail-closed style (WI#609, !2398 ABE-2): the
anchor for the wrapper's feature-flag pin. opencode v2.2.7 ships Console event
watches, a remote steering channel, behind the flag `console_watches`, and the
lane's wrapper starts every opencode with OPENCODE_FLAGS=-console_watches.
That pin is only as good as the pinned upstream source: an unknown entry in
OPENCODE_FLAGS is ignored without a word, and upstream ships a feature by
deleting its flag. The anchor is a TEXT TRIPWIRE, necessary and not
sufficient: it fails the build when the reviewed text no longer holds — the
whole-file digests of the two flag files, the two-statement watch gate in the
Console plugin, and the counts of files naming the watch wiring — but it does
not prove that the pin still holds. The per-bump human re-read of the flag
code (bump checklist item 7) remains the decision.

A pin that predates the flags has none of these files and no reference to
them: log and pass, as for the tunnel.

usage: compile_out_remote_tunnel.py [upstream repo root]
"""

from __future__ import annotations

import hashlib
import json
import os
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


# ── WI#609 ABE-2: anchor for the wrapper's OPENCODE_FLAGS pin ───────────────
FLAG_REGISTRY = pathlib.Path("packages/schema/src/config/flag.ts")
FLAG_RESOLVER = pathlib.Path("packages/core/src/flag.ts")
CONSOLE_PLUGIN = pathlib.Path("packages/core/src/plugin/provider/opencode.ts")
# Any of these anywhere in the checkout means the pin has a flag or Console
# watch surface, so the three files above must be where and what we expect.
FLAG_SURFACE_MARKERS = ("OPENCODE_FLAGS", "console_watch")
PINNED_OFF_FLAG = "console_watches"
# The registry as reviewed at the v2.2.7 bump (docs/ops/WORKER-CONTAINERS.md).
# Only PINNED_OFF_FLAG is forced off by the wrapper; the rest are off by
# default. Change this list only together with a decision in the bump entry.
REVIEWED_FLAGS = (
    "artifacts",
    "background_compaction",
    "codemode_vm",
    "console_watches",
    "media_blob",
    "schedules",
    "todo",
)
REGISTRY_BLOCK_RE = re.compile(r"export const registry = \{(.*?)\n\} as const", re.S)
REGISTRY_KEY_RE = re.compile(r"^  ([A-Za-z_][A-Za-z0-9_]*):", re.M)
# THIS ANCHOR IS A TEXT TRIPWIRE, NECESSARY AND NOT SUFFICIENT. It fails the
# build when the pinned source no longer holds the exact text this review
# read: the whole-file digests of the two flag files below, the
# two-statement watch gate, and the counts of files that name the watch
# wiring. It does not prove the pin holds; the per-bump human re-read of the
# flag code (bump checklist item 7) remains the decision. Success means
# "anchor text unchanged", nothing more.
FLAG_FILE_DIGESTS = {
    "packages/core/src/flag.ts": "0656ecfb6ef72ef3947eb64e4d82cc8cb2b891e424181df4f6bdf1bf894e395d",
    "packages/schema/src/config/flag.ts": "331addb3e50e742c4059b6051474e64cbe927820424a6a1c61afd65564ba530e",
}
# The gate in the Console plugin: the first line computes `watching` from the
# flag; the second line is its only use.
WATCH_GATE_LINES = (
    'const watching = allowed && next.remote?.watches === true && (yield* flags.enabled("console_watches"))',
    "yield* syncWatches(watching ? next.connection : undefined)",
)
# Counts of scanned files that name the watch wiring at v2.2.7: the plugin
# definition plus its one consumer name `OpencodeWatches`, and no scanned
# file uses the `Flag.Overrides` access form (the resolver's own
# "@opencode/Flag/Overrides" reference string is pinned by its digest). A new
# watches starter or overrides provider defeats the wrapper's pin, so any
# count drift fails the build.
WATCH_FILE_COUNTS = {"OpencodeWatches": 2, "Flag.Overrides": 0}


def fail(message: str) -> "SystemExit":
    print(f"FATAL: {message}", file=sys.stderr)
    return SystemExit(1)


def _flag_surface_files(root: pathlib.Path) -> list[pathlib.Path]:
    """Every text-ish file in the checkout, skipping dependency trees. The
    early-pass decision needs the whole tree: a Console plugin moved out of
    packages/*/src must not hide a live watch surface."""
    paths: list[pathlib.Path] = []
    for directory, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(name for name in dirnames if name not in ("node_modules", ".git"))
        for filename in filenames:
            if filename.endswith((".ts", ".tsx", ".js", ".mjs", ".cjs", ".json")):
                paths.append(pathlib.Path(directory) / filename)
    return paths


def _registry_diagnosis(path: pathlib.Path) -> str:
    """A human-readable hint about WHY the registry differs, for the bump
    entry. It is a hint, not a verdict: the digest mismatch above is the
    failure."""
    try:
        text = path.read_text(encoding="utf-8")
    except OSError:
        return "the flag registry could not be read"
    block = REGISTRY_BLOCK_RE.search(text)
    if block is None:
        return "the flag registry no longer declares 'export const registry = {…} as const'"
    registry = sorted(REGISTRY_KEY_RE.findall(block.group(1)))
    added = sorted(set(registry) - set(REVIEWED_FLAGS))
    removed = sorted(set(REVIEWED_FLAGS) - set(registry))
    if PINNED_OFF_FLAG in removed:
        return (
            f"registry keys: new: {added or 'none'}, gone: {removed} — "
            "this pin may predate the flag, or upstream may have shipped the feature; "
            "decide which in the bump entry"
        )
    return f"registry keys: new: {added or 'none'}, gone: {removed or 'none'}"


def check_flag_pin_anchor(root: pathlib.Path, sources: dict[pathlib.Path, str]) -> None:
    """Tripwire for the wrapper's OPENCODE_FLAGS pin (see the module docstring):
    fail the build when the reviewed text of the flag files, the watch gate or
    the name counts no longer holds."""
    files = {name: root / name for name in (FLAG_REGISTRY, FLAG_RESOLVER, CONSOLE_PLUGIN)}
    surface = sorted(
        str(path.relative_to(root))
        for path in _flag_surface_files(root)
        if any(
            marker in path.read_text(encoding="utf-8", errors="replace")
            for marker in FLAG_SURFACE_MARKERS
        )
    )
    if not surface and not any(path.is_file() for path in files.values()):
        print("compile_out_remote_tunnel: pin has no feature-flag surface; no flag pin to anchor")
        return
    missing = [str(name) for name, path in files.items() if not path.is_file()]
    if missing:
        raise fail(
            f"feature-flag or Console watch code exists ({surface[:5] or 'none'}) but {missing} "
            "is gone — upstream moved the flag registry, the resolver or the Console plugin; "
            "the wrapper's OPENCODE_FLAGS pin is unverified, update this anchor before bumping"
        )

    for relative, expected in FLAG_FILE_DIGESTS.items():
        actual = hashlib.sha256((root / relative).read_bytes()).hexdigest()
        if actual != expected:
            raise fail(
                f"{relative} differs from the reviewed v2.2.7 text — the wrapper's "
                "OPENCODE_FLAGS pin is unverified for this source; re-read the flag code and "
                f"update this anchor in the same commit (checklist item 7). "
                f"{_registry_diagnosis(files[FLAG_REGISTRY])}"
            )

    plugin_lines = [
        line.strip()
        for line in files[CONSOLE_PLUGIN].read_text(encoding="utf-8").splitlines()
    ]
    gated_calls = [line for line in plugin_lines if "syncWatches(watching" in line]
    all_calls = [line for line in plugin_lines if "syncWatches(" in line]
    if (
        WATCH_GATE_LINES not in list(zip(plugin_lines, plugin_lines[1:]))
        or len(gated_calls) != 1
        or len(all_calls) != 1
    ):
        raise fail(
            f"{CONSOLE_PLUGIN} no longer starts the watches on flags.enabled(\"{PINNED_OFF_FLAG}\") "
            "as its only call site — the wrapper's pin may not gate the Console event watches "
            "any more (WI#609)"
        )

    drifted_counts = {
        name: sum(1 for text in sources.values() if name in text)
        for name, expected in WATCH_FILE_COUNTS.items()
        if sum(1 for text in sources.values() if name in text) != expected
    }
    if drifted_counts:
        raise fail(
            "the files that name the watch wiring changed — "
            + "; ".join(
                f"{name}: {count} file(s), reviewed {WATCH_FILE_COUNTS[name]}"
                for name, count in drifted_counts.items()
            )
            + " — a new watches starter or overrides provider defeats the wrapper's pin (WI#609)"
        )

    print("compile_out_remote_tunnel: flag pin anchor text unchanged")


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

    check_flag_pin_anchor(root, sources)

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
