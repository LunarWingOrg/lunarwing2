# Agent Rules — opencode2.4lunarwing

## What This Is

Persistent container worker for upstream OpenCode **2** (the `v2` line, pinned
v2.2.5). This is a separate worker lane from `opencode4lunarwing/` (the proven
v1 worker, pinned v1.19.5) per
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
- `scripts/tsconfig.sdk-surface.json` — WI#369 compile-time gate: the Dockerfile builder stage runs `tsc --noEmit` (via the monorepo's pinned `@typescript/native` devDependency, `npm:typescript@7.1.0-beta` at the v2.2.5 pin — 7.0.2 at v2.1.2; v2.0.32's `@typescript/native-preview` shipped it as `tsgo`) over the SDK-facing scripts against the pinned generated client types, covering call argument and event payload shapes. The version comes from upstream's committed lock, never a separate fetch, and the binary path (`bin/tsc`) is what the Dockerfile invokes — re-check both on every bump (checklist item 7). Keep its `include` list covering every non-test script that imports `@opencode/client`. `scripts/gate-env.d.ts` carries the minimal runtime declarations the gate needs (the monorepo's bun install does not expose @types at a stable path); extend it when a gated script uses a new runtime API.
- `scripts/lunarwing_runtime.ts` — Shared types and envelope helpers. Keep in sync with `agent_comm_protocol.json`.
- `entrypoint-split.sh` — WI#454 bridge uid 10103:10003, private token home `/home/opencode2-bridge`; task role stays in `entrypoint.sh` as uid 10003:10003 and owns loopback serve. Both use no-new-privileges and no capabilities, with role-owned sticky `-ipc-v2` IPC. Rootful starts pin the workspace device/inode before setup.
- `scripts/task_runner.ts` / `scripts/task_runner_client.ts` — Bounded Unix task dispatch, cancellation and ping health. The task runner creates cron directories. Reject grants before delegation; jobs sharing this task container share authority. Keep the parser, image config and exported-source fixtures in sync with split identity ENV and boot files.
- `entrypoint.sh` — Three modes: websocket, cli, acp. Split images require an explicit websocket role; managed split-task runs setup, serve and the runner, split-bridge dispatches to the private launcher. Host legacy paths remain for build/config fixtures. Full lifecycle and migration contract: `docs/ops/WORKER-CONTAINERS.md`.
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
The candidate also exports `opencode2-rootfs.tar`; promotion requires the
out-of-band tar/config verifier. Do not replace that archive with BuildKit's
ownership-losing local directory export.
OpenCode 2 appends a timestamped, randomly-IDed log line to
`$HOME/.local/share/opencode/log/opencode.log` on EVERY start — including
`opencode --version` — so any build-time `RUN` that executes the binary must
run under the throwaway HOME/XDG environment rooted in the trap-cleaned
`/tmp/opencode2-build-tests` tree (see the self-test `RUN`s), and the
`/root/.bun` / `/root/.local` / `/root/.cache` / `/root/.config` leak
tripwires must stay. The started-entrypoint smoke must also put `WS_STATE_FILE`
under its trap-cleaned `/tmp/opencode2-runtime-smoke-entrypoint` tree; the bridge
writes a timestamped state file, and leaving it at `/tmp/lunarwing_ws_state.json`
changes the image layer digest between candidate and clean rebuild. Before the
smoke exits, `check_external_writes` fails closed if anything outside that tree
under `/tmp` or `/var/log/lunarwing` is newer than its marker. Every Dockerfile
`RUN` must leave no per-build residue and consume deterministic, pinned inputs;
the candidate/clean-rebuild digest gate remains mandatory. Never invoke bun or
the opencode binary in the image build with the default root HOME.

