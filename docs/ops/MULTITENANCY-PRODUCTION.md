# Production Multi-Tenant Deployment

This document describes the production multi-tenant system managed by `ic/scripts/lunarwing-mt-admin.sh`. Each tenant is a real OS user with its own repo clone, build artifacts, services, PostgreSQL container, TensorZero proxy, and XMPP bridge.

## Overview

The admin script runs as root and handles:

1. **OS user creation** with `loginctl enable-linger` (systemd) or system-level services with `User=` directives (OpenRC/Gentoo)
2. **Port allocation** from a centralized registry at `/etc/lunarwing/ports.json`
3. **Per-user repo clone** and sequential builds (flock-serialized to avoid OOM)
4. **Per-user PostgreSQL container** (Docker or Podman)
5. **Per-user TensorZero proxy** on its own port
6. **Per-user XMPP bridge** with its own JID
7. **Service unit generation** for systemd (user-level) or OpenRC (system-level)

> **Proxy opt-in (v1.1.9+):** The TensorZero proxy is opt-in as of v1.1.9. Use `--enable-proxy` on `add-tenant` to provision it. Without it, the daemon connects directly to the upstream LLM endpoint (`TENSORZERO_URL`). Existing tenants are unaffected until their units are re-rendered.

## Full Run Walkthrough

Step-by-step commands to set up a fresh multi-tenant deployment from scratch. Run all commands from the repo root (`/home/sun/lunarwing`).

### Step 1: Verify dependencies — and pick the container runtime

```bash
sudo ic/scripts/lunarwing-mt-admin.sh doctor
```

Fix any `[FAIL]` items before proceeding.

> **Boxes with both docker and podman installed:** auto-detection prefers
> docker, so pick your runtime explicitly ONCE before adding tenants — it is
> persisted machine-wide and no later command needs the env var:
>
> ```bash
> sudo env LUNARWING_CONTAINER_RUNTIME=podman ic/scripts/lunarwing-mt-admin.sh doctor
> ```
>
> Doctor's `[info] container runtime: podman (saved — /etc/lunarwing/container-runtime)`
> line confirms it stuck. A podman-only (or docker-only) box needs nothing —
> auto-detect resolves correctly. To change later, run any command with the
> env var again (it overwrites), or `sudo rm /etc/lunarwing/container-runtime`
> to return to auto-detect.

### Step 2: Add tenants

Add all tenants at once. Names are comma-separated and will be lowercased automatically.

```bash
sudo ic/scripts/lunarwing-mt-admin.sh add-tenants "Ruffles,Miyuki,Sparkie,Starforce" --docker-group
```

> **Optional services are explicit selections.** Add `--enable-proxy` or
> `--enable-darkirc` to provision those sidecars. Add `--with-pebble`,
> `--with-opencode`, or `--with-kimi` to select workers for these tenants.
> Building a shared worker image does not select that worker. Re-running
> `add-tenant <name>` with one of these flags enables it for an existing tenant;
> omitting a flag never disables an already-selected service. Selection is
> persisted in `/etc/lunarwing/ports.json`.

> **Knowledge seeding is a provisioning-time choice.** Add
> `--with-study-mcp` to wire the shared lw2studymcp Study Notes MCP read-only
> and arm the one-shot import of the entire shared study-notes base into each
> new tenant's private knowledge store (`KNOWLEDGE_SEED_FROM_STUDY_MCP=1`,
> first boot only). Tenants already running are never seeded or re-seeded,
> and `upgrade-tenant` never arms the flag. See
> [`../guides/TENANT_KNOWLEDGE_SEED.md`](../guides/TENANT_KNOWLEDGE_SEED.md).

This creates OS users, allocates port blocks, clones the repo, generates env files, starts PostgreSQL containers, and renders service units for each tenant.

### Step 3: Build binaries

Build all tenants sequentially (flock prevents concurrent builds to avoid OOM):

```bash
sudo ic/scripts/lunarwing-mt-admin.sh build-all
```

Or build one at a time:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh build-tenant ruffles
```

Include WASM extensions and/or shared worker images:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh build-all --with-wasm
sudo ic/scripts/lunarwing-mt-admin.sh build-all --with-pebble --with-opencode --with-kimi
```

Use `--with-toolchains` with the OpenCode or Kimi image flags when those workers
need Rust, Go, C/C++, Python, and native build tooling; their default images are
slim.

### Step 3b: Configure external workers (optional)

If using the Pebble worker, configure the NanoGPT API key for each selected tenant that needs it:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-pebble ruffles --nanogpt-api-key <key>
```

Optionally override the default model (`openai/gpt-5.2`):

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-pebble ruffles --nanogpt-api-key <key> --model openai/gpt-5.2
```

