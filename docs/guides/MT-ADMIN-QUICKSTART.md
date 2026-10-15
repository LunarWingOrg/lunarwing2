# Multi-Tenant LunarWing Quickstart

`ic/scripts/lunarwing-mt-admin.sh` is the supported lifecycle boundary for LunarWing tenants on both systemd-user and OpenRC hosts. Use it for provisioning, build, start/stop/restart, status, ports, backup/restore, upgrades, workers, service rendering, and host-health installation.

Do not substitute bare system-bus `systemctl` or hand-written `rc-service` commands for tenant lifecycle. On systemd hosts, tenant services run in the tenant account's user manager; on Gentoo they are OpenRC services managed through the same MT admin interface.

**Revalidated against source 2026-09-17 (v2.1.2.0); re-checked 2026-09-19 and 2026-09-27 (v2.1.3.0), 2026-10-01 (v2.1.4.0), 2026-10-04 (v2.1.5.0), and 2026-10-15 (v2.1.6.0).**
Every referenced mt-admin
subcommand and flag (`add-tenant` opt-ins, build/install-wasm, worker lifecycle,
`patch-env`, `render-units`, backup/restore, `upgrade-tenant`, `remove-tenant`,
`tokens`, WeeChat bootstrap recovery) was re-checked against
`ic/scripts/lunarwing-mt-admin.sh`. The 2026-09-19 repass added
the `opencode2` worker to the single-worker restart example. The 2026-09-27
repass added the v2.1.3.0 (WI#223) `build-sandbox-image` command and the
new-tenant `SANDBOX_IMAGE`/`SANDBOX_AUTO_PULL=false` defaults. The 2026-10-01
repass added the v2.1.4.0 (WI#268) read-only diagnostics verbs `psql` and
`daemon-pid` (see §8). The 2026-10-14 repass records the v2.1.6.0 (WI#330)
required tenant LLM endpoint in §3 and the (WI#376) OpenRC
`render-units --all-proxies` fleet migration command in §8; the 2026-10-15
re-check added the (WI#357) `worker-listeners` exposure audit to §8's
read-only inspection verbs. **Migration note
(WI#154/155):** `SANDBOX_NETWORK` defaults to `none` everywhere. Existing
explicit `proxy` settings are preserved (mt-admin renders `none` only when
unset). Tenants that need sandboxed web access must opt into `proxy-enforced`
(enforced; local Linux runtime + Python-3 image required) or the legacy
advisory `proxy`. See
[`../ops/history/v2.1.2.0_ITEM26_WI179_SANDBOX_HARDENING.md`](../ops/history/v2.1.2.0_ITEM26_WI179_SANDBOX_HARDENING.md).

For per-tenant instructions and identity editing, use the gateway's
**Settings → System Prompt** panel. See [Tenant System Prompts](TENANT-SYSTEM-PROMPTS.md)
for source files, preview semantics, and the Engine V2 restart/new-conversation
activation procedure.

## 1. Install Host Prerequisites

Required for the core MT flow:

- root or sudo access;
- Bash, coreutils, jq, git, curl, and Python 3;
- Docker or Podman with rootless support for tenant PostgreSQL and workers;
- PostgreSQL client utilities used by backup/restore checks;
- the distribution's supported init system: systemd or OpenRC.

Optional components add requirements:

- WeeChat, tmux, and Python `aiohttp` for the WeeChat relay/adapter;
- Tesseract/container support for LunarVision;
- the approved DarkFi build prerequisites for DarkIRC;
- worker-specific container images and credentials.

Use the current distribution guide:

- [`DEBIAN_PACKAGE_LIST.md`](DEBIAN_PACKAGE_LIST.md)
- [`UBUNTU_PACKAGE_LIST.md`](UBUNTU_PACKAGE_LIST.md)
- [`FEDORA_PACKAGE_LIST.md`](FEDORA_PACKAGE_LIST.md)
- [`ARCHLINUX_PACKAGE_LIST.md`](ARCHLINUX_PACKAGE_LIST.md)
- [`GENTOO_PACKAGE_LIST.md`](GENTOO_PACKAGE_LIST.md)

MT admin installs a tenant-local Rust toolchain when needed, so a host-level rustup installation is not required merely to add a tenant.

For Podman, configure approved short-name resolution before provisioning. The package guides describe the required registry configuration; do not accept an interactive image source during unattended setup.

## 2. Check the Host

From the repository root:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh doctor
```

Resolve failures for components you intend to use. A missing port registry before the first tenant is expected; MT admin creates it. Missing optional worker images are not core-daemon failures.

Do not install Python packages into an externally managed system interpreter with `--break-system-packages`. Use distribution packages or the component's virtual environment.

## 3. Add a Tenant

Run the script from the checkout and branch that the tenant should follow:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant ruffles --docker-group
```

Useful opt-ins and opt-outs include:

```bash
# Enable the per-tenant TensorZero proxy (upstream is REQUIRED — see below)
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant ruffles --enable-proxy \
  --tensorzero-url https://tensorzero.internal.example:7070

# Enable the per-tenant DarkIRC daemon and adapter
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant ruffles --enable-darkirc

# Seed the tenant's knowledge store with the full shared lw2studymcp
# study-notes base (one-shot, first boot only; read-only shared MCP included)
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant ruffles --with-study-mcp

# Skip automatic WeeChat relay bootstrap
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant ruffles --no-weechat-bootstrap
```

Since v2.1.6.0 (WI#330) `add-tenant` requires an explicit tenant LLM endpoint
and there is **no built-in default**: pass `--tensorzero-url <url>` (or set the
fleet `LUNARWING_MT_TENSORZERO_URL`) for the proxy path, or
`--llm-base-url <url>` (or set the fleet `LUNARWING_MT_LLM_BASE_URL`) for a
direct non-proxy endpoint. A missing endpoint
aborts provisioning before any tenant state exists; host-loopback and overlay
MagicDNS addresses are rejected for workers (they are unreachable from worker
containers), a rendered env never carries the baked `.invalid` placeholder
(mt-admin dies at render time when an enabled worker has no container-reachable
URL), and a hand-started container without `<WORKER>_BASE_URL` refuses to start
on the placeholder naming the environment variable to fix. `upgrade-tenant` backfills
an inheritable endpoint for every enabled worker before rebuilding, so managed
tenants do not need per-worker `configure-*` calls.

For knowledge-store seeding details (what is imported, the one-shot safety
invariants, opt-outs, and verification), see
[`TENANT_KNOWLEDGE_SEED.md`](TENANT_KNOWLEDGE_SEED.md).

Combine required flags in one reviewed command; examples above are illustrative, not instructions to create the same tenant repeatedly. Use `ic/scripts/create-tenant.sh` when the full convenience flow is desired; see [`CREATE_TENANT_NEW_SCRIPT.md`](CREATE_TENANT_NEW_SCRIPT.md).

Provisioning creates the OS account, protected tenant environment, PostgreSQL container, source checkout, state/log/run directories, port-registry entries, and init-appropriate service definitions. It does not justify reading or publishing the generated environment values.

## 4. Understand Ports

Each tenant receives a ten-port primary block in the `10000` range and a mirrored extended block in the `20000` range. The registry at `/etc/lunarwing/ports.json` is the source of truth.

Primary names currently cover gateway, HTTP webhook, XMPP bridge API, PostgreSQL, optional proxy, WeeChat relay, orchestrator, reserved legacy worker slots, Pebble WSS, and WeeChat adapter. Extended names cover additional workers, health listeners, LunarVision, DarkIRC, and other managed services.

Do not hardcode offsets in external health tooling. Query MT admin/registry names, especially `vision_service` and `vision_health` for OCR. Never force a multi-tenant LunarVision check to `127.0.0.1:8088`.

Inspect allocated tenants without exposing secrets:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh list-tenants
```

## 5. Build and Install

Long builds should run in `tmux`:

```bash
tmux new-session -s lunarwing-tenant-build
sudo ic/scripts/lunarwing-mt-admin.sh build-tenant ruffles --with-wasm
sudo ic/scripts/lunarwing-mt-admin.sh install-wasm ruffles
```

MT admin serializes tenant builds to avoid concurrent memory pressure. `--with-wasm` builds current registry channel/tool sources; `install-wasm` publishes the resulting artifacts and capability sidecars into the tenant state directories. On a fresh install, it assigns ownership to the tenant before patching WeeChat's generic capability ports to the allocated tenant ports; a separate `start-tenant` is not required for that patch.

Build optional shared components before starting tenants that require them:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh build-darkirc
sudo ic/scripts/lunarwing-mt-admin.sh build-pebble-worker
sudo ic/scripts/lunarwing-mt-admin.sh build-opencode-worker
sudo ic/scripts/lunarwing-mt-admin.sh build-opencode2-worker
sudo ic/scripts/lunarwing-mt-admin.sh build-kimi-worker
sudo ic/scripts/lunarwing-mt-admin.sh build-vision-sidecar
```

Since v2.1.3.0 (WI#223) new tenants default to `SANDBOX_IMAGE=lunarwing-sandbox:latest` with `SANDBOX_RUNTIME=podman` and `SANDBOX_AUTO_PULL=false`. `add-tenant` builds the lean generic sandbox image (from `ic/docker/sandbox.Dockerfile`) into the shared rootful store and distributes it into the tenant's rootless store automatically; if that step failed or the image was later removed, rebuild and redistribute it explicitly (optional tenant name selects that tenant's configured runtime):

```bash
sudo ic/scripts/lunarwing-mt-admin.sh build-sandbox-image [tenant]
```

`start-tenant` also reconciles a missing tenant-side sandbox image as a safety net. See [`../ops/TROUBLESHOOTING-HALF-BROKEN-TENANT-UPGRADE.md`](../ops/TROUBLESHOOTING-HALF-BROKEN-TENANT-UPGRADE.md) for the three-store diagnosis when sandboxed shell executions fail after an upgrade.

On Podman hosts, the **admin-store image build** uses `--network=host` so its Debian package installation can resolve DNS even where Podman's default build bridge cannot. This applies only to the trusted, repository-owned Dockerfile during image construction; it does not change tenant sandbox execution networking (`SANDBOX_NETWORK`) or the Docker fallback build.

Run only the builds selected for the deployment. Nanocode is retired; its registry slots remain reserved for compatibility but it is not a runnable worker.

## 6. Start and Verify

```bash
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant ruffles
sudo ic/scripts/lunarwing-mt-admin.sh status ruffles
sudo ic/scripts/lunarwing-mt-admin.sh doctor
```

The exact service graph depends on selected features. Core LunarWing, PostgreSQL, XMPP, optional proxy, WeeChat, adapter, DarkIRC, LunarVision, orchestrator, and workers must not be summarized as a fixed service count.

On rootless-podman hosts, a tenant can start failing with `cannot re-exec process to join the existing user namespace` (WI#142 — stale userns pairing after tenant churn; see [`../bugs/BUG-workload-scope-podman-userns.md`](../bugs/BUG-workload-scope-podman-userns.md)). Repair through MT admin rather than manual `podman system migrate` as root:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh fix-podman-userns ruffles
```

The command probes first and is a no-op on a healthy pairing; on the join signature it stops the tenant, cleans stale `pause.pid` state, runs `podman system migrate` as the tenant user, and restarts with health-gated sidecar recreation.

If a component fails, keep using MT admin to inspect the tenant and then read the init-appropriate logs. Do not bypass a failed `start-tenant` by manually starting a partial OpenRC or systemd graph; either fix/disable the failing optional component through supported configuration or rerender/reconcile the tenant.

For an authenticated gateway login, obtain the tenant token in a private terminal:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh tokens ruffles
```

Do not capture that output in shared logs. The gateway binds to the tenant's registry `gateway` port on loopback by default. Use an SSH tunnel or an approved tunnel provider for remote access.

## 7. WeeChat Relay

Fresh tenants bootstrap WeeChat's version-4 `api` relay automatically when dependencies are present. The bootstrap:

- binds to the tenant's loopback `weechat` port;
- stores `${env:RELAY_PASSWORD}` literally in `relay.conf`;
- passes only the resolved relay password through protected `env/weechat.env`;
- preserves and refuses to overwrite a non-empty existing WeeChat configuration;
- renders the WeeChat and adapter dependency chain for systemd-user or OpenRC.

Recover a skipped/failed bootstrap with:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-weechat-relay ruffles
sudo ic/scripts/lunarwing-weechat-preflight.sh ruffles
sudo ic/scripts/lunarwing-mt-admin.sh restart-tenant ruffles
```

Rotate an already-configured relay password with
`sudo ic/scripts/lunarwing-mt-admin.sh rotate-relay-password ruffles`.
This briefly stops and restarts the running WeeChat, adapter, and daemon;
it preserves the literal environment expression in `relay.conf`. Run the
preflight afterward. The verb refuses mismatched existing env credentials.

For architecture, service details, and upgrades, read [`../ops/WEECHAT-SERVICES.md`](../ops/WEECHAT-SERVICES.md) and [`lunarwing_weechat_wss/README.md`](lunarwing_weechat_wss/README.md).

## 8. Day-to-Day Operations

```bash
sudo ic/scripts/lunarwing-mt-admin.sh status ruffles
sudo ic/scripts/lunarwing-mt-admin.sh doctor
sudo ic/scripts/lunarwing-mt-admin.sh restart-tenant ruffles
sudo ic/scripts/lunarwing-mt-admin.sh backup-tenant ruffles
sudo ic/scripts/lunarwing-mt-admin.sh render-units ruffles
sudo ic/scripts/lunarwing-mt-admin.sh patch-env ruffles
```

`render-units` reconciles generated systemd-user/OpenRC definitions after source updates. `patch-env` fills current managed values without licensing manual secret inspection.

On OpenRC hosts still carrying legacy per-tenant TensorZero proxy units (`/etc/init.d/lunarwing-proxy-<tenant>` rendered before WI#376), neutralize the whole fleet at once (v2.1.6.0, WI#376):

```bash
sudo ic/scripts/lunarwing-mt-admin.sh render-units --all-proxies
```

Until that sweep succeeds, every other mt-admin command on OpenRC except `help` and `upgrade-tenant` refuses up front (exit 125; `writers-active` exits 2 = unknown) and names this command; only a positively detected systemd host skips the file-only legacy-unit guard. The sweep is file-only (legacy units are archived under `$LUNARWING_MT_OPENRC_PROXY_BACKUP_DIR`; services are not restarted) and stops on an unregistered or symlinked proxy unit — move that file outside `init.d` by hand and retry. In the same maintenance window update `tensorzero-proxy-configurations/lunarwing-proxy.py` in every proxy tenant's checkout before anything restarts the tenant, then restart through mt-admin (see [`../ops/MULTITENANCY-PRODUCTION.md`](../ops/MULTITENANCY-PRODUCTION.md)).

Use the worker lifecycle commands for one worker rather than restarting the entire tenant when possible:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh restart-worker ruffles pebble
sudo ic/scripts/lunarwing-mt-admin.sh restart-worker ruffles opencode
sudo ic/scripts/lunarwing-mt-admin.sh restart-worker ruffles kimi
sudo ic/scripts/lunarwing-mt-admin.sh restart-worker ruffles opencode2
```

For init-agnostic diagnostics, prefer the read-only inspection verbs over raw `psql`/`ps`/`pgrep` against tenant state (WI#268):

```bash
sudo ic/scripts/lunarwing-mt-admin.sh psql ruffles -c 'SELECT ...'
sudo ic/scripts/lunarwing-mt-admin.sh daemon-pid ruffles
sudo ic/scripts/lunarwing-mt-admin.sh worker-listeners ruffles
```

`psql` runs inside the tenant's own PostgreSQL container and is for SELECTs and fingerprints only — database rows can be secret-bearing. `daemon-pid` prints the PID of the process that actually loaded the tenant env, validated as tenant-owned (on OpenRC it reads OpenRC's `child_pid` record, never the supervise-daemon pidfile); a non-zero exit means the PID could not be resolved and must be treated as *uninspected*, never as evidence. `worker-listeners` (v2.1.6.0, WI#357) is the read-only worker-exposure audit: it lists listening sockets inside the tenant's running worker containers from the host side and fails on any bind or published port mapping outside the loopback contract ("workers must never self-expose"; see [`../ops/WORKER-CONTAINERS.md`](../ops/WORKER-CONTAINERS.md)). The original operator procedure and Engine V2 prompt-overlay audit that motivated these verbs are in the [v2.1.4.0 audit record](../ops/history/v2.1.4.0_WORK_ITEM_268_PROMPT_OVERLAY_AUDIT.md).

## 9. Backup, Restore, Upgrade, and Remove

Create guarded tenant database backups through MT admin:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh backup-tenant ruffles
sudo ic/scripts/lunarwing-mt-admin.sh list-backups ruffles
```

Restore is destructive to the tenant database and requires the tenant daemon stopped plus explicit `--yes`:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh stop-tenant ruffles
sudo ic/scripts/lunarwing-mt-admin.sh restore-tenant ruffles /absolute/path/backup.dump --yes
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant ruffles
```

Use `upgrade-tenant` only with a reviewed target ref and a clean tenant checkout. Run it in `tmux`; it performs backup, fetch/checkout, build, service rendering, environment reconciliation, and restart under lifecycle locking. Every tenant cargo build runs `ic/scripts/vendor-artifact-guard.sh` first, which cleans vendored crates that were patched in place since the tenant's last build (cargo never re-checks vendored sources). The first guarded upgrade of an existing tenant therefore rebuilds rig-core once.

Removing a tenant without `--purge` retains recoverable data according to MT admin policy. `--purge` is irreversible:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh remove-tenant ruffles
# destructive; use only after backup and explicit review:
sudo ic/scripts/lunarwing-mt-admin.sh remove-tenant ruffles --purge
```

## 10. Production Checklist

- [ ] MT admin `doctor` passes for selected host components.
- [ ] MT admin `status <tenant>` reports the tenant service graph healthy.
- [ ] Protected environment and secret files retain restrictive ownership/modes.
- [ ] Gateway and webhook authentication are enabled.
- [ ] systemd-user or OpenRC service graph matches the host.
- [ ] PostgreSQL backup and restore were rehearsed.
- [ ] Worker, channel, tunnel, and OCR ports come from the registry.
- [ ] WeeChat relay and adapter pass preflight when enabled.
- [ ] DarkIRC contacts use the typed exchange/fingerprint workflow when enabled.
- [ ] Host health pipeline is installed through MT admin and discovers every tenant.
- [ ] [`TESTING_GUIDE.md`](TESTING_GUIDE.md) checks pass or have explicit reviewed deferrals.
