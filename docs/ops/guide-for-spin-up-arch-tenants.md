# Spawn one live LunarWing tenant `luna1` (podman + systemd, Arch Linux)

Grounded in `ic/scripts/lunarwing-mt-admin.sh` and `docs/ops/MULTITENANCY-PRODUCTION.md`. Sister document to [`guide-for-spin-up-gentoo-tenants.md`](guide-for-spin-up-gentoo-tenants.md) — same flow, systemd/Arch specifics swapped in. Gaps are flagged inline rather than guessed.

The whole flow is basically **four mt-admin subcommands**: `add-tenant` → `build-tenant` → `start-tenant` → `status`. Everything else is sanity-checking around them.

Run from repo root (`/home/eris/lunarwing`). Mutating subcommands require root, so they are shown with `sudo`. `status`, `tokens`, and a current-schema `list-tenants` are read-only; run `doctor` with `sudo` because its root prerequisite otherwise reports `[FAIL]`, and use `sudo list-tenants` when a registry migration may be required. Example tenant name is `luna1` — swap it everywhere.

---

## 0. Preconditions — packages, pin selection, gate on `doctor`

### 0a. Host packages (pacman)

Stock Arch is always systemd, so there is no init-system choice to make — only the container runtime. Everything `doctor` gates on maps to:

```bash
pacman -S --needed \
  podman passt fuse-overlayfs \
  jq git python rustup openssh curl \
  base-devel
```

- `podman` — container runtime. Rootless per-tenant containers need a userspace port-forwarder; `passt` (provides `pasta`) is the podman-5 default, `slirp4netns` is the accepted alternative. `fuse-overlayfs` is the recommended rootless storage driver.
- `shadow` (base) — provides the setuid `newuidmap`/`newgidmap` binaries and `usermod --add-subuids/--add-subgids` that rootless mode needs. `util-linux` (base) provides `flock` for the self-heal lock.
- `rustup` — doctor checks both `cargo` and `rustup`. After installing the package, materialize a toolchain: `rustup default stable`.
- `openssh` — doctor probes sshd on `127.0.0.1:22` (loopback SSH tenants): `systemctl enable --now sshd`.
- Tenant-side WASM toolchains are **not** host packages: `add-tenant` installs rustup + the `wasm32-wasip2` target into the tenant account itself.

Known first-run caveat: on a fresh Arch host `/etc/subuid` and `/etc/subgid` do not exist yet, so doctor's `rootless: /etc/sub{u,g}id populated` checks `[FAIL]` **before** the first tenant exists. That is expected — `add-tenant` allocates each tenant's range itself (appending after the current max). Optionally pre-seed with `touch /etc/subuid /etc/subgid` so `useradd` auto-allocates via `login.defs`; either way, re-run `doctor` after step 1 to confirm those gates clear.

### 0b. Pin init/runtime + doctor

There is **no `--init-system` flag**; selection is env-override + autodetect (`/run/systemd/system` ⇒ systemd), so export both to make the run deterministic.

```bash
cd /home/eris/lunarwing

export LUNARWING_SERVICE_MANAGER=systemd      # systemd | openrc
export LUNARWING_CONTAINER_RUNTIME=podman     # docker | podman  (podman => rootless per-tenant)

test -d /run/systemd/system && command -v systemctl loginctl
command -v podman && podman info >/dev/null && echo "podman OK"
test -f /etc/lunarwing/ports.json && jq '.tenants' /etc/lunarwing/ports.json || echo "no registry yet (add-tenant creates it)"

# Dependency + health gate — clear every [FAIL] before step 1
# (rootless subuid/subgid + per-tenant linger checks only pass after step 1)
sudo LUNARWING_SERVICE_MANAGER=systemd LUNARWING_CONTAINER_RUNTIME=podman \
  ic/scripts/lunarwing-mt-admin.sh doctor
```

## 1. Create the tenant

Creates the OS user (with `loginctl enable-linger`), allocates the tenant's
subuid/subgid ranges plus its primary/extended/Kimi port ranges in
`/etc/lunarwing/ports.json`, clones the repo to `/home/luna1/lunarwing/ic`,
generates env, starts the per-tenant PostgreSQL container, and renders the
systemd **user** units. This example also selects Pebble for the tenant:

