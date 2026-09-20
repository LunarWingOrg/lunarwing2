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

The `lunarwing-agent-v1` WebSocket subprotocol is shared with Pebble and OpenCode workers. The legacy `ironclaw-agent-v1` alias has been removed and is not accepted. Changes to `agent_comm_protocol.json` or envelope format must stay backward-compatible or be coordinated across all workers.

## Build Notes

The Dockerfile installs the official Kimi Code CLI with `npm ci` from `kimi-cli/package-lock.json`; update that lock and the `KIMI_CODE_VERSION` default together. The bridge uses `scripts/package-lock.json`. Base images, Debian package indexes, rustup, and Go archives are pinned, so refresh their recorded digests/checksums deliberately rather than restoring unlocked installers.

The in-image test gate runs `npm test` first, then re-runs just the real-CLI contract lane (`scripts/kimi_cli_contract_test.ts`) against the locked CLI — that lane self-skips on a host with no kimi binary, so the Dockerfile is the only place the flag contract (headless `-p` accepted, `--yolo` rejected alongside `-p`) is asserted for real.

## Do Not

- Break the `lunarwing-agent-v1` envelope format
- Store secrets in config files (use `KIMI_API_KEY` env var; entrypoint writes it to the runtime config)
- Remove the health server — orchestration depends on it
- Run `kimi` in interactive TUI mode inside the container — the worker uses `-p` (print mode) only
- Re-enable the upstream dangerous-command guard or permission-mode reminder — the worker is headless (no TTY to answer approval prompts); the guard must stay off via `config.toml` `[permission]` + the entrypoint exports, or every dangerous-class shell command stalls the task to its full timeout
