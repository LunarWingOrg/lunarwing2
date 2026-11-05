# Agent Rules — kimicode4lunarwing

## What This Is

Persistent container worker for upstream Kimi Code CLI (MoonshotAI/kimi-code). The WebSocket bridge is Node.js/TypeScript and spawns `kimi -p <prompt> --output-format stream-json` as a subprocess per task.

## Key Files

- `scripts/lunarwing_bridge.ts` — The WebSocket server/client. Changes here affect protocol behavior.
- `scripts/kimi_task_executor.ts` — Task execution via the Kimi Code CLI subprocess. Controls process spawn, stream-json parsing, timeout/cancel.
- `scripts/lunarwing_runtime.ts` — Shared types and envelope helpers. Keep in sync with `agent_comm_protocol.json`.
- `scripts/lunarwing_tool_proxy.ts` — WI#324 worker-side relay: per-task unix socket that turns the in-task MCP shim's requests into `tool_call`/`tool_list` hub frames and routes replies back by call id.
- `scripts/lunarwing_mcp_server.ts` — WI#324 thin stdio MCP server the CLI spawns per task (registered in `$KIMI_CODE_HOME/mcp.json` by entrypoint.sh). Advertises zero tools without the per-task proxy env; the hub stays deny-by-default without a `tool_allowlist` grant.
- `scripts/task_runner.ts` — WI#454 task-container daemon, uid 10002 (refuses root and hub credential variables). Owns `/run/lunarwing-shared/runner/task-runner.sock`, executes tasks and provides the bounded ping health probe.
- `scripts/task_runner_client.ts` — Bridge-container delegation client, uid 10102 in task group 10002. Cancels on the run's own connection; the task container owns process cleanup across the PID namespace boundary.
- `entrypoint-split.sh` — Managed `split-bridge` / `split-task` launcher. The bridge's token is private to `/home/kimi-bridge`, task setup runs as kimi, and rootful starts verify the pinned workspace inode before setup. IPC uses the role-owned sticky `-ipc-v2` volume; this separates bridge credentials, not jobs sharing the task uid.
- `task_runner_bind.py` — Legacy host/rehearsal helper for a root-bound inherited descriptor; managed split containers do not invoke it.
- `entrypoint.sh` — Dispatches managed roles to the split launcher and retains one-shot/host legacy paths. Split images require an explicit websocket role. The Dockerfile fixes boot scripts at 0555 and config/hooks/scripts at 0555 directories / 0444 files independent of checkout modes. See `docs/ops/WORKER-CONTAINERS.md` for lifecycle, env/mount and upgrade contracts.
- `config/config.toml` — Kimi Code CLI default config (model, provider, thinking, fail-closed permission policy). Baked into the image. Default provider is the TensorZero gateway (function `tensorzero::function_name::FrontierCODEK`), not the managed api.kimi.com service — see README. Since WI#330 the baked `base_url` is an `.invalid` placeholder that `entrypoint.sh` refuses to run; mt-admin renders `KIMI_BASE_URL` per tenant (default: the tenant's own LLM endpoint).
- `hooks/headless_command_guard.mjs` — WI#206 fail-closed PreToolUse guard. The 2.0.2 behavior was live-verified; the real-CLI contract gate was re-run against 2.4.1 on 2026-11-02 (WI#531) and all four lanes pass, so the hook table and the `-p`/`--yolo` flag contract hold at the current pin; the protected worker image build remains the in-image proof. Print mode forces the auto policy, the upstream dangerous-command guard is skipped in auto mode, and `[[permission.rules]]` never load from config — this hook is the only in-CLI enforcement point for the headless lane.

## Protocol Compatibility

The `lunarwing-agent-v1` WebSocket subprotocol is shared with Pebble and OpenCode workers. The legacy `ironclaw-agent-v1` alias has been removed and is not accepted. Changes to `agent_comm_protocol.json` or envelope format must stay backward-compatible or be coordinated across all workers.

## Build Notes

