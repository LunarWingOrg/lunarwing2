# Spawn one live LunarWing tenant `luna1` (podman + OpenRC, Gentoo)

Grounded in `ic/scripts/lunarwing-mt-admin.sh` and `docs/ops/MULTITENANCY-PRODUCTION.md`. Gaps are flagged inline rather than guessed.

The whole flow is basically **four mt-admin subcommands**: `add-tenant` → `build-tenant` → `start-tenant` → `status`. Everything else is sanity-checking around them.

Run from repo root (`/home/eris/lunarwing`). Mutating subcommands require root, so they are shown with `sudo`. `status`, `tokens`, and a current-schema `list-tenants` are read-only; run `doctor` with `sudo` because its root prerequisite otherwise reports `[FAIL]`, and use `sudo list-tenants` when a registry migration may be required. Example tenant name is `luna1` — swap it everywhere.

---

## 0. Preconditions — pin selection, gate on `doctor`

There is **no `--init-system` flag**; selection is env-override + autodetect, so export both to make the run deterministic.

```bash
cd /home/eris/lunarwing

export LUNARWING_SERVICE_MANAGER=openrc      # systemd | openrc
export LUNARWING_CONTAINER_RUNTIME=podman    # docker | podman  (podman => rootless per-tenant)

ls -l /run/openrc/softlevel && command -v rc-service rc-update
command -v podman && podman info >/dev/null && echo "podman OK"
test -f /etc/lunarwing/ports.json && jq '.tenants' /etc/lunarwing/ports.json || echo "no registry yet (add-tenant creates it)"

# Dependency + health gate — clear every [FAIL] before step 1
sudo LUNARWING_SERVICE_MANAGER=openrc LUNARWING_CONTAINER_RUNTIME=podman \
  ic/scripts/lunarwing-mt-admin.sh doctor
```

## 1. Create the tenant

Creates the OS user, allocates the tenant's primary/extended/Kimi port ranges in
`/etc/lunarwing/ports.json`, clones the repo to `/home/luna1/lunarwing/ic`,
generates env, starts the per-tenant PostgreSQL container, and renders the
OpenRC units. This example also selects Pebble for the tenant:

```bash
sudo LUNARWING_SERVICE_MANAGER=openrc LUNARWING_CONTAINER_RUNTIME=podman \
  ic/scripts/lunarwing-mt-admin.sh add-tenant luna1 --docker-group --with-pebble
```

Important boolean flags include `--enable-proxy`, `--enable-darkirc`,
`--with-pebble`, `--with-opencode`, `--with-kimi`, `--no-health`, `--no-ssh`,
and `--no-weechat-bootstrap`. Value flags include `--xmpp-jid`,
`--xmpp-password`, `--llm-api-key`, `--llm-base-url`, `--llm-model`,
`--tensorzero-url`, `--gotify-url`, `--gateway-host`, and worker model/base-URL
settings. Use `lunarwing-mt-admin.sh help` for the current full list. Proxy,
DarkIRC, and all persistent workers default off; WeeChat defaults on, while
`--no-weechat-bootstrap` skips only relay configuration, not its lifecycle.

## 2. Build everything (binary + WASM + workers)

One canonical command. `build-tenant` is flock-serialized and builds in order:
`lunarwing` binary → XMPP bridge → requested WASM (built and installed with
`--with-wasm`) → requested shared worker images. Worker images are host-global,
but `start-tenant` launches one only when both its image exists and the tenant
selected it during step 1. **This is the slow first build** — if base images were
wiped by a `podman system reset`, Rust/Debian/Bun layers re-pull and recompile.

```bash
sudo LUNARWING_SERVICE_MANAGER=openrc LUNARWING_CONTAINER_RUNTIME=podman \
  ic/scripts/lunarwing-mt-admin.sh build-tenant luna1 --with-wasm --with-pebble
```

Standalone rebuilds or reinstalls, if needed:

```bash
sudo LUNARWING_CONTAINER_RUNTIME=podman ic/scripts/lunarwing-mt-admin.sh build-pebble-worker  # [--no-cache]
sudo ic/scripts/lunarwing-mt-admin.sh install-wasm luna1  # reinstall already-built WASM artifacts
```

Manual fallback ONLY where mt-admin doesn't reach (`--network=host --format docker` are podman-only):

```bash
# pebble image (Dockerfile in pebble4lunarwing/, context = repo root)
podman build --network=host --format docker -t lunarwing-worker-pebble:latest \
  -f /home/eris/lunarwing/pebble4lunarwing/Dockerfile /home/eris/lunarwing
```

### 2b. Only if using pebble — configure it before starting

`configure-pebble` writes `pebble.env` (mode 0600), read by the worker
container on next start, so this must precede `start-tenant`. The current command
accepts its API key on argv; use a private administrative shell and avoid shared
history or logs.

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-pebble luna1 --nanogpt-api-key <key>  # optional: --model openai/gpt-5.2
```

> **Supported persistent workers** are Pebble, OpenCode, and Kimi Code. Nanocode and Codex are retired.

## 3. Start + enable services (OpenRC)

`start-tenant` runs the sequence itself: it starts PostgreSQL and LunarVision,
requires LunarVision's internal `8089/health` probe to pass, then starts the core
OpenRC services and enabled sidecars under their dependency graph. LunarVision's
`lunarwing-vision-<tenant>` unit provides endpoint-aware status and its
`lunarwing-vision-<tenant>-sup` companion restarts a crashed rootless container.
The proxy and DarkIRC start only when selected; WeeChat starts when enabled.
After the daemon and SSH agent are ready, mt-admin starts each worker only when
it is selected for this tenant and its shared image exists. Services that start
successfully are added to the appropriate boot runlevel.

```bash
sudo LUNARWING_SERVICE_MANAGER=openrc LUNARWING_CONTAINER_RUNTIME=podman \
  ic/scripts/lunarwing-mt-admin.sh start-tenant luna1