Keep `ARG SOURCE_DATE_EPOCH=0` in the runtime stage before `useradd` and keep
the same-layer shadow-day assertion. Shadow-utils records the epoch day in
`/etc/shadow`; without the ARG a candidate and rebuild spanning UTC midnight
have different file contents, which BuildKit's timestamp rewrite cannot fix
(WI#510). All four workers and `CI_DEBIAN_SNAPSHOT` use `20261105T082236Z`,
which includes the Bookworm/Trixie Brotli security fixes; bump them together.
Snapshot sources stay HTTPS, with `Acquire::Retries=0` and
`Acquire::https::Timeout=60` on every `apt-get` call (WI#561); transient recovery
belongs to bounded outer retries, not APT's automatic download retries. Keep both
direct CA-bundle copies from the digest-pinned Python image before APT and the
exact `3.0.4` installed-version assertions; slim bases may have no trust store.

The Bun 1.4.4 photon-node hunk-10 repair that this lane used to apply at the
v2.1.2 pin is retired: v2.2.5 carries the `61845b0` whitespace repair as a
content-equivalent backport (the tag does not contain that commit), so
`fix_photon_patch_context.py` would fail as redundant. The two fixer files,
their Dockerfile `COPY`/`RUN` and self-test invocation, and this lane's two
`input_files` entries in `.gitlab/scripts/build-worker-image.sh` were removed
together in the WI#531 bump; do not weaken the drift guard or downgrade Bun.
See checklist item 7 in `docs/ops/WORKER-CONTAINERS.md`.

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
  `"disabled"`) or self-update. The `update` pin in the lane config is *not*
  the guard (the updater reads only the global config dir, never
  `OPENCODE_CONFIG`); the live controls are the image env
  `OPENCODE_DISABLE_AUTOUPDATE=1` and the wrapper's `upgrade`/`update`
  refusal. Keep both. Keep the
  guards in place: the every-token wrapper at `/usr/local/bin/opencode`,
  the boot-time `service.json` reset (`disabled: true`, resolved via
  `OPENCODE_CONFIG_DIR` or `${XDG_CONFIG_HOME:-~/.config}/opencode` like
  upstream, removing any agent plant at the path first, so server-backed
  commands default to private servers), the session/config permission denies,
  the root-owned read-only `entrypoint.sh`, `health_server.py`,
  `agent_comm_protocol.json`, `scripts/`+`config/`, the image build assertions, and
  `compile_out_remote_tunnel.py` fail-closed when bumping the pinned ref.
  These are naive-use guards, not a boundary — the real binary stays
  executable; only the egress allowlist (WI#424 control 1, enforced by
  mt-admin's `worker-egress` on every worker start) holds against a
  compromised agent. The wrapper rejects explicit service commands;
  implicit startup is controlled by the agent-writable live `service.json`,
  which the entrypoint resets at each boot.

## Exposure Policy (WI#357)

Workers must never self-expose; LunarWing owns every public path through the
daemon's tunnel. See `docs/ops/WORKER-CONTAINERS.md` → "Worker exposure
policy" for the full control set, the !1490 gate record, and the bump
checklist. This lane pins v2.2.5 (the v2.0.26 boundary was crossed at
v2.0.32 under the maintainer-approved WI#427):
v2.0.26 introduced OpenTunnel remote access (hosted relay, no self-host
option). The compile-out guard stubs `remote-tunnel.ts` at build time,
re-verified against v2.2.5, and CI + the build assertions fail if the tunnel
survives. Future bumps remain deliberate-only. WI#369's SDK-gate blind spots remain
open; WI#424's egress allowlist is implemented (mt-admin `worker-egress`) and
`scripts/runtime_smoke_entrypoint.sh` starts the real entrypoint for the
candidate smoke; `scripts/runtime_smoke.py` also runs against a live container.

## Key Files (this policy)

- `scripts/opencode-exposure-guard.sh` — installed as `/usr/local/bin/opencode`; scans every argv token for `service`/`pair`/`upgrade`/`update`/`--service`/non-loopback `--hostname` (naive-use guard).
- `scripts/exposure_policy_assert.py` — shared static policy checker (image build + CI guard); `--image-filesystem` checks installed ownership/modes, all three wrapper execute bits, real-binary metadata, and bounded parent directories. `--image-root` uses explicit tar-header metadata and image-environment JSON rather than host ownership or the CI job environment.
- `scripts/image_config_policy.py` — checkout-owned allowlist for the complete image `Env` (PATH order included), `Entrypoint`, `Cmd`, `User`, `WorkingDir`, `Healthcheck`, `ExposedPorts`, and `StopSignal`, with `OnBuild`, `Volumes`, and `Shell` required empty; shared by the protected tar verifier and mt-admin. The verifier normalizes Docker's `Config.Healthcheck` and Podman's top-level `Healthcheck` before applying this policy.
- `../.gitlab/go/dockerfile-parse` — required BuildKit-parser Dockerfile policy; the old bash grammar is retired. Keep its Go tests and divergence fixtures current, and preserve the pinned Go CI job.
- `scripts/entrypoint_config_test.py` — non-root startup configuration regression tests using a 0444 baked config, including repeat boots, HOME/XDG/`OPENCODE_CONFIG_DIR` service defaults, and agent plants (FIFO/dir/symlink) at the reset path.
- `scripts/runtime_smoke.py` — WI#424 control 6 runtime exposure suite. The candidate build runs `scripts/runtime_smoke_entrypoint.sh` against the real started entrypoint (scoped to a private netns via `RUN --network=none` in the Dockerfile — WI#511: mt-admin builds with `podman build --network=host`, so without the private netns the listener check reads the HOST's socket table and flags host sshd/NetBird as worker exposure); `--profile container` runs inside a live worker and `--profile remote` reads the container's own environment from the host. Fails on a `remote` service config (including unreadable files), an opentunnel identity dir, a non-loopback listener beyond the health/WS ports, an enabled `share`, or a missing/crashing wrapper that does not produce the specific exit-77 `service set remote true` refusal; `scripts/runtime_smoke_test.py` pins each failure mode, including the WI#511 netns-scoping and real-kernel-row fixtures.
- `scripts/compile_out_remote_tunnel.py` — fail-closed build patch that compiles the OpenTunnel client out; drift scan over `packages/{cli,server,schema,core}/src` (ts+tsx, any quoted `@opentunnel/*` specifier, all workspace manifests).
- `scripts/opencode_task_executor.ts` — `sessionPermissions()` carries the shell deny rules (after the allow-all rule; ordering matters).