The Dockerfile installs the official Kimi Code CLI with `npm ci` from `kimi-cli/package-lock.json`; update that lock and the `KIMI_CODE_VERSION` default together. The bridge uses `scripts/package-lock.json`. Base images, Debian package indexes, rustup, and Go archives are pinned, so refresh their recorded digests/checksums deliberately rather than restoring unlocked installers. The CLI install `RUN` ends with `touch -d "@${SOURCE_DATE_EPOCH}" /usr/local`: npm's global install does not deterministically modify that directory, and the resulting candidate-vs-rebuild mtime race flaked the CI reproducibility gate five times in October 2026 — keep the pin when editing the step.

Keep `ARG SOURCE_DATE_EPOCH=0` before the runtime `useradd`, not only before the CLI install. The same-layer assertion checks the account's `/etc/shadow` day against the source epoch so candidate/rebuild pairs spanning UTC midnight stay deterministic (WI#510). All four worker snapshots and `CI_DEBIAN_SNAPSHOT` move together; `20261105T082236Z` includes the Brotli OS-package fixes, not a fix for Node's separately bundled decoder (WI#514). Snapshot sources stay HTTPS, with `Acquire::Retries=0` and `Acquire::https::Timeout=60` on every `apt-get` call (WI#561); transient recovery belongs to bounded outer retries, not APT's automatic download retries. Keep both direct CA-bundle copies from the digest-pinned Python image before APT and the exact `2.6.2` installed-version assertions; the slim Node base may have no system trust store.

The in-image test gate runs `npm test` first, then re-runs just the real-CLI contract lane (`scripts/kimi_cli_contract_test.ts`) against the locked CLI — that lane self-skips on a host with no kimi binary, so the Dockerfile is the only place the flag contract (headless `-p` accepted, `--yolo` rejected alongside `-p`, and the baked `[[hooks]]` PreToolUse guard table valid) is asserted for real. Since MR !1549 (WI#324 EF-5) the lane also pins the MCP contract the tool-proxy shim relies on: the CLI loads `$KIMI_CODE_HOME/mcp.json`, spawns the stdio server with parent-env pass-through, and completes `initialize` → `tools/list` over NDJSON.

The in-image `npm test` runs as root inside rootless BuildKit, which grants no `CAP_SYS_PTRACE`. The root `worker-kimi-ts` lane passed such reads in master pipeline 6308 while the image build failed on them, so only the protected candidate build catches this. A root test therefore cannot read another uid's `/proc/<pid>/{exe,environ,fd}`. Tests that inspect the dropped runner read those entries through a capability-free probe on the runner's own uid (`runnerProc` in `entrypoint_config_test.ts` and `task_runner_client_test.ts`). The world-readable `/proc/<pid>/stat` and `status` need no probe. To reproduce the gate's shape on a host, run the suite under `podman unshare setpriv --bounding-set=-sys_ptrace --`.

## Do Not

- Break the `lunarwing-agent-v1` envelope format
- Store secrets in config files (use `KIMI_API_KEY` env var; entrypoint writes it to the runtime config)
- Remove the health server — orchestration depends on it
- Run `kimi` in interactive TUI mode inside the container — the worker uses `-p` (print mode) only
- Remove or weaken the fail-closed policy (WI#206): keep the `PreToolUse` Bash guard hook in `config.toml` + `hooks/headless_command_guard.mjs`, keep `dangerous_command_guard = true`, and never export `KIMI_CODE_DANGEROUS_COMMAND_GUARD` or `KIMI_CODE_TRUST_WORKSPACE` (the entrypoint unsets both and the executor scrubs both from per-task env; the latter is kimi-code 2.2.0's workspace-trust switch, which auto-spawns a repo's `.mcp.json` servers before any model turn, outside the hook — WI#411). The permission-mode reminder stays off (`KIMI_CODE_PERMISSION_MODE_REMINDER=0`) — it is prompt noise for a fixed-mode worker, and with the hook enforcing denials there is no approval stall to avoid.
- Let the worker self-expose (WI#357): this lane runs `kimi -p` per task and has no server mode, pairing, sharing, or tunnel; keep it that way, and keep `telemetry = false` in `config/config.toml`. Workers must never open their own inbound path or publish session content — see `docs/ops/WORKER-CONTAINERS.md` → "Worker exposure policy".