```bash
sudo LUNARWING_SERVICE_MANAGER=systemd LUNARWING_CONTAINER_RUNTIME=podman \
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
sudo LUNARWING_SERVICE_MANAGER=systemd LUNARWING_CONTAINER_RUNTIME=podman \
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

## 3. Start + enable services (systemd user manager)

`start-tenant` runs the sequence itself: it starts the rootless PostgreSQL and
LunarVision Quadlet containers, requires LunarVision's internal `8089/health`
probe to pass (force-recreating the container when its image/env/port/runtime
contract drifted), then `daemon-reload`s the tenant's user manager and
enables + starts the user units: `lunarwing-luna1.service`,
`xmpp-bridge-luna1.service`, the WeeChat pair when enabled, and the proxy /
DarkIRC units only when selected. After the daemon and SSH agent are ready,
mt-admin starts each worker only when it is selected for this tenant and its
shared image exists.

```bash
sudo LUNARWING_SERVICE_MANAGER=systemd LUNARWING_CONTAINER_RUNTIME=podman \
  ic/scripts/lunarwing-mt-admin.sh start-tenant luna1
```

Boot persistence is `systemctl --user enable` (units are
`WantedBy=default.target`) plus linger keeping the user manager alive across
reboot — both already covered by `add-tenant`/`start-tenant`; there is no
`rc-update` equivalent.

Manual fallback — **always through the tenant's user bus, never the system bus**
(bare `systemctl` against tenant units false-fails healthy tenants):

```bash
uid="$(id -u luna1)"
sudo -u luna1 XDG_RUNTIME_DIR="/run/user/$uid" systemctl --user enable --now lunarwing-luna1.service
sudo -u luna1 XDG_RUNTIME_DIR="/run/user/$uid" systemctl --user status lunarwing-luna1.service
```

## 4. Verify it's live

```bash
sudo ic/scripts/lunarwing-mt-admin.sh status luna1        # ports + pg/worker container state + user-service states
sudo ic/scripts/lunarwing-mt-admin.sh list-tenants        # all tenants + port allocations
sudo ic/scripts/lunarwing-mt-admin.sh tokens luna1        # gateway auth token for the web UI

uid="$(id -u luna1)"
sudo -u luna1 XDG_RUNTIME_DIR="/run/user/$uid" systemctl --user is-active \
  lunarwing-luna1.service xmpp-bridge-luna1.service lunarwing-pg-luna1.service
sudo -u luna1 XDG_RUNTIME_DIR="/run/user/$uid" journalctl --user -u lunarwing-luna1.service -f

# Probe the dedicated LunarVision health publication from the registry.
vision_health_port="$(jq -r '.tenants.luna1.extended_ports.vision_health' /etc/lunarwing/ports.json)"
curl --fail "http://127.0.0.1:${vision_health_port}/health"

# First tenant on an empty registry => gateway port 10000, but read the real value from status/list-tenants
curl -sS -o /dev/null -w 'gateway HTTP %{http_code}\n' http://127.0.0.1:10000/

# Optional re-run of the dependency/health gate (subuid/linger gates now pass)
sudo LUNARWING_SERVICE_MANAGER=systemd LUNARWING_CONTAINER_RUNTIME=podman \
  ic/scripts/lunarwing-mt-admin.sh doctor

tail -f /home/luna1/lunarwing/logs/lunarwing.log
```

> Any HTTP code (`200`/`401`) = gateway up; connection-refused = not live. Log in by pasting the `tokens luna1` value into `http://127.0.0.1:<gateway-port>`.

---

## Notes / placeholders

