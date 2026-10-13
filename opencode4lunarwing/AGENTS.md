# Agent Rules — opencode4lunarwing

## What This Is

Persistent container worker for upstream opencode. The WebSocket bridge is TypeScript/Bun and uses `@opencode-ai/sdk/v2` to communicate with the internal opencode headless server.

## Key Files

- `scripts/lunarwing_bridge.ts` — The WebSocket server/client. Changes here affect protocol behavior.
- `scripts/opencode_task_executor.ts` — Task execution via opencode SDK. Controls session creation, permissions, event streaming.
- `scripts/lunarwing_runtime.ts` — Shared types and envelope helpers. Keep in sync with `agent_comm_protocol.json`.
- `scripts/opencode_result.ts` — Result/error extraction and the serve-log enrichment (`withServeLogDetail`): reads serve's structured log (`$XDG_DATA_HOME|~/.local/share/opencode/log/opencode.log`, or `OPENCODE_SERVE_LOG`; empty disables) to attach the root cause to generic "Unexpected server error" job failures. Path is frozen at module load.
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
