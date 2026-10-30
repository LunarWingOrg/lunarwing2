# Agent Rules — pebble4lunarwing

## What This Is

Persistent container worker for upstream Pebble (nanogpt-community/pebble). The WebSocket bridge is Rust and execs the pinned `pebble` binary as a subprocess per task (a fresh `pebble prompt` for CLI mode, or the task-runner protocol for the split-container lane).

## Key Files

- `src/bridge.rs` — WebSocket bridge and hub protocol routing. Changes here affect protocol behavior.
- `src/executor.rs` — Task execution: spawns the `pebble` process with the WI#324 proxy env (`LUNARWING_TASK_ID` / `LUNARWING_TOOL_PROXY_SOCKET` injected after task-supplied env) and the reserved keys stripped from inherited and task-supplied env.
- `src/tool_proxy.rs` — WI#324 worker-side relay: per-task unix socket that turns the in-task shim's requests into `tool_call`/`tool_list` hub frames and routes replies back by call id.
- `src/mcp_shim.rs` — WI#324 thin stdio MCP server exposed as `pebble4lunarwing mcp-shim`. Pebble spawns it from `$PEBBLE_CONFIG_HOME/settings.json`; advertises zero tools without the proxy env, and the hub stays deny-by-default without a `tool_allowlist` grant.
- `src/task_runner.rs` / `src/workspace_path.rs` — Task-container runner and workspace-path confinement. The latter carries the FIFO/O_DIRECTORY and transient-classification pins that gate the image build, not just host runs.
- `entrypoint.sh` / `entrypoint-split.sh` — WI#454 managed `split-bridge` / `split-task` launcher. The image defaults to `USER 10004:10004` (the task identity); mt-admin overrides the bridge container to `10104:10004` and the task runner to `10004:10004`, both with no capabilities and no new privileges (WI#400). The `split-bridge` / `split-task` roles dispatch to `entrypoint-split.sh` before the legacy websocket root check; that check (`PEBBLE_MODE=websocket` + non-`inherit` `PEBBLE_TASK_UID` + non-root) only guards the un-split legacy shape, whose comment still names the pre-WI#454 `USER 0:0`. The hub credential is moved out of the environment into a root/bridge-only `AGENT_AUTH_TOKEN_FILE` before any long-lived process, and CLI-mode prompts drop to the task uid via setpriv. See `docs/ops/WORKER-CONTAINERS.md`.
- `config/settings.json` — Baked MCP server registration installed to root-owned `/etc/lunarwing/pebble/settings.json` (0444 in a 0555 directory). Project-scope `.pebble/settings.json` can shadow or disable it; the hub stays authoritative.
- `upstream/Cargo.lock` — Pinned upstream lock, held to the Dockerfile's `PEBBLE_LOCK_SHA256` ARG via `scripts/verify_upstream_lock.py`.

## Protocol Compatibility

The `lunarwing-agent-v1` WebSocket subprotocol is shared with Kimi Code and OpenCode workers. The legacy `ironclaw-agent-v1` alias has been removed and is not accepted. Changes to the envelope format must stay backward-compatible or be coordinated across all workers (spec: `../opencode4lunarwing/agent_comm_protocol.json`).

## Build Notes

Base images, the Debian snapshot, `PEBBLE_REF`, `SOURCE_REVISION`, rustup and the Go archive are pinned; refresh their recorded digests/checksums deliberately rather than restoring tag-only or fallback installs. `upstream/Cargo.lock` is held to `PEBBLE_LOCK_SHA256`; update the reviewed digest together with any intentional lock change and re-run the verifier.

Keep `ARG SOURCE_DATE_EPOCH=0` before runtime account creation and preserve the same-layer `/etc/shadow` epoch-day assertion (WI#510): timestamp rewriting cannot fix a wall-clock day embedded in file contents. All four worker snapshots and `CI_DEBIAN_SNAPSHOT` move together; `20261024T011300Z` includes the Bookworm/Trixie Brotli OS-package fixes, not a fix for Node's separately bundled decoder (WI#514).

The candidate/uncached-clean-rebuild whole-image digest gate remains mandatory. A `RUN` in this Dockerfile must leave no per-build residue (no writes outside its trap-cleaned tree, no wall-clock or random content baked into a layer) and consume deterministic, pinned inputs. The `pebble self-update` PATH wrapper refuses the one self-exposure footgun and honours no environment override; the root-owned binary and parent directory prevent replacement (see `docs/ops/WORKER-CONTAINERS.md` → "Worker exposure policy (WI#357)").

## Do Not

- Break the `lunarwing-agent-v1` envelope format
- Store secrets in config files (use `AGENT_AUTH_TOKEN` / `AGENT_AUTH_TOKEN_FILE`; the entrypoint moves it to a root-only file and strips it from task env)
- Remove the health server — orchestration depends on it (`GET /health`, `GET /ready`)
- Let the worker self-expose (WI#357): this lane has no tunnel, relay, pairing, or public listener; keep it that way. Workers must never open their own inbound path or publish session content — see `docs/ops/WORKER-CONTAINERS.md` → "Worker exposure policy".
- Remove or weaken the `PEBBLE_LOCK_SHA256` build contract: it is the reviewed pin that ties the upstream lock to CI, and a mismatch must fail the build, not silently drift.