This creates `pebble.env` (mode 600) in the tenant's env directory. The worker container reads it on next start.

If using the OpenCode worker, optionally override the model and/or upstream base URL per tenant:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-opencode ruffles --model openai/gpt-5.2
sudo ic/scripts/lunarwing-mt-admin.sh configure-opencode ruffles --base-url http://127.0.0.1:<proxyport>/v1
```

This upserts `OPENCODE_MODEL`/`OPENCODE_BASE_URL` into the tenant's `lunarwing.env`. Optional Paseo MCP integration is available via `PASEO_URL`/`PASEO_TOKEN` in the env.

For Kimi Code, pass credentials through a root-readable file rather than the
process list:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-kimi ruffles \
  --model <model> --base-url <url> --api-key-file /root/kimi-api-key
```

This upserts `KIMI_MODEL`, `KIMI_BASE_URL`, and `KIMI_API_KEY` into the tenant's
mode-0600 `lunarwing.env`; restart that worker after changing its settings.

### Step 4: Start services

```bash
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant ruffles
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant miyuki
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant sparkie
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant starforce
```

> `start-tenant` also uploads the tenant's staged SSH key and bounces the
> daemon once so the SSH agent can sign immediately — no manual
> `restart-tenant` needed. It ends with an "SSH readiness" summary; if
> `keys_loaded` is 0 there, re-run `start-tenant`.

### Step 5: Verify

```bash
# List all tenants and their ports
sudo ic/scripts/lunarwing-mt-admin.sh list-tenants

# Check a specific tenant
sudo ic/scripts/lunarwing-mt-admin.sh status ruffles

# Get gateway auth tokens to log into the web UI
sudo ic/scripts/lunarwing-mt-admin.sh tokens
```

### Step 6: Access gateways

Gateways bind to `127.0.0.1` by default. From the same machine, open a browser to `http://127.0.0.1:<gateway-port>` and paste the token from step 5.

From a remote machine, use SSH port forwarding:

```bash
ssh -L 10000:127.0.0.1:10000 -L 10010:127.0.0.1:10010 \
    -L 10020:127.0.0.1:10020 -L 10030:127.0.0.1:10030 user@host
```

Then access `http://localhost:10000`, `http://localhost:10010`, etc.

### Step 7: Manage

```bash
# Stop a tenant
sudo ic/scripts/lunarwing-mt-admin.sh stop-tenant sparkie

# Restart a tenant
sudo ic/scripts/lunarwing-mt-admin.sh restart-tenant sparkie

# Rotate a tenant's PostgreSQL password (ALTER ROLE + pg.secret + DATABASE_URL; then restart)
sudo ic/scripts/lunarwing-mt-admin.sh rotate-pg-password sparkie

# Back up a tenant's DB (pg_dump custom format → $LUNARWING_MT_BACKUP_DIR/<name>/)
sudo ic/scripts/lunarwing-mt-admin.sh backup-tenant sparkie
sudo ic/scripts/lunarwing-mt-admin.sh backup-all
sudo ic/scripts/lunarwing-mt-admin.sh list-backups sparkie

# Restore a tenant's DB from a dump (DESTRUCTIVE: DROP+recreate; stop the daemon first)
sudo ic/scripts/lunarwing-mt-admin.sh stop-tenant sparkie
sudo ic/scripts/lunarwing-mt-admin.sh restore-tenant sparkie /var/lib/lunarwing-backups/sparkie/sparkie-<ts>.dump --yes
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant sparkie

# Remove a tenant (stop services, deallocate ports, preserve user home)
sudo ic/scripts/lunarwing-mt-admin.sh remove-tenant sparkie

# Remove a tenant completely (delete user and home directory)
sudo ic/scripts/lunarwing-mt-admin.sh remove-tenant sparkie --purge
```

### Viewing logs

Systemd:
```bash
# As the tenant user
journalctl --user -u lunarwing-ruffles.service -f

# As root
sudo -u ruffles XDG_RUNTIME_DIR=/run/user/$(id -u ruffles) journalctl --user -u lunarwing-ruffles.service -f
```

OpenRC:
```bash
tail -f /home/ruffles/lunarwing/logs/lunarwing.log
```

## Prerequisites

- Root or sudo access for the admin script
- Docker or Podman installed and running
- Rust toolchain (rustup, cargo) accessible to tenant users
- `jq` for port registry JSON operations
- Python 3 for the TensorZero proxy
- Git for repo cloning

