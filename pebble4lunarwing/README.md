# pebble4lunarwing

WebSocket bridge that integrates the [Pebble](https://github.com/nanogpt-community/pebble) agentic coding harness as a LunarWing external worker.

## Quick Start

### Native

```bash
cargo build --release
PEBBLE_BIN=../pebble/target/release/pebble \
WORKSPACE_ROOT=/tmp/workspace \
./target/release/pebble4lunarwing
```

### Docker

```bash
docker compose build
docker compose up -d pebble-worker
```

The Dockerfile pins upstream Pebble to a reviewed commit and supplies a
reviewed `PEBBLE_LOCK_SHA256` default for `upstream/Cargo.lock`. Plain Docker
builds, both Compose services, mt-admin's Docker/Podman builders, and CI use
that same default without an extra argument. The build rejects a digest
mismatch and compares complete parsed package records — keyed by
`(name, version, source)` so multi-version crates are fully covered — against
the pinned upstream lock; only `rustls`, `rustls-webpki`, and `chacha20`
may differ (the first two for RUSTSEC-2026-0285, the last to retire upstream's
yanked 0.10.1 pin).
Package additions, removals, or source/checksum/dependency edits outside that
allowlist fail even when a caller supplies a matching digest override, and a
no-op override fails too, so the input is retired once upstream ships its own
fix. Update the reviewed digest together with any intentional lock change; CI
checks they match.

Fast regression checks (no image build):

```bash
# From the repository root, with Python >= 3.11 and PyYAML installed:
python3 .gitlab/scripts/validate_config.py
python3 -m unittest discover -s .gitlab/tests -p 'test_*.py'
bash ic/scripts/tests/test-mt-admin-pebble-build-inputs.sh
```

### CLI mode (one-shot)

```bash
docker compose --profile cli run pebble-cli prompt "hello world"
```

## LunarWing Configuration

Add to `config.toml` under `LUNARWING_BASE_DIR`:

```toml
[[sandbox.external_workers]]
name = "pebble"
url = "ws://localhost:9090/ws/agent"
timeout_ms = 3600000
prompt_context = { max_messages = 12, max_chars_per_message = 2000, max_total_chars = 12000 }
```

The agent logs `External workers configured: pebble` on startup.

## Usage

```
create_job(title: "Fix the tests", description: "Run cargo test and fix failures", mode: "pebble")
```

Add `context_scope: "recent"` to include bounded user/assistant messages from
current conversation. Default `none` sends no history. History is untrusted
reference JSON; task remains authoritative. Metadata, identity, environment,
system prompts, and tool results never enter worker prompt. Configured limits
cannot exceed 50 messages, 10,000 characters per message, or 50,000 total.

External jobs default to `wait=false`; use `job_events <id>` and `job_status <id>`
to monitor progress.

## Environment Variables

See `.env.example` for the full list with descriptions.

## Health Endpoints

- `GET /health` — Worker status, uptime, version
- `GET /ready` — WebSocket readiness and connection count

## Exposure Policy (WI#357)

Workers must never self-expose: no own tunnels, relays, pairing links, or
public listeners, and no publishing session content to third-party services.
LunarWing owns every public exposure path through the daemon's tunnel; the
only instruction channel into this worker is the daemon's job dispatch → the
bridge → pebble on loopback. Upstream pebble has no listener/pairing/sharing
surface; its one outbound footgun is `pebble self-update` (fetches GitHub
releases and replaces the binary). The image pins `USER 0:0` and every
spawned pebble process drops to the task uid via setpriv (WI#400 — see
"Task-uid split (WI#400)" in `docs/ops/WORKER-CONTAINERS.md`; the rootful
launch runs the whole container as the tenant uid, WI#422 — on that lane
the agent shares the bridge's uid and can still signal or trace the
bridge and read its environment; no split, by design), so the agent
no longer runs as container root and cannot replace the root-owned bridge
binary on the rootless lane — `self-update` remains an egress concern for
the (unprivileged) task uid. Keep the pinned `PEBBLE_LOCK_SHA256` build contract intact; the
real fix for the egress half is the deferred egress allowlist
(WI#357 control 1). Full policy, controls, and the per-bump checklist:
`docs/ops/WORKER-CONTAINERS.md` → "Worker exposure policy (WI#357)".

## Tool proxy (WI#324)

The bridge binary ships the worker-side half of the WI#324 tenant tool
proxy: a per-task unix-socket relay (`src/tool_proxy.rs`) plus a thin stdio
MCP server (`pebble4lunarwing mcp-shim`, `src/mcp_shim.rs`) that pebble
spawns (once for tool discovery, then once per tool call) from the baked
`mcpServers.lunarwing` entry in
`$PEBBLE_CONFIG_HOME/settings.json` (`/etc/lunarwing/pebble`; root-owned
0444 in a 0555 directory — with the WI#400 split, task code no longer
runs as container root, so the directory is no longer writable by it;
pebble's own state files in the same directory are the reason the config
home is otherwise read-only for the task uid).
The executor injects `LUNARWING_TASK_ID` / `LUNARWING_TOOL_PROXY_SOCKET`
into each pebble process (after task-supplied env, so they cannot be
shadowed; both keys are also stripped from inherited and task-supplied env
alongside `AGENT_AUTH_TOKEN`), and pebble passes its env through to the MCP
child. The shim translates MCP `tools/list` / `tools/call` into
`tool_list` / `tool_call` frames on the hub WebSocket; tools appear to the
agent as `mcp__lunarwing__<name>`. The hub stays deny-by-default until the
worker's `tool_allowlist` (or `cron.tool_allowlist` for tick jobs) grants
phase-1 names; without the proxy env the shim advertises zero tools.
Pebble merges project-scope `.pebble/settings.json` from the task
workspace, so workspace content can shadow or disable the `lunarwing`
entry — the hub stays authoritative either way. Design doc:
`docs/proposals/EXTERNAL-WORKER-TOOL-PROXY.md`.

## Protocol

Speaks the `lunarwing-agent-v1` WebSocket subprotocol (the legacy `ironclaw-agent-v1` alias is no longer accepted). See `../opencode4lunarwing/agent_comm_protocol.json` for the full spec.
