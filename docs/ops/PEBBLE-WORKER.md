# Pebble Worker — Operational Guide

The Pebble worker (`pebble4lunarwing/`) integrates the Pebble agentic coding harness as a LunarWing external worker. It speaks the `lunarwing-agent-v1` WebSocket protocol (the legacy `ironclaw-agent-v1` alias is no longer accepted) and spawns `pebble prompt --output-format ndjson` per task. Because the alias is gone, recreate/upgrade worker containers in the same maintenance window as the daemon (see the upgrade-ordering note in [`WORKER-CONTAINERS.md`](WORKER-CONTAINERS.md)).

## Workspace Confinement

The daemon-supplied `context.project_dir` is never used verbatim as the
subprocess working directory. Both the bridge and the executor resolve it
through `src/workspace_path.rs` (a port of the kimi worker's
`workspace_path.ts`):

- `~` and `~/...` map under `WORKSPACE_ROOT`; relative paths join it.
- The candidate is canonicalized through the filesystem, so `..` segments
  and symlinks are resolved before the check.
- A resolved path that escapes `WORKSPACE_ROOT` is rejected fail-closed:
  the bridge answers with a `task_result` error envelope
  (`Workspace path escapes WORKSPACE_ROOT: ...`) and `spawn_pebble`
  refuses to spawn (defense in depth).
- One task at a time owns a workspace subtree: a second task whose
  project dir overlaps (equal to, parent of, or child of) an in-flight
  task's workspace is rejected until the first finishes or is cancelled.
  This mirrors the kimi bridge's `activeWorkspaces` guard and applies
  across worker connections.

This matters most under the default `PEBBLE_PERMISSION_MODE=danger-full-access`,
where an unconfined `project_dir` would otherwise point the pebble CLI at
any host directory (the native-mode quick-start has no container boundary
at all).

## Single-Instance Setup

### Native (no Docker)

On the Linux dev VM, keep Cargo limited to six CPUs and six jobs:

```bash
cd pebble
taskset -c 0-5 cargo build --release -j6
cd ../pebble4lunarwing
taskset -c 0-5 cargo build --release -j6

PEBBLE_BIN=../pebble/target/release/pebble \
WORKSPACE_ROOT=/tmp/pebble-workspace \
./target/release/pebble4lunarwing
```

On platforms without `taskset`, omit only the `taskset -c 0-5` prefix and keep
`-j6`.

### Docker

```bash
cd pebble4lunarwing
docker compose build
docker compose up -d pebble-worker
```

### LunarWing Config

Add to `config.toml` under `LUNARWING_BASE_DIR`:

```toml
[[sandbox.external_workers]]
name = "pebble"
url = "ws://localhost:9090/ws/agent"
timeout_ms = 3600000
prompt_context = { max_messages = 12, max_chars_per_message = 2000, max_total_chars = 12000 }
```

`create_job(..., mode: "pebble", context_scope: "recent")` opts into scoped
user/assistant history. Default `none` preserves prompt-only behavior.

## Multi-Tenant Setup

### Select and Build

Worker selection is per tenant and defaults off. Select Pebble while provisioning,
or re-run the idempotent add command for an existing tenant:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant <name> --with-pebble
```

This flips `workers.pebble=true` in `/etc/lunarwing/ports.json`; building the
shared image alone does not select it. Run long worker-image builds in `tmux`:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh build-pebble-worker
```

### Port Allocation

`lunarwing-mt-admin.sh` initializes and migrates the ports registry before its
normal tenant operations. The historical v3 → v4 migration assigns
`pebble_wss` at offset +8; operators should not run `migrate-ports-v4.sh`
directly during routine provisioning.

### Start/Stop

Pebble starts and stops with the tenant. For worker-only lifecycle operations,
use the dedicated commands:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh start-worker <name> pebble
sudo ic/scripts/lunarwing-mt-admin.sh stop-worker <name> pebble
sudo ic/scripts/lunarwing-mt-admin.sh restart-worker <name> pebble
```

Since v2.1.1.0 (work item #128), a Pebble stop/restart shuts down the owning
systemd user unit first (with a container-stop fallback) and clears any
`StartLimitBurst` lockout before restarting, so a tenant bounce no longer
leaves the tenant without its Pebble worker.

### Per-Tenant Configuration

Use the supported configuration command; it writes the tenant-owned
`pebble.env` with mode `0600`:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-pebble <name> \
  --nanogpt-api-key '<key>' \
  --model openai/gpt-5.2
sudo ic/scripts/lunarwing-mt-admin.sh restart-worker <name> pebble
```

The current command accepts the NanoGPT key on argv, so run it only from a
private administrative shell and avoid recording the command in shared history
or logs.

### Status

```bash
sudo ic/scripts/lunarwing-mt-admin.sh status <name>
sudo ic/scripts/lunarwing-mt-admin.sh list-tenants
```

## Health Endpoints

- `GET /health` — `{"status":"ok","uptime_seconds":...,"worker_id":"...","version":"...","mode":"websocket"}`
- `GET /ready` — `{"ready":true,"connections":1}` (200) or `{"ready":false,"connections":0}` (503)

## Troubleshooting

### Worker not starting

Check the image exists:
```bash
docker image inspect lunarwing-worker-pebble:latest
```

If missing, build it:
```bash
sudo ic/scripts/lunarwing-mt-admin.sh build-pebble-worker
```

### Task failures

Start with the init-agnostic tenant status rather than calling Docker or Podman
directly:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh status <tenant>
```

Then inspect the service/container logs through the host's configured init and
container runtime. The bridge logs task start/completion and subprocess stderr.

### Agent can't connect

The same status command reports the allocated `pebble_wss` port and worker
state. Check that `config.toml` points at that port.