Run `sudo scripts/lunarwing-mt-admin.sh doctor` to verify all dependencies.

## Quick Start

### Single tenant

```bash
cd /path/to/lunarwing/ic

# Add a tenant (creates user, allocates ports, clones repo, generates env, starts PG)
sudo scripts/lunarwing-mt-admin.sh add-tenant ruffles --docker-group

# Build binaries for the tenant (flock-serialized, OOM-safe)
sudo scripts/lunarwing-mt-admin.sh build-tenant ruffles

# Configure pebble worker (if using)
sudo scripts/lunarwing-mt-admin.sh configure-pebble ruffles --nanogpt-api-key <key>

# Start all services, including selected supported workers
sudo scripts/lunarwing-mt-admin.sh start-tenant ruffles

# Check status
sudo scripts/lunarwing-mt-admin.sh status ruffles

# View gateway auth token
sudo scripts/lunarwing-mt-admin.sh tokens ruffles
```

### Multiple tenants at once

```bash
# Add four tenants in one command
sudo scripts/lunarwing-mt-admin.sh add-tenants "Ruffles,Miyuki,Sparkie,Starforce" --docker-group

# Build all tenants sequentially
sudo scripts/lunarwing-mt-admin.sh build-all

# Start each tenant
sudo scripts/lunarwing-mt-admin.sh start-tenant ruffles
sudo scripts/lunarwing-mt-admin.sh start-tenant miyuki
sudo scripts/lunarwing-mt-admin.sh start-tenant sparkie
sudo scripts/lunarwing-mt-admin.sh start-tenant starforce

# List all tenants with their port allocations
sudo scripts/lunarwing-mt-admin.sh list-tenants
```

### Teardown

```bash
# Stop services (preserves data)
sudo scripts/lunarwing-mt-admin.sh stop-tenant ruffles

# Remove tenant entirely (stop services, deallocate ports, remove user + home dir)
sudo scripts/lunarwing-mt-admin.sh remove-tenant ruffles --purge
```

## Port Allocation

### Registry

The port registry lives at `/etc/lunarwing/ports.json` (root-owned, world-readable). It is written atomically (write to temp file, `mv` into place) to prevent corruption.

### Port blocks

Each tenant gets a contiguous block of 10 ports from the range `10000-19999`, supporting up to 1000 tenants, plus a mirrored **extended block** of 10 ports in the range `20000-29999` (`extended_base = base_port + 10000`), a two-port Kimi block in `30000-39999` (`kimi_base = base_port + 20000`), a two-port LunarVoice block in `40000-49999` (`voice_base = base_port + 30000`), and a two-port OpenCode 2 block in `51000-51999` (`opencode2_base = base_port + 41000`). The host-global `health_metrics` slot lives at `50000`.

Primary block (10000-19999):

| Offset | Service | Description |
|--------|---------|-------------|
| +0 | gateway | Web gateway (browser UI, WebSocket, REST API) |
| +1 | http | HTTP webhook endpoint |
| +2 | bridge | XMPP bridge HTTP API |
| +3 | postgres | PostgreSQL container port |
| +4 | proxy | TensorZero LLM proxy |
| +5 | weechat | WeeChat relay |
| +6 | orchestrator | Job orchestrator API |
| +7 | nanocode_wss | Reserved legacy Nanocode slot; never reallocated |
| +8 | pebble_wss | Pebble worker WebSocket |
| +9 | weechat_adapter | WeeChat WS adapter HTTP API |

Extended block (20000-29999):

| Offset | Service | Description |
|--------|---------|-------------|
| ebase+0 | darkirc_adapter | DarkIRC adapter HTTP |
| ebase+1 | darkirc_irc | DarkIRC IRC port |
| ebase+2 | darkirc_rpc | DarkIRC RPC port |
| ebase+3 | nanocode_health | Reserved legacy Nanocode slot; never reallocated |
| ebase+4 | pebble_health | Pebble worker `/health` (host self-heal) |
| ebase+5 | vision_service | LunarVision sidecar API (OCR port 8088) |
| ebase+6 | vision_health | LunarVision sidecar `/health` (port 8089) |
| ebase+7 | opencode_wss | OpenCode worker WebSocket |
| ebase+8 | opencode_health | OpenCode worker `/health` (host self-heal) |
| ebase+9 | coord_admin | Authenticated coordination admin API |

Kimi block (30000-39999):

| Offset | Service | Description |
|--------|---------|-------------|
| kbase+0 | kimi_wss | Kimi Code worker WebSocket |
| kbase+1 | kimi_health | Kimi Code worker `/health` (host self-heal) |

