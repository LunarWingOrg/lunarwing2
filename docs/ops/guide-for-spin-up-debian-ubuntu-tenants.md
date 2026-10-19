# Spawn one live LunarWing tenant `luna1` (podman + systemd, Debian/Ubuntu)

Grounded in `ic/scripts/lunarwing-mt-admin.sh` and `docs/ops/MULTITENANCY-PRODUCTION.md`. Sister document to [`guide-for-spin-up-gentoo-tenants.md`](guide-for-spin-up-gentoo-tenants.md), [`guide-for-spin-up-arch-tenants.md`](guide-for-spin-up-arch-tenants.md), and [`guide-for-spin-up-fedora-tenants.md`](guide-for-spin-up-fedora-tenants.md) — same flow, Debian/Ubuntu specifics swapped in. Gaps are flagged inline rather than guessed. Package names are cross-checked against [`../guides/DEBIAN_PACKAGE_LIST.md`](../guides/DEBIAN_PACKAGE_LIST.md) and [`../guides/UBUNTU_PACKAGE_LIST.md`](../guides/UBUNTU_PACKAGE_LIST.md), which were index-mapped rather than live-verified — let `doctor` be the arbiter on first bring-up.

The whole flow is basically **four mt-admin subcommands**: `add-tenant` → `build-tenant` → `start-tenant` → `status`. Everything else is sanity-checking around them.

Run from repo root (`/home/eris/lunarwing`). Mutating subcommands require root, so they are shown with `sudo`. `status`, `tokens`, and a current-schema `list-tenants` are read-only; run `doctor` with `sudo` because its root prerequisite otherwise reports `[FAIL]`, and use `sudo list-tenants` when a registry migration may be required. Example tenant name is `luna1` — swap it everywhere.

---

## 0. Preconditions — packages, pin selection, gate on `doctor`

### 0a. Host packages (apt)

Stock Debian and Ubuntu are always systemd, so there is no init-system choice to make — only the container runtime. Everything `doctor` gates on maps to:

```bash
sudo apt-get update
sudo apt-get install \
  podman fuse-overlayfs slirp4netns uidmap \
  jq git python3 curl openssh-server \
  build-essential pkg-config libssl-dev clang libclang-dev \
  cron

sudo systemctl enable --now ssh
```

On Ubuntu make sure the **universe** repository is enabled first — `podman`, `passt`, and `slirp4netns` live there: `sudo add-apt-repository universe`.

- `podman` — container runtime. The package pulls `netavark`, `aardvark-dns`, `conmon`, and `crun`; `fuse-overlayfs` is the recommended rootless storage driver. Rootless per-tenant containers need a userspace port-forwarder (doctor accepts `pasta` **or** `slirp4netns`): `slirp4netns` exists on every supported release and is in the install line above; `passt` (provides `pasta`, the podman-5 default) is packaged on Debian 12+ and Ubuntu 23.10+, so install `passt` instead of/in addition to `slirp4netns` where available.
- **podman version reality** — doctor gates **podman ≥ 4.6 (Quadlet supervision)** whenever rootless podman is selected on systemd:
  - Debian 13 (trixie) ships podman 5.x — fine.
  - Debian 12 (bookworm, oldstable) ships podman 4.3 → fails that gate. Use `bookworm-backports` (podman 5.x) for the Quadlet path; on 4.3 mt-admin otherwise falls back to managed user units plus a `podman wait` babysitter.
  - Ubuntu 24.04 LTS (noble) ships podman 4.9 — fine.
  - Ubuntu 22.04 LTS (jammy) ships podman 3.4 (long EOL upstream) → fails the Quadlet gate; use 24.04+.
