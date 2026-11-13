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
deleting its flag. So the build fails unless

- the flag registry is exactly the reviewed list below (a new flag, or a
  removed one, needs a decision in the bump entry first);
- the Console plugin still starts the watches only when
  flags.enabled("console_watches") holds;
- the flag resolver still reads OPENCODE_FLAGS, still turns a flag off for a
  leading "-", and still lets that override win over config and the default.

A pin that predates the flags has none of these files and no reference to
them: log and pass, as for the tunnel.

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


# ── WI#609 ABE-2: anchor for the wrapper's OPENCODE_FLAGS pin ───────────────
FLAG_REGISTRY = pathlib.Path("packages/schema/src/config/flag.ts")
FLAG_RESOLVER = pathlib.Path("packages/core/src/flag.ts")
CONSOLE_PLUGIN = pathlib.Path("packages/core/src/plugin/provider/opencode.ts")
# Any of these in the scanned sources means the pin has a flag or Console
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
# The one expression that starts the watches, on one line.
WATCH_GATE_RE = re.compile(r"^.*\bconst watching\b.*flags\.enabled\(\"console_watches\"\).*$", re.M)
# Whitespace-normalized fragments of the resolver. Together they say: the
# override comes from OPENCODE_FLAGS, "-key" means off, the override is
# consulted before config and before the channel default, and the result is
# returned as resolved. The precedence fragment starts at the assignment, so
# a layer put in front of the override (upstream reserves a place for a
# remote kill switch there) breaks it too.
RESOLVER_ANCHORS = (
    ("OPENCODE_FLAGS is the override source", "defaultValue: () => process.env.OPENCODE_FLAGS"),
    ('a leading "-" turns a flag off', '(item.startsWith("-") ? [item.slice(1), false] : [item, true])'),
    ("the override is looked up per key", "const override = env.get(key) ?? (all ? true : undefined)"),
    (
        "the override wins over config and the default",
        'const [enabled, source]: [boolean, State["source"]] = override !== undefined ? [override, "env"] '
        ': configured !== undefined ? [configured, "config"] : [onByDefault(input.channel), "default"]',
    ),
    ("the resolved value is returned unchanged", "return { enabled, source }"),
)


def fail(message: str) -> "SystemExit":
    print(f"FATAL: {message}", file=sys.stderr)
    return SystemExit(1)


def check_flag_pin_anchor(root: pathlib.Path, sources: dict[pathlib.Path, str]) -> None:
    """Fail the build when the wrapper's -console_watches pin stops meaning
    what the lane relies on (see the module docstring)."""
    files = {name: root / name for name in (FLAG_REGISTRY, FLAG_RESOLVER, CONSOLE_PLUGIN)}
    surface = sorted(
        str(path.relative_to(root))
        for path, text in sources.items()
        if any(marker in text for marker in FLAG_SURFACE_MARKERS)
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

    block = REGISTRY_BLOCK_RE.search(files[FLAG_REGISTRY].read_text(encoding="utf-8"))
    if block is None:
        raise fail(f"{FLAG_REGISTRY} no longer declares 'export const registry = {{…}} as const'")
    registry = sorted(REGISTRY_KEY_RE.findall(block.group(1)))
    added = sorted(set(registry) - set(REVIEWED_FLAGS))
    removed = sorted(set(REVIEWED_FLAGS) - set(registry))
    if PINNED_OFF_FLAG in removed:
        raise fail(
            f"the '{PINNED_OFF_FLAG}' flag is gone from {FLAG_REGISTRY} — upstream ships a "
            "feature by deleting its flag, so the Console event watches may now be always on "
            "and the wrapper's pin does nothing; this bump needs a new decision (WI#609)"
        )
    if added or removed:
        raise fail(
            f"the feature-flag registry changed (new: {added or 'none'}, gone: {removed or 'none'}) — "
            "record a decision for each in the bump entry, then update REVIEWED_FLAGS"
        )

    if WATCH_GATE_RE.search(files[CONSOLE_PLUGIN].read_text(encoding="utf-8")) is None:
        raise fail(
            f"{CONSOLE_PLUGIN} no longer starts the watches on flags.enabled(\"{PINNED_OFF_FLAG}\") — "
            "the wrapper's pin may not gate the Console event watches any more (WI#609)"
        )

    resolver = " ".join(files[FLAG_RESOLVER].read_text(encoding="utf-8").split())
    drifted = [what for what, fragment in RESOLVER_ANCHORS if fragment not in resolver]
    if drifted:
        raise fail(
            f"{FLAG_RESOLVER} changed how flags resolve ({drifted}) — the wrapper's "
            "OPENCODE_FLAGS=-console_watches pin depends on it; re-read the resolver and "
            "update this anchor (WI#609)"
        )
    print(
        f"compile_out_remote_tunnel: flag pin anchored ({len(registry)} reviewed flags; "
        f"'{PINNED_OFF_FLAG}' gates the Console watches; OPENCODE_FLAGS resolves first)"
    )


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
