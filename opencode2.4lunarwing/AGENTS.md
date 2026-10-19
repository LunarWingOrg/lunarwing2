# Agent Rules — opencode2.4lunarwing

## What This Is

Persistent container worker for upstream OpenCode **2** (the `v2` line, pinned
v2.0.32). This is a separate worker lane from `opencode4lunarwing/` (the proven
v1 worker, pinned v1.18.25) per
`docs/plans/OPENCODE2_EXTERNAL_WORKER_PLAN.md`. Tenants opt in explicitly; v1
stays the default until v2 earns trust. The WebSocket bridge is
TypeScript/Bun and uses `@opencode/client/promise` to communicate with the
internal opencode headless server.

## Key Files

- `scripts/lunarwing_bridge.ts` — The WebSocket server/client. Changes here affect protocol behavior. Starts/stops the WI#324 per-task tool-proxy relay around each task and routes hub `tool_result`/`tool_catalog` frames to it.
- `scripts/lunarwing_tool_proxy.ts` — WI#324 bridge-side relay: per-task unix socket the serve-side plugin connects to, translating its NDJSON IPC into `tool_list`/`tool_call` hub frames (256 KiB frame cap, `[A-Za-z0-9_-]{1,128}` identifiers, never-throw task lifecycle).
- `scripts/lunarwing_tool_plugin/index.ts` — WI#324 v2 serve-process plugin (directory plugin registered by absolute path in `config/opencode2.json`'s `plugins` array; v2 drops configured plugin *file* paths). Statically registers the phase-1 `lunarwing_*` tools via the v2 promise API (`{id, setup}` default export, `ctx.tool.transform`, `codemode: false`, plain JSON Schema inputs) and relays each call over the session's task socket. Dependency-free on purpose — the runtime image ships no `@opencode/plugin`/`effect` for it to import.
- `scripts/opencode_task_executor.ts` — Task execution via the v2 client SDK. Controls session creation, permissions, event streaming.
- `scripts/sdk_surface.ts` — Canonical list of the `@opencode/client` method paths the bridge calls and the event `type` literals it matches (WI#329). It drives the image-build gate (`scripts/sdk_surface_assert.ts`, run from the Dockerfile — asserts the method paths against the real pinned client and the event names against the pinned generated V2Event union, WI#369) and the executor self-test, which binds it to the injected mock, to the call sites/event literals extracted from every non-test `scripts/*.ts` source, and — via a recording Proxy over the mock — to every client leaf path the suite actually touches. ADD to this list whenever the bridge calls a new client method or matches a new event; never shrink it to force an assertion to pass.
- `scripts/tsconfig.sdk-surface.json` — WI#369 compile-time gate: the Dockerfile builder stage runs `tsgo --noEmit` (from the monorepo's pinned devDependencies) over the SDK-facing scripts against the pinned generated client types, covering call argument and event payload shapes. Keep its `include` list covering every non-test script that imports `@opencode/client`. `scripts/gate-env.d.ts` carries the minimal runtime declarations the gate needs (the monorepo's bun install does not expose @types at a stable path); extend it when a gated script uses a new runtime API.
- `scripts/lunarwing_runtime.ts` — Shared types and envelope helpers. Keep in sync with `agent_comm_protocol.json`.
- `entrypoint.sh` — Startup orchestration. Three modes: websocket, cli, acp.
- `config/opencode2.json` — Mounted into container; controls LLM provider, MCP servers. v2 config shape (`providers`, `settings.baseURL`, `package`).

## Protocol Compatibility

The `lunarwing-agent-v1` WebSocket subprotocol is shared with Pebble, Kimi
Code, and the v1 OpenCode workers. Changes to `agent_comm_protocol.json` or
envelope format must stay backward-compatible or be coordinated across all
workers.

## v2-Specific Notes

- The v2 client SDK is `@opencode/client` (promise API:
  `OpenCode.make({ baseUrl, headers })`), sourced from the same build as the
  CLI binary. The v1 SDK import path (`@opencode-ai/sdk/v2/client`) does not
  exist in v2.
- Prompt completion settles via `session.execution.{succeeded,failed,
  interrupted}` / `session.idle` events; canonical assistant output is read
  from the top-level `message.list` (`GET /api/session/:sessionID/message`,
  paginated `{ data, cursor }`). The promise client has NO
  `session.message.list` — `session.message` only exposes `get`.
- Cancellation uses `session.interrupt` (v2 dropped `session.abort`).
- The v2 config file is `opencode2.json`, kept deliberately separate from the
  v1 worker's `opencode.json`. opencode v2 only auto-discovers
  `opencode.json`/`opencode.jsonc`, so `entrypoint.sh` exports
  `OPENCODE_CONFIG` pointing at the materialized
  `$WORKSPACE_ROOT/.opencode/opencode2.json`; without it the server silently
  ignores the lane config. Provider `package` is the plain npm name
  (`@ai-sdk/openai-compatible`, mapped to v2's bundled provider) — an `ai:`
  prefix fails with `Unsupported URL Type "ai:"`.
- v1 plugins do not load on v2 — the v1 file-plugin shape (`{id, server}`)
  does not exist there. The WI#324 tool proxy is the one deliberate
  exception, re-implemented on the v2 promise plugin API as
  `scripts/lunarwing_tool_plugin/` (a directory plugin; see the design doc
  `docs/proposals/EXTERNAL-WORKER-TOOL-PROXY.md`). Re-add anything else
  deliberately after the lane proves itself.

## Build Notes

The Dockerfile fetches anomalyco/opencode at the full SHA in `OPENCODE_REF`
and requires its committed Bun lock. Update the release-to-commit mapping
deliberately and treat each v2 bump like the v1.18.29 incident until proven.
Base images, Debian package indexes, rustup, and Go archives are pinned;
refresh their recorded digests/checksums rather than restoring tag-only or
fallback installs.

The CI publication gate requires the whole image to be byte-reproducible
(candidate vs uncached clean rebuild; both must report the same digest).
OpenCode 2 appends a timestamped, randomly-IDed log line to
`$HOME/.local/share/opencode/log/opencode.log` on EVERY start — including
`opencode --version` — so any build-time `RUN` that executes the binary must
run under the throwaway HOME/XDG environment rooted in the trap-cleaned
`/tmp/opencode2-build-tests` tree (see the self-test `RUN`s), and the
`/root/.bun` / `/root/.local` / `/root/.cache` / `/root/.config` leak
tripwires must stay. Never invoke bun or the opencode binary in the image
build with the default root HOME.

## Do Not

- Break the `lunarwing-agent-v1` envelope format
- Expose the internal opencode serve port (4096) externally
- Store secrets in config files (use env vars with `{env:VAR}` syntax)
- Remove the health server — orchestration depends on it
- Configure opencode and opencode2 on the same tenant during the transition
  (agent routing confusion; see plan §5)
- Let the worker self-expose (WI#357): never start service mode
  (`opencode service …`, `serve --service`), pairing (`opencode pair …`),
  or a non-loopback `serve`; never enable session sharing (`share` stays
  `"disabled"`) or self-update (`update` stays `"disable"`). Keep the
  guards in place: the every-token wrapper at `/usr/local/bin/opencode`,
  the boot-time `service.json` reset (`disabled: true`, resolved via
  `OPENCODE_CONFIG_DIR` or `${XDG_CONFIG_HOME:-~/.config}/opencode` like
  upstream, removing any agent plant at the path first, so server-backed
  commands default to private servers), the session/config permission denies,
  the root-owned read-only `entrypoint.sh`, `health_server.py`,
  `agent_comm_protocol.json`, `scripts/`+`config/`, the image build assertions, and
  `compile_out_remote_tunnel.py` fail-closed when bumping the pinned ref.
  These are naive-use guards, not a boundary — the real binary stays
  executable; only the egress allowlist (WI#357 control 1, deferred) holds
  against a compromised agent. The wrapper rejects explicit service commands;
  implicit startup is controlled by the agent-writable live `service.json`,
  which the entrypoint resets at each boot.

## Exposure Policy (WI#357)

Workers must never self-expose; LunarWing owns every public path through the
daemon's tunnel. See `docs/ops/WORKER-CONTAINERS.md` → "Worker exposure
policy" for the full control set, the !1490 gate record, and the bump
checklist. This lane pins v2.0.32 under the maintainer-approved WI#427:
v2.0.26 introduced OpenTunnel remote access (hosted relay, no self-host
option). The compile-out guard stubs `remote-tunnel.ts` at build time,
re-verified against v2.0.32, and CI + the build assertions fail if the tunnel
survives. Future bumps remain deliberate-only. WI#369's SDK-gate blind spots
and WI#424's egress allowlist remain open; see the bump record.

## Key Files (this policy)

- `scripts/opencode-exposure-guard.sh` — installed as `/usr/local/bin/opencode`; scans every argv token for `service`/`pair`/`upgrade`/`update`/`--service`/non-loopback `--hostname` (naive-use guard).
- `scripts/exposure_policy_assert.py` — shared static policy checker (image build + CI guard); `--image-filesystem` also checks installed ownership and modes and (v2, run-103 GD-2 + run-106 GJ-3) verifies the installed wrapper: `/usr/local/bin/opencode` must be the byte-identical, root-owned, non-group-writable, executable guard that build-time `PATH` resolves to, with the real binary re-scanned for tunnel markers, no `LUNARWING_REAL_OPENCODE` retarget at build time, and the installed digest matching the `LUNARWING_WRAPPER_SHA256` build-arg anchor (computed from the checkout by `build-worker-image.sh` and mt-admin's `build_opencode2_worker`; missing or mismatched fails the build).
- `scripts/entrypoint_config_test.py` — non-root startup configuration regression tests using a 0444 baked config, including repeat boots, HOME/XDG/`OPENCODE_CONFIG_DIR` service defaults, and agent plants (FIFO/dir/symlink) at the reset path.
- `scripts/compile_out_remote_tunnel.py` — fail-closed build patch that compiles the OpenTunnel client out; drift scan over `packages/{cli,server,schema,core}/src` (ts+tsx, any quoted `@opentunnel/*` specifier, all workspace manifests).
- `scripts/opencode_task_executor.ts` — `sessionPermissions()` carries the shell deny rules (after the allow-all rule; ordering matters).