- `uidmap` — **Debian/Ubuntu-specific**: the setuid `newuidmap`/`newgidmap` binaries doctor checks for live in this package, *not* in the base shadow packages (unlike Arch `shadow` / Fedora `shadow-utils`). `passwd`/`login` (Essential) provide `useradd`/`usermod --add-subuids`. `util-linux` (Essential) provides `flock` for the build/self-heal locks.
- `openssh-server` — doctor probes sshd on `127.0.0.1:22` (loopback SSH tenants). The service is named **`ssh`** on Debian/Ubuntu (not `sshd`), hence `systemctl enable --now ssh` above.
- `build-essential` / `pkg-config` / `libssl-dev` / `clang` / `libclang-dev` — the C toolchain, `openssl-sys` (probes system OpenSSL via pkg-config), and `libclang` for `bindgen`/`-sys` crates in the tenant build graph.
- `rustup` — doctor checks both `cargo` and `rustup` on the host, but `add-tenant` installs rustup *per tenant* (via rustup.sh into the tenant home, stable toolchain + `wasm32-wasip2` target), so a host `[FAIL] rustup installed` is benign. apt's `rustc`/`cargo` do not provide the rustup shim; installing host rustup anyway (via rustup.rs, or `sudo apt install rustup` on trixie) makes the gate green.
- `cron` — fallback scheduler for the health pipeline; the primary path on systemd hosts is the `lunarwing-mt-health.timer` unit (see Notes), so this is optional but harmless (installed by default on most Debian/Ubuntu images; minimal/cloud images sometimes omit it).
- Tenant-side WASM toolchains (`cargo-component`, `wasm-tools`, wasm32 targets) are **not** host packages: `add-tenant` installs them into the tenant account itself.

First-run caveat (like Fedora): on Debian/Ubuntu, `useradd` auto-allocates subordinate uid/gid ranges via `/etc/login.defs` (`SUB_UID_MIN`/`SUB_UID_COUNT`) once the files exist — the `uidmap` package ships empty `/etc/subuid` + `/etc/subgid` — so doctor's `rootless: /etc/sub{u,g}id populated` checks normally pass out of the box. If a tenant somehow lacks a range, `add-tenant` allocates one itself (appending after the current max). Either way, re-run `doctor` after step 1 to confirm the per-tenant gates clear.

Debian/Ubuntu-specific host hardening notes:

