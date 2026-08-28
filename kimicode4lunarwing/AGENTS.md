# Agent Rules — kimicode4lunarwing

## What This Is

Persistent container worker for upstream Kimi Code CLI (MoonshotAI/kimi-code). The WebSocket bridge is Node.js/TypeScript and spawns `kimi -p <prompt> --output-format stream-json` as a subprocess per task.

## Key Files

- `scripts/lunarwing_bridge.ts` — The WebSocket server/client. Changes here affect protocol behavior.
- `scripts/kimi_task_executor.ts` — Task execution via the Kimi Code CLI subprocess. Controls process spawn, stream-json parsing, timeout/cancel.
- `scripts/lunarwing_runtime.ts` — Shared types and envelope helpers. Keep in sync with `agent_comm_protocol.json`.
- `entrypoint.sh` — Startup orchestration. Two modes: websocket, cli.
- `config/config.toml` — Kimi Code CLI default config (model, provider, thinking). Mounted into container. Default provider is the TensorZero gateway (NetBird overlay `http://100.88.232.146:3000/openai/v1`, function `tensorzero::function_name::FrontierCODE`), not the managed api.kimi.com service — see README.

## Protocol Compatibility

The `lunarwing-agent-v1` WebSocket subprotocol (legacy alias `ironclaw-agent-v1` still accepted for one deprecation cycle) is shared with Pebble and OpenCode workers. Changes to `agent_comm_protocol.json` or envelope format must stay backward-compatible or be coordinated across all workers.

## Build Notes

The Dockerfile installs the official Kimi Code CLI from npm (`@moonshot-ai/kimi-code@<KIMI_CODE_VERSION>`). For reproducible builds, pin a specific version by changing the `KIMI_CODE_VERSION` build arg (default `0.39.0`). The bridge itself compiles from TypeScript at build time.

## Do Not

- Break the `lunarwing-agent-v1` envelope format
- Store secrets in config files (use `KIMI_API_KEY` env var; entrypoint writes it to the runtime config)
- Remove the health server — orchestration depends on it
- Run `kimi` in interactive TUI mode inside the container — the worker uses `-p` (print mode) only