LunarVoice block (40000-49999):

| Offset | Service | Description |
|--------|---------|-------------|
| vbase+0 | voice_service | LunarVoice STT sidecar API |
| vbase+1 | voice_health | LunarVoice sidecar `/health` (host self-heal) |

OpenCode 2 block (51000-51999):

| Offset | Service | Description |
|--------|---------|-------------|
| obase+0 | opencode2_wss | OpenCode 2 worker WebSocket |
| obase+1 | opencode2_health | OpenCode 2 worker `/health` (host self-heal) |

The host-global `health_metrics` slot (50000) is not per-tenant.

### Example allocation

| Tenant | Base | Gateway | HTTP | Bridge | PG | Proxy | Orchestrator | Legacy reserved | Pebble | Kimi WSS |
|--------|------|---------|------|--------|----|-------|-------------|-----------------|--------|----------|
| ruffles | 10000 | 10000 | 10001 | 10002 | 10003 | 10004 | 10006 | 10007 | 10008 | 30000 |
| miyuki | 10010 | 10010 | 10011 | 10012 | 10013 | 10014 | 10016 | 10017 | 10018 | 30010 |

### Port collision warnings

The registry only tracks what mt-admin itself allocated, so a freshly allocated block can still collide with a listener mt-admin does not know about — a stale container from a removed tenant, a dev server, or any hand-started process bound to `127.0.0.1`. Since v2.0.9.0, `add-tenant` probes every freshly assigned port (primary, extended, Kimi, Voice, and OpenCode 2 blocks) after allocation and prints a non-fatal `WARNING: port collision ...` naming the affected `slot=port` pairs. Heed it: the tenant's units will otherwise fail later with `EADDRINUSE`.

Independently, `doctor` fails the `no duplicate ports in registry` check when any port number is assigned to more than one slot across the registry (including the host-global `health_metrics` slot), which catches hand-edited or badly migrated registries. The duplicate list is also available directly:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh doctor    # includes the duplicate-port check
```

### Registry schema

The current schema is v14. In addition to ports and feature selections, it gives
every DarkIRC-enabled tenant a stable, unique, non-secret `darkirc_scope_id`.

```json
{
  "version": 14,
  "range": { "start": 10000, "end": 19999 },
  "block_size": 10,
  "extended_range": { "start": 20000, "end": 29999 },
  "extended_block_size": 10,
  "kimi_range": { "start": 30000, "end": 39999 },
  "kimi_block_size": 2,
  "tenants": {
    "ruffles": {
      "base_port": 10000,
      "user": "ruffles",
      "created_at": "2026-08-05T00:00:00Z",
      "enable_darkirc": false,
      "darkirc_scope_id": null,
      "enable_proxy": false,
      "enable_weechat": true,
      "workers": {
        "nanocode": false,
        "pebble": true,
        "opencode": false,
        "kimi": false
      },
      "ports": {
        "gateway": 10000,
        "http": 10001,
        "bridge": 10002,
        "postgres": 10003,
        "proxy": 10004,
        "weechat": 10005,
        "orchestrator": 10006,
        "nanocode_wss": 10007,
        "pebble_wss": 10008,
        "weechat_adapter": 10009
      },
      "extended_base": 20000,
      "extended_ports": {
        "darkirc_adapter": 20000,
        "darkirc_irc": 20001,
        "darkirc_rpc": 20002,
        "nanocode_health": 20003,
        "pebble_health": 20004,
        "vision_service": 20005,
        "vision_health": 20006,
        "opencode_wss": 20007,
        "opencode_health": 20008,
        "coord_admin": 20009
      },
      "kimi_base": 30000,
      "kimi_ports": {
        "kimi_wss": 30000,
        "kimi_health": 30001
      }
    }
  }
}
```

## Per-Tenant Directory Layout

```
/home/<tenant>/lunarwing/
  ic/                            # Git clone of the core daemon
    target/<profile>/lunarwing   # Built main binary
    bridges/xmpp-bridge/
      target/<profile>/xmpp-bridge
  env/
    lunarwing.env                # Main daemon and worker settings (mode 0600)
    pg.secret                    # Stable per-tenant PostgreSQL password (mode 0600)
    xmpp-bridge.env              # Bridge env (mode 0600)
    weechat.env                  # Minimal relay credential env (mode 0600)
    proxy.env                    # Present only when the proxy is enabled
    vision.env                   # LunarVision sidecar env
    darkirc-adapter.env          # Present only when DarkIRC is enabled
    pebble.env                   # Pebble worker env (optional, mode 0600)
    nanocode.env.retired*        # Archived legacy Nanocode env, when present
  state/                         # LUNARWING_BASE_DIR
    channels/                    # WASM channel artifacts
    tools/                       # WASM tool artifacts
    xmpp/                        # XMPP/OMEMO state
    darkirc/                     # DarkIRC state when enabled
  nanocode-workspace/            # Preserved legacy data; never deleted automatically
  pebble-workspace/              # Pebble worker task workspace (if selected)
  opencode-workspace/            # OpenCode worker task workspace (if selected)
  kimi-workspace/                # Kimi Code worker task workspace (if selected)
  logs/                          # OpenRC logs
  run/                           # PID files and sockets