- **AppArmor** is default-on (Ubuntu always; Debian 10+) — **leave it on**. The packaged podman AppArmor profiles cover what rootless podman needs (including Ubuntu 24.04's restricted unprivileged user namespaces). If a container start still fails, check for denials (`sudo journalctl -k | grep DENIED`, `aa-status`) before assuming an mt-admin bug; do not work around it by tearing down AppArmor.
- **ufw** (Ubuntu) may be default-deny for inbound, but every tenant port (gateway, sidecar health, worker WSS) is published on `127.0.0.1`, so no firewall openings are needed. Only touch the firewall if you deliberately re-bind a listener off-loopback.
- No USE-flag/iptables step is needed: Debian 12+/Ubuntu ship an nftables-based firewall stack, which is what podman needs, and systemd mounts `/` `rshared` at boot, so rootless podman works without the Gentoo shared-mount step.

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
# (per-tenant linger checks only pass after step 1; a host rustup FAIL is benign)
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
  ic/scripts/lunarwing-mt-admin.sh add-tenant luna1 --with-pebble
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

# Optional re-run of the dependency/health gate (linger gates now pass)
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
- **systemd vs OpenRC caveat** — on Debian/Ubuntu the units are **user-level** units in `/home/luna1/.config/systemd/user/`, running on the tenant's own `systemd --user` manager (`XDG_RUNTIME_DIR=/run/user/<uid>`); they are never system-bus units. Boot persistence is `systemctl --user enable` + `loginctl enable-linger luna1` (done by `add-tenant`), which also keeps `/run/user/<uid>` alive across reboots. Logs: `journalctl --user -u <unit>` (the file logs under `/home/luna1/lunarwing/logs/` also still exist). The OpenRC-only concepts in the Gentoo sister guide — `/etc/init.d/` scripts, `supervise-daemon`, `rc-update`, `-sup` crash-supervisor services — do **not** apply here.
- **Quadlet containers** — with rootless podman on systemd, PostgreSQL, LunarVision, LunarVoice, and selected workers are Quadlet `.container` files at `/home/luna1/.config/containers/systemd/` that the podman user-generator turns into `.service` units at `daemon-reload` (pg gets `Restart=on-failure` + `HealthCmd=pg_isready`). This needs podman ≥ 4.6 — true on Debian 13 and Ubuntu 24.04+, but **not** on Debian 12 stock (4.3) or Ubuntu 22.04 (3.4); on an older podman mt-admin falls back to a managed user unit plus a `podman wait` babysitter. Generated units are started/health-gated by their owning `start_tenant_*` path and are intentionally **not** in the `systemctl enable` list (`enable` fails on generated units).
- **Resource bounds** — systemd tenants get `lunarwing-workloads.slice` and `lunarwing-build.slice` cgroup bounds around agent-spawned workloads and operator builds. OpenRC tenants use native service cgroup v2 ceilings for the combined daemon/workload tree and a temporary memory-bounded cgroup for operator builds. Missing service-controller delegation fails startup unless the explicit emergency opt-out is set; unavailable build delegation falls back to a nice-only build with a warning. Legacy/cgroup-less service hosts retain process-tree cleanup.
- **TensorZero proxy** is opt-in with `--enable-proxy` and is **not** a container — when selected, it runs as the tenant-owned user unit `lunarwing-proxy-luna1.service` (`python3 .../tensorzero-proxy-configurations/lunarwing-proxy.py` with `EnvironmentFile=…/env/proxy.env`). PostgreSQL, LunarVision, and selected workers are rootless Podman Quadlet containers.
- **LunarVoice** is opt-in per tenant: use `install-voice luna1 --model-manifest <trusted.json>`, optional validated `configure-voice` flags, then `activate-voice luna1`. Activation requires verified STT artifacts, publishes registry `voice_service`/`voice_health` ports, health-gates both internal `8091/health` and `/ready`, and reloads a running tenant daemon; `deactivate-voice` restores its prior transcription provider and `remove-voice` removes tenant-owned voice assets. See `projects/voice-sidecar/README.md`.
- **Host health pipeline** — on systemd hosts the host-global self-heal pipeline is the **system-level** timer `/etc/systemd/system/lunarwing-mt-health.timer` (enabled by `add-tenant` unless `--no-health`; `Persistent=true` covers missed runs; a cron entry via the `cron` package is the fallback scheduler, and the primary on OpenRC). It discovers tenant health ports from `/etc/lunarwing/ports.json` — never point it at a hardcoded single-node port.
- **WeeChat / weechat-adapter** are optional; `start-tenant` skips them non-fatally if their deps are missing, and the main daemon does not depend on them. Debian/Ubuntu packages: `weechat tmux python3-aiohttp`.
- **`doctor`** returns non-zero if any check fails and includes a "root" assertion — run it via `sudo` so that check passes, and clear every `[FAIL]` before step 1. On systemd its extra gates are `loginctl`, podman ≥ 4.6 (Quadlet supervision) when rootless, and per-tenant linger + `/run/user/<uid>` presence for every registered tenant. A host `rustup`/`cargo` `[FAIL]` is benign — those toolchains are installed per tenant.

## Cross-reference

- Gentoo/OpenRC sister guide: [`guide-for-spin-up-gentoo-tenants.md`](guide-for-spin-up-gentoo-tenants.md)
- Arch Linux sister guide: [`guide-for-spin-up-arch-tenants.md`](guide-for-spin-up-arch-tenants.md)
- Fedora sister guide: [`guide-for-spin-up-fedora-tenants.md`](guide-for-spin-up-fedora-tenants.md)
- Debian host package list (full dependency tables): [`../guides/DEBIAN_PACKAGE_LIST.md`](../guides/DEBIAN_PACKAGE_LIST.md)
- Ubuntu host package list (full dependency tables): [`../guides/UBUNTU_PACKAGE_LIST.md`](../guides/UBUNTU_PACKAGE_LIST.md)
- Production reference: [`MULTITENANCY-PRODUCTION.md`](MULTITENANCY-PRODUCTION.md)
- Admin quickstart: [`../guides/MT-ADMIN-QUICKSTART.md`](../guides/MT-ADMIN-QUICKSTART.md)
