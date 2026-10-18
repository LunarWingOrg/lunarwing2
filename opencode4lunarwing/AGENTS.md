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
- `entrypoint.sh` — Startup orchestration. Three modes: websocket, cli, acp.
- `config/opencode.json` — Mounted into container; controls LLM provider, plugins, MCP servers.

## Protocol Compatibility

The `lunarwing-agent-v1` WebSocket subprotocol is shared with Pebble and Kimi Code workers. The legacy `ironclaw-agent-v1` alias has been removed and is not accepted. Changes to `agent_comm_protocol.json` or envelope format must stay backward-compatible or be coordinated across all workers.

## Build Notes

The Dockerfile fetches anomalyco/opencode at the full SHA in `OPENCODE_REF` and requires its committed Bun lock. Update the release-to-commit mapping deliberately. Base images, Debian package indexes, rustup, and Go archives are pinned; refresh their recorded digests/checksums rather than restoring tag-only or fallback installs.

## Do Not

- Break the `lunarwing-agent-v1` envelope format
- Expose the internal opencode serve port (4096) externally
- Store secrets in config files (use env vars with `{env:VAR}` syntax)
- Remove the health server — orchestration depends on it
- Let the worker self-expose (WI#357): v1.18.25 has no service mode, pairing,
  or tunnel (those are v2 surfaces), but session sharing exists and stays
  pinned `"share": "disabled"` in `config/opencode.json`. Workers must never
  open their own inbound path or publish sessions; see
  `docs/ops/WORKER-CONTAINERS.md` → "Worker exposure policy".