```

## Services Per Tenant

The unit set is feature- and runtime-dependent; there is no fixed per-tenant
unit count. Always use `lunarwing-mt-admin.sh status <name>` for the authoritative
view rather than assuming every optional unit exists.

### Systemd (user-level with linger)

Units are installed under `~/.config/systemd/user/` and run on the tenant's user
manager. `loginctl enable-linger` keeps them running after logout.

| Unit | When present |
|------|--------------|
| `lunarwing-<name>.service` | Always; main daemon |
| `xmpp-bridge-<name>.service` | Always; `PartOf` the main daemon |
| `lunarwing-weechat-<name>.service` and `lunarwing-weechat-adapter-<name>.service` | WeeChat enabled (the default) |
| `lunarwing-proxy-<name>.service` | `--enable-proxy` selected |
| `lunarwing-darkirc-<name>.service` and `lunarwing-darkirc-adapter-<name>.service` | `--enable-darkirc` selected |
| `lunarwing-pg-<name>.service`, `lunarwing-vision-<name>.service`, and selected worker units | Rootless Podman Quadlets after their containers are provisioned; LunarVision uses a managed `podman wait` user-unit fallback when the installed Podman is too old for the emitted Quadlet contract |
| `lunarwing-voice-<name>.service` | LunarVoice activated (`install-voice` + `activate-voice`); rootless Podman Quadlet with the same managed fallback as LunarVision |
| `lunarwing-workloads.slice` and `lunarwing-build.slice` | Always; resource bounds for agent and operator workloads |

LunarVision publishes the registry `vision_service` and `vision_health` ports to
its internal `8088` API and `8089` operational listeners. `start-tenant` requires
the internal `/health` probe to pass and force-recreates a stale container when
its image, environment, ports, runtime, or init contract changes.

The main unit pulls in only sidecars that are enabled. Admin tooling always
addresses tenant units through that user's bus:

```bash
sudo -u <tenant> XDG_RUNTIME_DIR=/run/user/<uid> systemctl --user <command> <unit>
```

A tenant can manage its own user units directly:

```bash
systemctl --user status lunarwing-<name>.service
journalctl --user -u lunarwing-<name>.service -f
```

### OpenRC (system-level with tenant execution)

OpenRC scripts live in `/etc/init.d/`; the baseline is
`lunarwing-<name>`, `xmpp-bridge-<name>`, and `lunarwing-pg-<name>`.
WeeChat and its adapter are present by default. Proxy and DarkIRC scripts are
rendered only when selected, while LunarVision, LunarVoice, and selected
Pebble/OpenCode/Kimi workers get container services when provisioned. Rootless
Podman PostgreSQL, LunarVision, LunarVoice, and worker containers also get
`<container>-sup` babysitter services. For LunarVision,
`lunarwing-vision-<name>` provides health-aware status and
`lunarwing-vision-<name>-sup` restarts a crashed container within seconds; the
host self-heal pipeline remains the bounded backstop. LunarVoice follows the
same pattern: `lunarwing-voice-<name>` pairs with a `-sup` crash supervisor.

The process services use `supervise-daemon` with the tenant identity. Dependency
wiring starts PostgreSQL and enabled sidecars before the main daemon. The main
daemon, XMPP bridge, proxy, WeeChat client, and WeeChat/DarkIRC adapters do not
source tenant-owned environment files from root-side hooks. Unit rendering
installs `/usr/local/libexec/lunarwing-openrc-env-exec`; after dropping to the
tenant, that root-owned launcher validates and literally parses `KEY=value`
files. It fails closed unless the env directory is tenant-owned mode `0700` and
the env file is a non-symlink, single-link, tenant-owned regular file mode
`0600`.

Existing OpenRC tenants adopt this boundary after `render-units <name>` followed
by `restart-tenant <name>`. Check ownership and modes before restarting.

```bash
rc-service lunarwing-<name> start
rc-service lunarwing-<name> status
```

## PostgreSQL

Each tenant gets its own Docker/Podman container named `lunarwing-pg-<name>`, bound to `127.0.0.1:<allocated-port>:5432`. The user and database are both `lunarwing`; the **password is a per-tenant random hex string** generated at `add-tenant` time, stored in `…/env/pg.secret` (0600, tenant-owned) and woven into the daemon's `DATABASE_URL`. Tenants created before this change keep their original `lunarwing` password until rotated — run `lunarwing-mt-admin.sh rotate-pg-password <name>` to move them onto a random one (it runs `ALTER ROLE`, updates `pg.secret` + `DATABASE_URL`, and prompts for a restart).

The container is created with `--restart unless-stopped` so it survives host reboots when using Docker. Podman has no daemon to honor that policy, so each tenant's container gets a first-class **supervised unit** instead:

- **OpenRC** — a dedicated `/etc/init.d/lunarwing-pg-<name>` service (which the main daemon `need`s) `podman start`s the container and waits for `pg_isready`; supported workers get `/etc/init.d/lunarwing-{pebble,opencode,kimi}-<name>` the same way. See [`MT-GENTOO-SETUP-AND-CHANGES-MADE.md`](MT-GENTOO-SETUP-AND-CHANGES-MADE.md).
- **Podman + systemd (rootless)** — a per-tenant **Quadlet** `.container` at `~/.config/containers/systemd/lunarwing-pg-<name>.container` (`Restart=on-failure`, `HealthCmd=pg_isready`), which the podman user-generator turns into `lunarwing-pg-<name>.service` at `daemon-reload`. This supersedes the older `podman generate systemd` approach. See the archived [`MT_SYSTEMD_PARITY.md`](../internal/history/proposals/MT_SYSTEMD_PARITY.md) implementation plan.

## Container Runtime

The script supports both Docker and Podman. Detection priority:

1. `LUNARWING_CONTAINER_RUNTIME` env var override (`docker` or `podman`) — persisted machine-wide on use
2. Saved choice in `/etc/lunarwing/container-runtime`
3. If only Podman is installed, use Podman
4. Otherwise default to Docker

All container operations use the detected runtime — no Docker-specific commands are hardcoded.

## Build Serialization

Only one tenant builds at a time, enforced by an flock on `/var/lock/lunarwing-build.lock`. This prevents OOM on memory-constrained hosts when multiple tenants need to compile Rust.

```bash
# Build one tenant
sudo scripts/lunarwing-mt-admin.sh build-tenant ruffles

