# Agent Rules — kimicode4lunarwing

## What This Is

Persistent container worker for upstream Kimi Code CLI (MoonshotAI/kimi-code). The WebSocket bridge is Node.js/TypeScript and spawns `kimi -p <prompt> --output-format stream-json` as a subprocess per task.

## Key Files

- `scripts/lunarwing_bridge.ts` — The WebSocket server/client. Changes here affect protocol behavior.
- `scripts/kimi_task_executor.ts` — Task execution via the Kimi Code CLI subprocess. Controls process spawn, stream-json parsing, timeout/cancel.
- `scripts/lunarwing_runtime.ts` — Shared types and envelope helpers. Keep in sync with `agent_comm_protocol.json`.
- `entrypoint.sh` — Startup orchestration. Two modes: websocket, cli.
- `config/config.toml` — Kimi Code CLI default config (model, provider, thinking, fail-closed permission policy). Baked into the image. Default provider is the TensorZero gateway (function `tensorzero::function_name::FrontierCODEK`), not the managed api.kimi.com service — see README. Since WI#330 the baked `base_url` is an `.invalid` placeholder that `entrypoint.sh` refuses to run; mt-admin renders `KIMI_BASE_URL` per tenant (default: the tenant's own LLM endpoint).
- `hooks/headless_command_guard.mjs` — WI#206 fail-closed PreToolUse guard. The 2.0.2 behavior was live-verified; the 2.1.1 CLI contract gate still needs a protected worker image build. Print mode forces the auto policy, the upstream dangerous-command guard is skipped in auto mode, and `[[permission.rules]]` never load from config — this hook is the only in-CLI enforcement point for the headless lane.

## Protocol Compatibility

The `lunarwing-agent-v1` WebSocket subprotocol is shared with Pebble and OpenCode workers. The legacy `ironclaw-agent-v1` alias has been removed and is not accepted. Changes to `agent_comm_protocol.json` or envelope format must stay backward-compatible or be coordinated across all workers.

## Build Notes

The Dockerfile installs the official Kimi Code CLI with `npm ci` from `kimi-cli/package-lock.json`; update that lock and the `KIMI_CODE_VERSION` default together. The bridge uses `scripts/package-lock.json`. Base images, Debian package indexes, rustup, and Go archives are pinned, so refresh their recorded digests/checksums deliberately rather than restoring unlocked installers.

The in-image test gate runs `npm test` first, then re-runs just the real-CLI contract lane (`scripts/kimi_cli_contract_test.ts`) against the locked CLI — that lane self-skips on a host with no kimi binary, so the Dockerfile is the only place the flag contract (headless `-p` accepted, `--yolo` rejected alongside `-p`, and the baked `[[hooks]]` PreToolUse guard table valid) is asserted for real.

## Do Not

- Break the `lunarwing-agent-v1` envelope format
- Store secrets in config files (use `KIMI_API_KEY` env var; entrypoint writes it to the runtime config)
- Remove the health server — orchestration depends on it
- Run `kimi` in interactive TUI mode inside the container — the worker uses `-p` (print mode) only
- Remove or weaken the fail-closed policy (WI#206): keep the `PreToolUse` Bash guard hook in `config.toml` + `hooks/headless_command_guard.mjs`, keep `dangerous_command_guard = true`, and never export `KIMI_CODE_DANGEROUS_COMMAND_GUARD` (the entrypoint unsets it and the executor scrubs it from per-task env). The permission-mode reminder stays off (`KIMI_CODE_PERMISSION_MODE_REMINDER=0`) — it is prompt noise for a fixed-mode worker, and with the hook enforcing denials there is no approval stall to avoid.
