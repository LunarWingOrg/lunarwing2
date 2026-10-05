# Agent Rules — opencode2.4lunarwing

## What This Is

Persistent container worker for upstream OpenCode **2** (the `v2` line, pinned
v2.0.23). This is a separate worker lane from `opencode4lunarwing/` (the proven
v1 worker, pinned v1.18.25) per
`docs/plans/OPENCODE2_EXTERNAL_WORKER_PLAN.md`. Tenants opt in explicitly; v1
stays the default until v2 earns trust. The WebSocket bridge is
TypeScript/Bun and uses `@opencode/client/promise` to communicate with the
internal opencode headless server.

## Key Files

- `scripts/lunarwing_bridge.ts` — The WebSocket server/client. Changes here affect protocol behavior.
- `scripts/opencode_task_executor.ts` — Task execution via the v2 client SDK. Controls session creation, permissions, event streaming.
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
  from `session.message.list`.
- Cancellation uses `session.interrupt` (v2 dropped `session.abort`).
- The v2 config file is `opencode2.json`, kept deliberately separate from the
  v1 worker's `opencode.json`.
- v1 plugins are NOT ported yet. Re-add deliberately after the lane proves
  itself.

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
