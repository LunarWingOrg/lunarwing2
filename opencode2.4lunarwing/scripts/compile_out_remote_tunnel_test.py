#!/usr/bin/env python3
"""Self-test for compile_out_remote_tunnel.py (WI#357 control 8).

Builds synthetic upstream trees for the three pin shapes: predating the
tunnel (historical v2.0.24), carrying it (v2.1.2 shape), and drifted (module moved,
or a new RemoteTunnel export in use). The guard must pass the first, stub
the second, and fail closed on every drift shape.

The second half covers the flag pin anchor (WI#609 ABE-2): a tree with the
v2.2.7 shape of the flag registry, the resolver and the Console plugin
passes, and every way upstream could take the meaning out of the wrapper's
OPENCODE_FLAGS=-console_watches pin fails the build.
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
    'const load = () => import("@opentunnel/client/effect")\n'
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
        (root / "packages/cli/src/commands/handlers").mkdir(parents=True, exist_ok=True)
        (root / "packages/cli/src/commands/handlers/pair.ts").write_text(
            'import { RemoteTunnel } from "../../services/remote-tunnel"\n'
            "const h = yield* RemoteTunnel.hostname(route)\n",
            encoding="utf-8",
        )
    (root / "packages/cli").mkdir(parents=True, exist_ok=True)
    (root / "packages/cli/package.json").write_text(
        json.dumps({"name": "opencode", "dependencies": deps}), encoding="utf-8"
    )


FLAG_REGISTRY = "packages/schema/src/config/flag.ts"
FLAG_RESOLVER = "packages/core/src/flag.ts"
CONSOLE_PLUGIN = "packages/core/src/plugin/provider/opencode.ts"

# The lines of the three upstream files at v2.2.7 that the anchor reads, with
# their original line breaks.
REGISTRY_SOURCE = '''\
export * as ConfigFlag from "./flag.js"

/** The `OPENCODE_FLAGS` entry that turns every flag on, as in `all,-media_blob`. It is not a flag key. */
export const wildcard = "all"

/**
 * Every feature flag, by key, with what it turns on. A flag is on in local and dev builds and off in released ones;
 * `experimental.<key>` in config and `OPENCODE_FLAGS` override that, and `OPENCODE_FLAGS=all` or
 * `experimental.all_flags: true` turn every flag on. Read one with Core's `Flag.enabled("<key>")`.
 */
export const registry = {
  todo: "Enable the experimental todowrite tool.",
  background_compaction:
    "Enable experimental background compaction: summarize the conversation in the background as it nears the compaction threshold, so automatic compaction does not wait for the summary. Background summaries that are never used still cost tokens.",
  media_blob: "Store new session media once in a deduplicated blob table instead of inline in every message row.",
  schedules: "Enable experimental scheduled tasks that wake a session with a prompt.",
  artifacts: "Enable experimental artifacts backed by pluggable providers.",
  codemode_vm: "Run the Code Mode execute tool on the experimental bytecode VM instead of the legacy interpreter.",
  console_watches:
    "Watch Slack threads, GitHub pull requests, issues, commits and Actions runs, and the workspace's MCP servers' events through OpenCode Console, and wake sessions when they change.",
} as const satisfies Record<string, string>
export type Key = keyof typeof registry
'''

RESOLVER_SOURCE = '''\
export * as Flag from "./flag.js"

import { Context, Effect, Layer, Record } from "effect"
import { makeLocationNode } from "@opencode/util/effect/app-node"
import { ConfigFlag } from "@opencode/schema/config/flag"
import type { Document, Entry } from "@opencode/schema/config"
import { App } from "./app.js"
import { Config } from "./config.js"

export type Key = ConfigFlag.Key

export interface State {
  readonly enabled: boolean
  /** Where `enabled` came from, highest priority first. */
  readonly source: "env" | "config" | "default"
}

export interface Interface {
  readonly resolve: (entries: readonly Entry[]) => Record<Key, State>
  readonly all: Effect.Effect<Record<Key, State>>
  readonly enabled: (key: Key) => Effect.Effect<boolean>
}

export class Service extends Context.Service<Service, Interface>()("@opencode/Flag") {}

/**
 * `OPENCODE_FLAGS`, a comma-separated list such as `schedules,-media_blob`; a leading `-` turns a flag off, and
 * `all` turns every flag on unless the same list opts one out, as in `all,-media_blob`.
 */
export const Overrides = Context.Reference<string | undefined>("@opencode/Flag/Overrides", {
  defaultValue: () => process.env.OPENCODE_FLAGS,
})

// Any other channel, such as a release or a branch preview build, keeps flags off so none turns on by accident.
const onByDefault = (channel: string) => channel === "local" || channel === "dev"

/**
 * Resolves every flag: `OPENCODE_FLAGS` (`-key`, then `all`), then `experimental.<key>` from the highest-priority
 * document that sets `experimental` (the same document every other `experimental` setting reads), then `all_flags: true`
 * from the highest-priority document that sets `all_flags`, so a global switch survives a project's own `experimental`
 * block, then on for local and dev builds. `all_flags: false` falls through rather than turning flags off.
 * A remote kill switch belongs above `OPENCODE_FLAGS` here.
 */
export function resolve(input: {
  readonly entries: readonly Entry[]
  readonly channel: string
  readonly overrides?: string
}) {
  const env = new Map(
    (input.overrides ?? "")
      .split(",")
      .map((item) => item.trim())
      .filter(Boolean)
      .map((item): [string, boolean] => (item.startsWith("-") ? [item.slice(1), false] : [item, true])),
  )
  const all = env.get(ConfigFlag.wildcard) === true
  const experimental = Config.latest(input.entries, "experimental")
  const allFlags =
    input.entries.findLast(
      (entry): entry is Document => entry.type === "document" && entry.info.experimental?.all_flags !== undefined,
    )?.info.experimental?.all_flags === true
  return Record.map(ConfigFlag.registry, (_, key): State => {
    const override = env.get(key) ?? (all ? true : undefined)
    const configured = experimental?.[key] ?? (allFlags ? true : undefined)
    const [enabled, source]: [boolean, State["source"]] =
      override !== undefined
        ? [override, "env"]
        : configured !== undefined
          ? [configured, "config"]
          : [onByDefault(input.channel), "default"]
    return { enabled, source }
  })
}

export const layer = Layer.effect(
  Service,
  Effect.gen(function* () {
    const app = yield* App.Service
    const config = yield* Config.Service
    const overrides = yield* Overrides
    const resolveEntries = (entries: readonly Entry[]) => resolve({ entries, channel: app.channel, overrides })
    const all = Effect.gen(function* () {
      return resolveEntries(yield* config.entries())
    })
    return Service.of({
      resolve: resolveEntries,
      all,
      enabled: (key) => Effect.map(all, (flags) => flags[key].enabled),
    })
  }),
)

export const testLayer = (
  entries: Entry[] = [],
  options?: { readonly app?: Partial<App.Info>; readonly overrides?: string },
) =>
  Layer.fresh(layer).pipe(
    Layer.provideMerge(
      Layer.mergeAll(
        App.layer(options?.app),
        Config.testLayer(entries),
        Layer.succeed(Overrides, options?.overrides),
      ),
    ),
  )

export const node = makeLocationNode({
  service: Service,
  layer,
  deps: [App.node, Config.node],
})

/** Every flag with its value and where it came from, read from the location's Config on each call. */
export const all = Service.use((flags) => flags.all)

export const enabled = (key: Key) => Service.use((flags) => flags.enabled(key))
'''

PLUGIN_SOURCE = '''\
      yield* syncInstructionsAndSkills(next, allowed)
      const watching = allowed && next.remote?.watches === true && (yield* flags.enabled("console_watches"))
      yield* syncWatches(watching ? next.connection : undefined)
      const provider = OpencodeWatches.layer()
'''


def write_flag_files(root: pathlib.Path) -> None:
    for relative, source in (
        (FLAG_REGISTRY, REGISTRY_SOURCE),
        (FLAG_RESOLVER, RESOLVER_SOURCE),
        (CONSOLE_PLUGIN, PLUGIN_SOURCE),
    ):
        (root / relative).parent.mkdir(parents=True, exist_ok=True)
        (root / relative).write_text(source, encoding="utf-8")
    watches = root / "packages/core/src/plugin/provider/opencode-watches.ts"
    watches.write_text(
        'export const OpencodeWatches = {}\nconst namespace = "console_watch"\n',
        encoding="utf-8",
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


class FlagPinAnchorTests(unittest.TestCase):
    """WI#609 ABE-2: the build must stop when the wrapper's pin stops working."""

    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name) / "opencode"
        self.root.mkdir()
        write_tree(self.root, with_tunnel=True)
        write_flag_files(self.root)

    def edit(self, relative: str, old: str, new: str) -> None:
        path = self.root / relative
        text = path.read_text(encoding="utf-8")
        self.assertEqual(text.count(old), 1, old)
        path.write_text(text.replace(old, new), encoding="utf-8")

    def assert_fails(self, *fragments: str) -> None:
        result = run_guard(self.root)
        self.assertEqual(result.returncode, 1, result.stdout)
        for fragment in fragments:
            self.assertIn(fragment, result.stderr)
        # The anchor runs before the tunnel is stubbed, so a failed build
        # never leaves a half-prepared tree behind.
        self.assertNotIn(STUB_MARK, (self.root / MODULE).read_text(encoding="utf-8"))

    def test_pinned_shape_passes_and_the_tunnel_is_still_stubbed(self) -> None:
        result = run_guard(self.root)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("flag pin anchor text unchanged", result.stdout)
        self.assertIn(STUB_MARK, (self.root / MODULE).read_text(encoding="utf-8"))

    def test_pin_without_any_flag_surface_passes(self) -> None:
        for relative in (FLAG_REGISTRY, FLAG_RESOLVER, CONSOLE_PLUGIN):
            (self.root / relative).unlink()
        (self.root / "packages/core/src/plugin/provider/opencode-watches.ts").unlink()
        result = run_guard(self.root)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("no flag pin to anchor", result.stdout)

    def test_shipped_feature_fails_closed(self) -> None:
        # Upstream ships a feature by deleting its flag: the pin would then
        # name an unknown flag, which the resolver ignores without a word.
        self.edit(
            FLAG_REGISTRY,
            '  console_watches:\n'
            '    "Watch Slack threads, GitHub pull requests, issues, commits and Actions runs, and '
            "the workspace's MCP servers' events through OpenCode Console, and wake sessions when they change.\",\n",
            "",
        )
        self.assert_fails("differs from the reviewed v2.2.7 text", "may predate the flag")

    def test_new_flag_fails_until_reviewed(self) -> None:
        self.edit(FLAG_REGISTRY, "  todo:", '  remote_control: "Let a hosted service drive the session.",\n  todo:')
        self.assert_fails("differs from the reviewed v2.2.7 text", "remote_control")

    def test_removed_other_flag_fails_until_reviewed(self) -> None:
        self.edit(FLAG_REGISTRY, '  schedules: "Enable experimental scheduled tasks that wake a session with a prompt.",\n', "")
        self.assert_fails("differs from the reviewed v2.2.7 text", "schedules")

    def test_reshaped_registry_fails_closed(self) -> None:
        self.edit(FLAG_REGISTRY, "export const registry = {", "export const registry: Registry = {")
        self.assert_fails("no longer declares")

    def test_watches_no_longer_gated_by_the_flag_fails_closed(self) -> None:
        self.edit(CONSOLE_PLUGIN, ' && (yield* flags.enabled("console_watches"))', "")
        self.assert_fails("no longer starts the watches on flags.enabled")

    def test_gate_moved_off_the_watching_expression_fails_closed(self) -> None:
        # The flag is still read somewhere in the file, but no longer decides
        # whether the watches start.
        self.edit(
            CONSOLE_PLUGIN,
            'const watching = allowed && next.remote?.watches === true && (yield* flags.enabled("console_watches"))',
            'const telemetry = yield* flags.enabled("console_watches")\n'
            "      const watching = allowed && next.remote?.watches === true",
        )
        self.assert_fails("no longer starts the watches on flags.enabled")

    def test_resolver_drift_fails_closed(self) -> None:
        # The resolver file is whole-file digest-pinned, so ANY edit to it —
        # precedence, a kill switch above the override, a renamed variable —
        # fails the anchor, even one that keeps the old fragments intact.
        for label, old, new in (
            (
                "config consulted before the override",
                "override !== undefined",
                "configured !== undefined",
            ),
            (
                "a kill switch above the override",
                "      override !== undefined",
                "      remote !== undefined",
            ),
            ("no opt-out syntax", 'startsWith("-")', "true"),
            ("another variable", "process.env.OPENCODE_FLAGS", "process.env.OPENCODE_FEATURES"),
            ("override not looked up per key", "env.get(key) ??", "all ?"),
            ("the result changed after it was resolved", "return { enabled, source }", "return { enabled }"),
        ):
            with self.subTest(label=label):
                (self.root / MODULE).write_text(REAL_MODULE, encoding="utf-8")
                (self.root / FLAG_RESOLVER).write_text(RESOLVER_SOURCE, encoding="utf-8")
                self.edit(FLAG_RESOLVER, old, new)
                self.assert_fails("differs from the reviewed v2.2.7 text")

    def test_moved_flag_files_fail_closed(self) -> None:
        for relative in (FLAG_REGISTRY, FLAG_RESOLVER, CONSOLE_PLUGIN):
            with self.subTest(relative=relative):
                (self.root / MODULE).write_text(REAL_MODULE, encoding="utf-8")
                write_flag_files(self.root)
                (self.root / relative).unlink()
                self.assert_fails("is gone", relative)

    def test_reworded_registry_description_fails_closed(self) -> None:
        # A reworded description changes no key but does change the digest:
        # the anchor is a text tripwire and fails on any difference.
        self.edit(
            FLAG_REGISTRY,
            "Store new session media once in a deduplicated blob table",
            "Store media once in a table",
        )
        self.assert_fails("differs from the reviewed v2.2.7 text")

    def test_watching_computed_but_unused_fails_closed(self) -> None:
        # `watching` is computed and then ignored: the flag no longer decides
        # whether the watches start, even though the text mentions both.
        self.edit(
            CONSOLE_PLUGIN,
            "yield* syncWatches(watching ? next.connection : undefined)",
            "yield* syncWatches(next.connection)",
        )
        self.assert_fails("no longer starts the watches on flags.enabled")

    def test_second_sync_watches_call_fails_closed(self) -> None:
        # A second, unconditional call site would start the watches without
        # the flag, beside the gated one.
        plugin = self.root / CONSOLE_PLUGIN
        plugin.write_text(
            plugin.read_text(encoding="utf-8") + "      yield* syncWatches(next.connection)\n",
            encoding="utf-8",
        )
        self.assert_fails("no longer starts the watches on flags.enabled")

    def test_new_opencode_watches_file_fails_closed(self) -> None:
        # A new file that names the watches wiring means a new starter or
        # wiring point the review did not see.
        path = self.root / "packages/core/src/plugin/provider/second-watches.ts"
        path.write_text("export const OpencodeWatches = 1\n", encoding="utf-8")
        self.assert_fails("the files that name the watch wiring changed", "OpencodeWatches")

    def test_new_flag_overrides_provider_fails_closed(self) -> None:
        # A new provider of Flag.Overrides would sit above the environment
        # default and take the meaning out of the wrapper's pin.
        path = self.root / "packages/core/src/plugin/provider/overrides.ts"
        path.write_text(
            'import { Flag } from "@opencode/flag"\nexport const kill = Flag.Overrides\n',
            encoding="utf-8",
        )
        self.assert_fails("the files that name the watch wiring changed", "Flag.Overrides")

    def test_flag_files_moved_with_a_hidden_watch_surface_fails_closed(self) -> None:
        # The early pass must not fire when the watch code still exists but
        # has moved outside packages/*/src: the whole-tree scan sees it.
        for relative in (FLAG_REGISTRY, FLAG_RESOLVER, CONSOLE_PLUGIN):
            (self.root / relative).unlink()
        (self.root / "packages/core/src/plugin/provider/opencode-watches.ts").unlink()
        moved = self.root / "packages/legacy/plugin.ts"
        moved.parent.mkdir(parents=True)
        moved.write_text('const watch = flags.enabled("console_watches")\n', encoding="utf-8")
        self.assert_fails("is gone")

    def test_older_pin_registry_diagnosis_is_hedged(self) -> None:
        # A registry without console_watches could be an older pin (the flag
        # never existed) or a shipped feature; the diagnosis must not claim
        # "always on".
        self.edit(
            FLAG_REGISTRY,
            '  console_watches:\n'
            '    "Watch Slack threads, GitHub pull requests, issues, commits and Actions runs, and '
            "the workspace's MCP servers' events through OpenCode Console, and wake sessions when they change.\",\n",
            "",
        )
        result = run_guard(self.root)
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("differs from the reviewed v2.2.7 text", result.stderr)
        self.assertIn("may predate the flag", result.stderr)
        self.assertNotIn("always on", result.stderr)


if __name__ == "__main__":
    unittest.main()