# Build all tenants sequentially
sudo scripts/lunarwing-mt-admin.sh build-all

# Include WASM extensions
sudo scripts/lunarwing-mt-admin.sh build-all --with-wasm

# Include worker images
sudo scripts/lunarwing-mt-admin.sh build-all --with-pebble --with-opencode --with-kimi

# Build worker images standalone
sudo scripts/lunarwing-mt-admin.sh build-pebble-worker
sudo scripts/lunarwing-mt-admin.sh build-opencode-worker
sudo scripts/lunarwing-mt-admin.sh build-kimi-worker
```

## Security Considerations

- Env files are mode `0600`, owned by the tenant user
- Gateway tokens are auto-generated (64-char hex) per tenant
- Secrets master keys are unique per tenant
- Each tenant runs in its own user scope (cgroup on systemd)
- PostgreSQL containers bind to `127.0.0.1` only
- All HTTP services bind to `127.0.0.1` by default
- Systemd units include `NoNewPrivileges=true` and `PrivateTmp=true`

## Environment Variables

### Tenant `lunarwing.env`

| Variable | Default | Description |
|----------|---------|-------------|
| `SSH_API_TOKEN` | random per-tenant token | Dedicated bearer credential for every SSH host/key API route. Missing values leave the SSH API unmounted. `add-tenant` writes it; `configure-ssh` and staged-key startup backfill it atomically for older tenants without rotating an existing token. |

### Host/operator environment

| Variable | Default | Description |
|----------|---------|-------------|
| `LUNARWING_SERVICE_MANAGER` | auto-detect | Force `systemd` or `openrc` |
| `LUNARWING_CONTAINER_RUNTIME` | saved choice, else auto-detect | Force `docker` or `podman`; an explicit choice is persisted in `/etc/lunarwing/container-runtime` |
| `LUNARWING_MT_PROFILE` | `release` | Build profile. Keep `release` for production and routine verification; do not use full debug builds. |
| `LUNARWING_MT_BUILD_JOBS` | `$(nproc)` | Explicit Cargo job cap for tenant builds |
| `LUNARWING_MT_SOURCE_REPO` | repository root | Source checkout cloned for each tenant |
| `LUNARWING_MT_TENSORZERO_URL` | `http://192.168.1.157:3000/openai/v1` | Upstream LLM URL and proxy upstream default |
| `LUNARWING_MT_LLM_BASE_URL` | empty | Fleet default `LLM_BASE_URL`; empty selects the local proxy when enabled, otherwise `LUNARWING_MT_TENSORZERO_URL` |
| `LUNARWING_MT_BACKUP_DIR` | `/var/lib/lunarwing-backups` | PostgreSQL dump root |
| `LUNARWING_MT_BACKUP_KEEP` | `7` | Dumps retained per tenant; `0` keeps all |