```

Manual fallback (the boot-persistence part mt-admin already covers, if wiring a unit by hand):

```bash
rc-update add lunarwing-luna1 default
rc-service lunarwing-luna1 start
rc-service lunarwing-luna1 status
```

## 4. Verify it's live

```bash
sudo ic/scripts/lunarwing-mt-admin.sh status luna1        # ports + pg/worker container state + OpenRC service states
sudo ic/scripts/lunarwing-mt-admin.sh list-tenants        # all tenants + port allocations
sudo ic/scripts/lunarwing-mt-admin.sh tokens luna1        # gateway auth token for the web UI

rc-service lunarwing-luna1 status
rc-service lunarwing-pg-luna1 status
rc-service lunarwing-vision-luna1 status
rc-service lunarwing-vision-luna1-sup status

# Probe the dedicated LunarVision health publication from the registry.
vision_health_port="$(jq -r '.tenants.luna1.extended_ports.vision_health' /etc/lunarwing/ports.json)"
curl --fail "http://127.0.0.1:${vision_health_port}/health"

# First tenant on an empty registry => gateway port 10000, but read the real value from status/list-tenants
curl -sS -o /dev/null -w 'gateway HTTP %{http_code}\n' http://127.0.0.1:10000/

# Optional re-run of the dependency/health gate
sudo LUNARWING_SERVICE_MANAGER=openrc LUNARWING_CONTAINER_RUNTIME=podman \
  ic/scripts/lunarwing-mt-admin.sh doctor

tail -f /home/luna1/lunarwing/logs/lunarwing.log
```

> Any HTTP code (`200`/`401`) = gateway up; connection-refused = not live. Log in by pasting the `tokens luna1` value into `http://127.0.0.1:<gateway-port>`.

---

## Notes / placeholders

- **Tenant name** is lowercased and sanitized to `[a-z0-9-]`. Reserved prefixes `pg-*`, `proxy-*`, `nanocode-*`, `pebble-*`, `opencode-*`, `kimi-*`, and `weechat-*` are rejected. `nanocode-*` remains reserved for legacy-unit collision safety.
- **Ports** auto-allocate through `/etc/lunarwing/ports.json`. `nanocode_wss=base+7` remains a reserved legacy slot and is never reallocated; `pebble_wss=base+8` remains active. Always read actual numbers from `status`/`list-tenants`.
- **Base dir / layout** for `luna1`: home `/home/luna1`; LunarWing root `/home/luna1/lunarwing`; repo clone `/home/luna1/lunarwing/ic`; env dir `/home/luna1/lunarwing/env` (holds `lunarwing.env`, `pg.secret`, and `pebble.env` if configured); **state dir = `LUNARWING_BASE_DIR` = `/home/luna1/lunarwing/state`**; logs `/home/luna1/lunarwing/logs`; run/pids `/home/luna1/lunarwing/run`.
- **Init-system / runtime selection** — no `--init-system` flag. Selection is the env override `LUNARWING_SERVICE_MANAGER=openrc|systemd` (else autodetected: `/run/openrc/softlevel` ⇒ openrc) and `LUNARWING_CONTAINER_RUNTIME=podman|docker` (else autodetected). Exporting both, as in step 0, makes the run deterministic. With podman the script sets `MT_ROOTLESS=true`: all per-tenant pg/worker containers run as the tenant user against their own rootless store (override with `LUNARWING_MT_ROOTLESS` if ever needed). Per-tenant image cleanup later lives in `/home/luna1/.local/share/containers`, **not** root's store.
- **OpenRC vs systemd caveat** — on OpenRC the units are **system-level** init scripts in `/etc/init.d/` using `supervise-daemon` with `command_user=<tenant>`; logs go to `/home/luna1/lunarwing/logs/lunarwing.log` (tail it, there is no `journalctl --user`). Boot persistence is `rc-update add lunarwing-luna1 default` (already done by `start-tenant`). The systemd-only concepts in the docs — `~/.config/systemd/user/` units, `loginctl enable-linger`, podman Quadlet `.container` files — do **not** apply here. One overlap: `add-tenant` still calls `loginctl enable-linger luna1`, which works under elogind to keep the rootless runtime dir across reboots (not a systemd dependency).
- **TensorZero proxy** is opt-in with `--enable-proxy` and is **not** a container under OpenRC — when selected, it runs as a tenant-owned `python3` process under `supervise-daemon` from the tenant's own `tensorzero-proxy-configurations/lunarwing-proxy.py`. PostgreSQL, LunarVision, and selected workers are rootless Podman containers with matching `-sup` crash supervisors.
- **WeeChat / weechat-adapter** are optional; `start-tenant` skips them non-fatally if their deps are missing, and the main daemon does not depend on them.
- **`doctor`** returns non-zero if any check fails and includes a "root" assertion — run it via `sudo` so that check passes, and clear every `[FAIL]` before step 1.