- **Tenant name** is lowercased and sanitized to `[a-z0-9-]`. Reserved prefixes `pg-*`, `proxy-*`, `nanocode-*`, `pebble-*`, `opencode-*`, `kimi-*`, and `weechat-*` are rejected. `nanocode-*` remains reserved for legacy-unit collision safety.
- **Ports** auto-allocate through `/etc/lunarwing/ports.json`. `nanocode_wss=base+7` remains a reserved legacy slot and is never reallocated; `pebble_wss=base+8` remains active. Always read actual numbers from `status`/`list-tenants`.
- **Base dir / layout** for `luna1`: home `/home/luna1`; LunarWing root `/home/luna1/lunarwing`; repo clone `/home/luna1/lunarwing/ic`; env dir `/home/luna1/lunarwing/env` (holds `lunarwing.env`, `pg.secret`, and `pebble.env` if configured); **state dir = `LUNARWING_BASE_DIR` = `/home/luna1/lunarwing/state`**; logs `/home/luna1/lunarwing/logs`; run/pids `/home/luna1/lunarwing/run`.
- **Init-system / runtime selection** — no `--init-system` flag. Selection is the env override `LUNARWING_SERVICE_MANAGER=systemd|openrc` (else autodetected: `/run/systemd/system` ⇒ systemd) and `LUNARWING_CONTAINER_RUNTIME=podman|docker` (else autodetected; an explicit choice is persisted in `/etc/lunarwing/container-runtime`). Exporting both, as in step 0, makes the run deterministic. With podman the script sets `MT_ROOTLESS=true`: all per-tenant pg/worker containers run as the tenant user against their own rootless store (override with `LUNARWING_MT_ROOTLESS` if ever needed). Per-tenant image cleanup later lives in `/home/luna1/.local/share/containers`, **not** root's store.
- **systemd vs OpenRC caveat** — on Arch the units are **user-level** units in `/home/luna1/.config/systemd/user/`, running on the tenant's own `systemd --user` manager (`XDG_RUNTIME_DIR=/run/user/<uid>`); they are never system-bus units. Boot persistence is `systemctl --user enable` + `loginctl enable-linger luna1` (done by `add-tenant`), which also keeps `/run/user/<uid>` alive across reboots. Logs: `journalctl --user -u <unit>` (the file logs under `/home/luna1/lunarwing/logs/` also still exist). The OpenRC-only concepts in the sister guide — `/etc/init.d/` scripts, `supervise-daemon`, `rc-update`, `-sup` crash-supervisor services — do **not** apply here.
- **Quadlet containers** — with rootless podman on systemd, PostgreSQL, LunarVision, LunarVoice, and selected workers are Quadlet `.container` files at `/home/luna1/.config/containers/systemd/` that the podman user-generator turns into `.service` units at `daemon-reload` (pg gets `Restart=on-failure` + `HealthCmd=pg_isready`). This needs podman ≥ 4.6 — trivially true on current Arch; on an older podman mt-admin falls back to a managed user unit plus a `podman wait` babysitter. Generated units are started/health-gated by their owning `start_tenant_*` path and are intentionally **not** in the `systemctl enable` list (`enable` fails on generated units).
- **Resource bounds** — systemd tenants get `lunarwing-workloads.slice` and `lunarwing-build.slice` cgroup bounds around agent-spawned workloads and operator builds. OpenRC tenants use native service cgroup v2 ceilings for the combined daemon/workload tree and a temporary memory-bounded cgroup for operator builds. Missing service-controller delegation fails startup unless the explicit emergency opt-out is set; unavailable build delegation falls back to a nice-only build with a warning. Legacy/cgroup-less service hosts retain process-tree cleanup.
- **TensorZero proxy** is opt-in with `--enable-proxy` and is **not** a container — when selected, it runs as the tenant-owned user unit `lunarwing-proxy-luna1.service` (`python3 .../tensorzero-proxy-configurations/lunarwing-proxy.py` with `EnvironmentFile=…/env/proxy.env`). PostgreSQL, LunarVision, and selected workers are rootless Podman Quadlet containers.
- **LunarVoice** is opt-in per tenant: build the shared image with `build-voice-sidecar`, then `enable-voice luna1` (verified models required) and restart the tenant. It publishes `voice_service`/`voice_health` registry ports and is health-gated on its internal `8091` listener via the same Quadlet/fallback supervision contract as LunarVision.
- **Host health pipeline** — on systemd hosts the host-global self-heal pipeline is the **system-level** timer `/etc/systemd/system/lunarwing-mt-health.timer` (enabled by `add-tenant` unless `--no-health`; cron/fcron are the OpenRC fallback). It discovers tenant health ports from `/etc/lunarwing/ports.json` — never point it at a hardcoded single-node port.
- **WeeChat / weechat-adapter** are optional; `start-tenant` skips them non-fatally if their deps are missing, and the main daemon does not depend on them.
- **`doctor`** returns non-zero if any check fails and includes a "root" assertion — run it via `sudo` so that check passes, and clear every `[FAIL]` before step 1. On systemd its extra gates are `loginctl`, podman ≥ 4.6 (Quadlet supervision) when rootless, and per-tenant linger + `/run/user/<uid>` presence for every registered tenant.