## Command Reference

This is a compact operator reference, not the parser's exhaustive help. Run
`sudo ic/scripts/lunarwing-mt-admin.sh help` for every current flag and worker,
backup, migration, DarkIRC, and configuration subcommand.

```
add-tenant <name> [options]      Create user, allocate ports, clone repo,
                                 generate env, render and install services
add-tenants <names> [options]    Apply the same options to comma-separated names
  --docker-group                 Add user to docker/podman group
  --enable-proxy                 Provision the optional TensorZero proxy
  --enable-darkirc               Provision DarkIRC daemon and adapter
  --with-pebble                  Select Pebble for these tenants
  --with-opencode                Select OpenCode for these tenants
  --with-kimi                    Select Kimi Code for these tenants
  --xmpp-jid <jid>               XMPP JID for add-tenant
  --xmpp-domain <domain>         XMPP domain for add-tenants
  --llm-base-url <url>           Explicit daemon LLM endpoint
  --no-health                    Skip host health-pipeline installation
  --no-ssh                       Skip SSH harness provisioning
  --no-weechat-bootstrap         Skip relay auto-bootstrap

remove-tenant <name>             Stop services and deallocate ports
  --purge                        Also delete the OS user and home

build-tenant <name>              Build one tenant (flock-serialized)
build-all                        Build registered tenants sequentially
  --with-wasm                    Also build WASM extensions
  --require-wasm                 Fail rather than preserve stale WASM (build-tenant)
  --with-pebble                  Also build the shared Pebble image
  --with-opencode                Also build the shared OpenCode image
  --with-kimi                    Also build the shared Kimi Code image
  --with-toolchains              Use full toolchain OpenCode/Kimi images

build-pebble-worker              Build the Pebble image
build-opencode-worker            Build the OpenCode image
build-kimi-worker                Build the Kimi Code image
  --no-cache                     Force a clean image rebuild

configure-pebble <name>          Set Pebble API key/model
configure-opencode <name>        Set OpenCode model/base URL
configure-kimi <name>            Set Kimi model/base URL/API key file

start-tenant <name>              Start core and selected optional services
stop-tenant <name>               Stop all tenant services
restart-tenant <name>            Stop then start
start-worker <name> <worker>     Start one selected worker
stop-worker <name> <worker>      Gracefully stop one worker
restart-worker <name> <worker>   Restart one worker
kill-worker <name> <worker>      Emergency zero-grace worker stop
                                 workers: pebble, opencode, kimi
render-units <name>              Re-render units without restarting
patch-env <name>                 Add missing current environment values

backup-tenant <name>             Create a custom-format PostgreSQL dump
restore-tenant <name> <file>     Destructive restore; requires --yes
list-tenants                     Show all tenants with ports
status <name>                    Detailed init-agnostic tenant status
tokens [name]                    Print gateway auth tokens
doctor                           System dependency and health checks
```

On OpenRC, `stop-worker` and `kill-worker` persist operator intent and remove
both direct worker and `-sup` babysitter units from every runlevel. Later
`start-tenant` calls and health remediation leave that worker stopped until
`start-worker` (or `restart-worker`) restores desired-running state and boot
persistence. Unselected workers receive same stale-unit cleanup.

## Relationship to Test Harness

The test harness (`ic/scripts/lunarwing-xmpp-test-env.sh`) provides `mt-init`,
`mt-up`, `mt-verify`, and `mt-down` for ephemeral multi-tenancy testing under
`${LUNARWING_MT_ROOT:-/tmp/lunarwing-mt}-{a,b}`. It runs both tenants as the
current user. `mt-up` uses user systemd on systemd hosts, launchd on macOS, and
direct PID management on OpenRC or unknown init systems; it does **not** test
production OpenRC units.

The production admin script (`lunarwing-mt-admin.sh`) is for persistent deployments with proper OS-level isolation. Key differences:

