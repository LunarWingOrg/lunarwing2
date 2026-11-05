# Agent Rules — opencode4lunarwing

## What This Is

Persistent container worker for upstream opencode. The WebSocket bridge is TypeScript/Bun and uses `@opencode-ai/sdk/v2` to communicate with the internal opencode headless server.

## Key Files

- `scripts/lunarwing_bridge.ts` — The WebSocket server/client. Changes here affect protocol behavior.
- `scripts/opencode_task_executor.ts` — Task execution via opencode SDK. Controls session creation, permissions, event streaming.
- `scripts/lunarwing_runtime.ts` — Shared types and envelope helpers. Keep in sync with `agent_comm_protocol.json`.
- `scripts/opencode_result.ts` — Result/error extraction and the serve-log enrichment (`withServeLogDetail`): reads serve's structured log (`$XDG_DATA_HOME|~/.local/share/opencode/log/opencode.log`, or `OPENCODE_SERVE_LOG`; empty disables) to attach the root cause to generic "Unexpected server error" job failures. Path is frozen at module load.
- `scripts/lunarwing_tool_proxy.ts` — WI#324 worker-side relay: per-task unix socket that turns the in-task plugin's requests into `tool_call`/`tool_list` hub frames and routes replies back by call id. Same wire contract as the kimi lane's relay.
- `scripts/lunarwing_tool_plugin.ts` — WI#324 opencode plugin (registered in `config/opencode.json`) that statically exposes the phase-1 tools to the serve process. opencode owns MCP servers per project Instance with frozen env, so instead of an MCP shim each tool's `execute()` resolves the calling session's task id from its title (`lunarwing-opencode-<task_id>`) via the plugin host's authenticated client and relays over the per-task socket. Dependency-free on purpose (the runtime image ships only `@opencode-ai/sdk`).
- `entrypoint-split.sh` — WI#454 managed pair: bridge uid 10101:10001 holds the token under `/home/opencode-bridge`; task uid 10001:10001 owns serve and task execution. Both drop all capabilities. IPC is role-owned on the sticky `-ipc-v2` volume; rootful launchers verify the mounted workspace inode.
- `scripts/task_runner.ts` / `scripts/task_runner_client.ts` — Bounded Unix task dispatch, cancellation, follow-ups and ping health. Cron directory creation runs in the task container. Reject credential grants before delegation; prompt refusal must not abort the run. Jobs inside one task container still share authority.
- `entrypoint.sh` — Dispatches managed roles to the split launcher; retains cli/acp and host legacy paths. Split images require an explicit websocket role. See `docs/ops/WORKER-CONTAINERS.md` for paired lifecycle, mounts and migration.
- `entrypoint-task-setup.sh` — Task-uid setup (git/SSH, config materialization, MCP injection). WI#529: the baked `/app/config` is immutable (`chmod -R a-w` + build assertion), so every consumer that rewrites a config calls `materialize_config_copy`/`ensure_config_rewritable` — those two helpers own the rewrite contract, and `.gitlab/tests/test_worker_config_rewrite_contract.py` binds them to the Dockerfile gate and the live boot. Do not add an in-place writer that skips them.
- `config/opencode.json` — Mounted into container; controls LLM provider, plugins, MCP servers.
- `models-dev/api.json` — WI#467 reviewed models.dev catalog snapshot compiled into the binary at build time (`MODELS_DEV_API_JSON`), committed pretty-printed (`json.dumps` indent=1, `ensure_ascii=False`); sha256 pinned as the Dockerfile's `MODELS_DEV_SNAPSHOT_SHA256` ARG and cross-checked by `validate_config.py`, which also enforces strict JSON in that layout. Refresh both in one deliberate commit (command in `docs/ops/WORKER-CONTAINERS.md`).
- `scripts/check_models_dev_snapshot.py` — WI#467 fail-closed build assert: the pinned upstream `generate.ts` must still read `MODELS_DEV_API_JSON` and `OPENCODE_MODELS_URL` (the build points the latter at a closed loopback port, so any live fetch fails), and the snapshot must be strict JSON.

## Protocol Compatibility

The `lunarwing-agent-v1` WebSocket subprotocol is shared with Pebble and Kimi Code workers. The legacy `ironclaw-agent-v1` alias has been removed and is not accepted. Changes to `agent_comm_protocol.json` or envelope format must stay backward-compatible or be coordinated across all workers.

## Build Notes

The Dockerfile fetches anomalyco/opencode at the full SHA in `OPENCODE_REF` and requires its committed Bun lock. Update the release-to-commit mapping deliberately. Base images, Debian package indexes, rustup, and Go archives are pinned; refresh their recorded digests/checksums rather than restoring tag-only or fallback installs. The models.dev catalog the build compiles into the binary is pinned the same way (WI#467): the committed `models-dev/api.json` snapshot plus the `MODELS_DEV_SNAPSHOT_SHA256` ARG, never a live fetch — update both together when refreshing. `validate_config.py` pins the builder-stage build steps exactly (`OPENCODE_MODELS_DEV_BUILD_STEPS`); update that tuple in the same commit when a bump changes them.

Keep `ARG SOURCE_DATE_EPOCH=0` before runtime account creation and preserve the same-layer `/etc/shadow` epoch-day assertion (WI#510): timestamp rewriting cannot fix a wall-clock day embedded in file contents. All four worker snapshots and `CI_DEBIAN_SNAPSHOT` move together; `20261024T011300Z` includes the Bookworm/Trixie Brotli OS-package fixes. Snapshot sources stay HTTPS, with `Acquire::Retries=0` and `Acquire::https::Timeout=60` on every `apt-get` call (WI#561); transient recovery belongs to bounded outer retries, not APT's automatic download retries.

The Bun 1.4.4 photon-node hunk-10 repair that this lane used to apply at the
v1.18.25 pin is retired: v1.19.5 carries the `61845b0` whitespace repair as a
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
- Let the worker self-expose (WI#357): v1.19.5 has no service mode, pairing,
  or tunnel (those are v2 surfaces), but session sharing exists and stays
  pinned `"share": "disabled"` in `config/opencode.json`. Workers must never
  open their own inbound path or publish sessions; see
  `docs/ops/WORKER-CONTAINERS.md` → "Worker exposure policy".
- Widen the baked `/app/config` modes to make a config writer work (WI#529).
  The image hardening is the contract; consumers widen their own workspace
  copy through the task-setup helpers instead.