| Aspect | Test harness | Production admin |
|--------|-------------|-----------------|
| Users | Current user only | Dedicated OS user per tenant |
| State | `${LUNARWING_MT_ROOT}-{a,b}` | `/home/<tenant>/lunarwing/` |
| Services | User systemd, launchd, or direct fallback | User systemd with linger or system OpenRC |
| Ports | Fixed A/B offsets | Registry-allocated blocks |
| Build | Shared build artifacts | Per-user builds, flock-serialized |
| PostgreSQL | One isolated container per harness tenant | Per-tenant containers |
| Lifecycle | Ephemeral (test and discard) | Persistent (survives reboot) |

## Health Checks

The host-global infrastructure health suite (`ic-infrastructure-health-check/`)
auto-detects the init system and checks every registered tenant:

- **systemd**: `health-systemd.sh` discovers tenant user managers and checks
  their units, restart counts, and timer metadata.
- **OpenRC**: `health-openrc.sh` discovers `/etc/init.d/lunarwing-*` and
  `xmpp-bridge-*`, which includes core, sidecar, worker, and babysitter units.

Registry feature flags keep intentionally disabled WeeChat, proxy, DarkIRC, and
worker services classified as `skipped`; retired Nanocode units remain visible
but cannot degrade host health. LunarVision health ports are discovered per
tenant from `vision_health` in `/etc/lunarwing/ports.json`, so multi-tenant hosts
must not force any single-node target. Without registry targets the dedicated
single-node operational listener is `127.0.0.1:8089`; `8088` is the API listener.
`SERVICES="svc1 svc2"` remains an intentional OpenRC override.

`upgrade-tenant` refreshes the host-global health/self-heal scripts from the
current mt-admin tree when the managed health launcher is installed. A removed
pipeline leaves its old script copy on disk; that copy alone does not cause an
upgrade to reinstall or schedule it. The existing `health.env` retains operator
settings (the installer only reconciles its managed init-system and registry
paths). `LUNARWING_MT_HEALTH_ENABLED=false` disables new auto-installs, not
refreshes of an already-installed host-global pipeline.

Force init detection only when necessary with
`LUNARWING_SERVICE_MANAGER=systemd` or `LUNARWING_SERVICE_MANAGER=openrc`.

## Routine System Improvements

All tenants benefit from the routine resilience features:

- **Native retry with backoff**: When a routine fails with a retryable error (LLM timeout, empty response, execution timeout), it is automatically retried with exponential backoff. Per-routine `RetryPolicy` defaults: 3 retries, 60s initial delay, 2x backoff, 1h max delay. Retries use the existing `next_fire_at` column and cron ticker — no extra infrastructure. Non-retryable errors (auth, config, DB) skip retry entirely.
- **Lightweight timeout**: Lightweight routine executions are wrapped in `tokio::time::timeout` (default 300s, configurable via `ROUTINES_LIGHTWEIGHT_TIMEOUT_SECS`)
- **Stuck-run sweeper**: On every cron tick, the engine sweeps lightweight runs stuck in `running` beyond the timeout threshold and marks them as failed, unblocking the routine for future fires
- **FullJob crash recovery**: `sync_dispatched_runs()` recovers orphaned full-job runs from previous process crashes

These mechanisms prevent a single failed routine from permanently blocking itself due to a stale `running` status in the database.

## Troubleshooting

### Services don't start after reboot

- systemd: Verify linger is enabled: `loginctl show-user <tenant> | grep Linger`
- OpenRC: Add services to default runlevel: `rc-update add lunarwing-<name> default`

### PostgreSQL container won't start

- Check if the port is already in use: `ss -tlnp | grep <port>`
- Check container logs: `docker logs lunarwing-pg-<name>` or `podman logs lunarwing-pg-<name>`

### Build fails with OOM

- The flock on `/var/lock/lunarwing-build.lock` ensures only one tenant build at a time.
- Keep `LUNARWING_MT_PROFILE=release`; do not switch to a full debug build.
- Set `LUNARWING_MT_BUILD_JOBS=6` or lower to cap tenant Cargo jobs, and increase swap if the host still runs out of memory.

### Tenant user can't access Docker

- Verify group membership: `id <tenant>`
- User may need to log out and back in after being added to the docker group
- For Podman rootless, ensure `subuid`/`subgid` ranges are allocated: `grep <tenant> /etc/subuid`

### Gateway not accessible from network

- Gateways bind to `127.0.0.1` by default
- Use SSH port forwarding: `ssh -L <port>:127.0.0.1:<port> <host>`
- Or edit the tenant's `lunarwing.env` to set `GATEWAY_HOST=0.0.0.0` and restart
