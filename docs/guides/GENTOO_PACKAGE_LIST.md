# Gentoo Package List — Live Multi-Tenant Environment

Packages needed to run a live LunarWing multi-tenant deployment on **Gentoo + OpenRC** via
`ic/scripts/lunarwing-mt-admin.sh`, including the infra health-check + self-heal pipeline.

**Revalidated 2026-09-17 (v2.1.2.0); re-checked 2026-09-27 (v2.1.3.0), 2026-10-01 (v2.1.4.0), and 2026-10-04 (v2.1.5.0).** Package set re-checked against the
current mt-admin prerequisite probes (`require_cmd` set: cargo, chcon, curl,
git, jq, od, passwd, python3, setpriv, sha256sum, tar, taskset, useradd —
plus podman/docker); no changes were needed (the v2.1.2.0
sandbox rework added no new host packages — podman was already required, and
the v2.1.4.0 read-only diagnostics verbs `psql`/`daemon-pid` reuse existing
tooling inside the tenant containers).

This list reflects what an actual Gentoo/OpenRC/Podman MT bring-up required. Atoms are Gentoo
package names; adjust for your profile (e.g. `clang` moved to `llvm-core/` on newer profiles).

> Tip: run `sudo ic/scripts/lunarwing-mt-admin.sh doctor` — it reports most of these as
> `[PASS]`/`[FAIL]`. A `[FAIL] rustup installed` on the host is benign (see Build toolchain).

---

## 1. Core (required)

The admin script, the per-tenant daemons, and the health/self-heal pipeline need these:

| Package | Why |
|---------|-----|
| `app-misc/jq` | Port registry (`/etc/lunarwing/ports.json`), health-check JSON, self-heal, send-notification |
| `dev-vcs/git` | Per-tenant repo clones |
| `dev-lang/python` (python3) | TensorZero proxy, WeeChat WS adapter, all `health-*.sh` scripts |
| `net-misc/curl` | Self-heal Gotify pages, `health-models` probe, gateway deep-checks |
| `sys-apps/util-linux` | Provides `flock` — build lock + self-heal single-instance lock (usually already installed). Also provides `setpriv` — **required for cgroup v2 build bounding, and gated behind the `caps` USE flag** (see §1b). |

```bash
sudo emerge app-misc/jq dev-vcs/git net-misc/curl
```

### 1b. cgroup v2 build bounding — host prerequisites

`build-tenant` caps each tenant build's memory (`BUILD_MEMORY_HIGH`/`BUILD_MEMORY_MAX`)
inside a cgroup v2 build scope. On OpenRC this needs **all three** of:

1. **`setpriv` present** — Gentoo's util-linux ebuild builds it only with the
   `caps` USE flag (`$(use_enable caps setpriv)`). A default rebuild without
   `caps` installs util-linux *without* setpriv, and every build silently
   degrades to nice-only (`cgroup v2 bounding unavailable ... running nice-only`):

   ```bash
   echo 'sys-apps/util-linux caps' | sudo tee /etc/portage/package.use/lunarwing-mt
   sudo emerge --ask --oneshot sys-apps/util-linux   # pulls sys-libs/libcap-ng
   command -v setpriv   # expect /usr/bin/setpriv
   ```

2. **Unified cgroup v2 mode.** `stat -fc %T /sys/fs/cgroup` must print
   `cgroup2fs`. Gentoo OpenRC commonly boots **hybrid** by default (`/sys/fs/cgroup`
   is tmpfs, v2 lives only at `/sys/fs/cgroup/unified`), where the memory
   controller cannot be delegated — bounding is impossible. Fix:

   ```bash
   sudo sed -i 's/^#rc_cgroup_mode=.*/rc_cgroup_mode="unified"/' /etc/rc.conf && sudo reboot
   ```

3. **`memory` in the root `cgroup.controllers`** — absent only with kernels
   built without it or `cgroup_disable=memory` boot args:
   `grep -w memory /sys/fs/cgroup/cgroup.controllers`.

Current mt-admin versions print the exact failing prerequisite before the
nice-only fallback; see `docs/ops/PREDELEGATED_SUBTREE_CGROUPS.md` §0 for details.

## 2. Container runtime (required) — Podman or Docker

Each tenant's PostgreSQL runs as a container, so one runtime is **required**. This deployment uses
**Podman**:

```bash
# Podman needs the nftables USE flag on iptables (pulled in by containers-common):
echo 'net-firewall/iptables nftables' | sudo tee /etc/portage/package.use/lunarwing-mt
sudo emerge app-containers/podman      # pulls containers-common, netavark, aardvark-dns,
                                       # crun, conmon, fuse-overlayfs, passt, ...
```

Then point Podman's unqualified image search at Docker Hub so the hardcoded
`pgvector/pgvector:pg16` image resolves without a TTY prompt:

```bash
echo 'unqualified-search-registries = ["docker.io"]' \
  | sudo tee /etc/containers/registries.conf.d/zz-lunarwing-docker-io.conf
sudo podman pull pgvector/pgvector:pg16   # optional pre-pull; add-tenant does it otherwise
```

(Docker alternative: `app-containers/docker` + `app-containers/docker-cli`, added to a runlevel.)

> The `pgvector/pgvector:pg16` **container image** is pulled, not emerged.

## 3. Cron daemon (required for the health/self-heal schedule)

The host-global health-check → self-heal pipeline is scheduled via cron/fcron by
`ensure_health_pipeline()`. Without a cron daemon the schedule never fires:

```bash
sudo emerge sys-process/fcron      # or sys-process/cronie, or sys-process/dcron
```

`mt-admin` adds the cron daemon to the `default` runlevel automatically when it installs the
schedule (`*/5` by default). `fcron` is preferred (supports catch-up of missed runs).

## 4. Rust build toolchain (per-tenant builds)

| Package | Notes |
|---------|-------|
| `dev-util/rustup` | **Optional on the host** — `add-tenant` auto-installs rustup *per tenant* (via rustup.sh into the tenant's home) and sets the stable toolchain + the `wasm32-wasip2` target. The `doctor` `[FAIL] rustup installed` for the host is therefore harmless. |
| `sys-devel/clang` (or `llvm-core/clang`) | Provides `libclang`, needed by Rust crates that use `bindgen`/`-sys` native bindings during the build. (`gcc` is already present for the C toolchain.) |

`cargo-component` + `wasm-tools` (for `build-tenant --with-wasm`) are installed **per tenant** via
`cargo install` by `add-tenant` — not Gentoo packages.

## 5. WeeChat channel (optional — only if using WeeChat)

Required only if you enable the WeeChat relay channel + its services; the core MT stack and the
health pipeline do **not** need these:

| Package | Why |
|---------|-----|
| `net-irc/weechat` | The IRC client the agent talks to |
| `app-misc/tmux` | The `weechat-<tenant>` OpenRC unit runs weechat inside a tmux session |
| `dev-python/aiohttp` | Required by the WeeChat WS adapter (`lunarwing-weechat-adapter-<tenant>`); without it the adapter crashes on import |

```bash
sudo emerge net-irc/weechat app-misc/tmux dev-python/aiohttp
```

> If WeeChat deps are missing, `start-tenant` skips those services non-fatally (the core daemon
> does not depend on them).

## 6. Python packages (ops tooling)

The supported host-side Python tooling (multi-tenant onboard CLIs/web UI and Hermes importer)
needs these third-party modules. Gentoo's system Python is PEP-668 managed, so either
emerge the `dev-python/*` atoms **or** install the pinned requirements into a venv
(`python3 -m venv .venv && .venv/bin/pip install -r requirements.txt`) — each tool ships one.

| Package | Consumers |
|---------|-----------|
| `dev-python/cryptography` | `lunarwing_mt_onboard/secrets_ops.py`, `hermes_kawarimi` (secrets crypto) |
| `dev-python/psycopg` | Onboarding secret writes to tenant PostgreSQL (imports `psycopg2`; use a slot/venv providing psycopg 2.x, or `pip install psycopg2-binary` in a venv) |
| `dev-python/rich` | `lunarwing_mt_onboard` CLI, `hermes_kawarimi` |
| `dev-python/questionary` | `lunarwing_mt_onboard` interactive CLI, `hermes_kawarimi` |
| `dev-python/pyyaml` | `hermes_kawarimi` (`config.yaml` parsing), `.gitlab/scripts/validate_config.py` |
| `dev-python/fastapi` | `lunarwing_mt_onboard_web` (MT onboarding web UI) |
| `dev-python/uvicorn` | Serves `lunarwing_mt_onboard_web` |
| `dev-python/websockets` | `lunarwing_mt_onboard_web` (WS endpoints), `tests/mock_orchestrator` |
| `dev-python/pydantic` | `lunarwing_mt_onboard_web` models |
| `dev-python/httpx` | `lunarwing_mt_onboard_web` TestClient, `ic/tests/e2e` |
| `dev-python/aiohttp` | WeeChat WS adapter (see §5), `ic/tests/e2e` mock LLM |

```bash
sudo emerge \
  dev-python/cryptography dev-python/psycopg \
  dev-python/rich dev-python/questionary dev-python/pyyaml \
  dev-python/fastapi dev-python/uvicorn dev-python/websockets \
  dev-python/pydantic dev-python/httpx dev-python/aiohttp
```

Notes:

- `lunarwing_mt_onboard.secrets_ops.install_dependencies()` can `pip install cryptography
  psycopg2-binary` on demand when they are missing — venv installs are fine for these.
- The TensorZero proxy scripts (`tensorzero-proxy-configurations/*.py`) and the WeeChat relay's
  core path are stdlib-only beyond `aiohttp`; no extra modules needed.

### Test/CI-only Python packages

Only needed to run the repo's Python test harnesses, not for a live deployment — a venv with the
shipped requirements files is the recommended route:

| Package | Consumers |
|---------|-----------|
| `dev-python/pytest` (+ `pytest-asyncio`, `pytest-timeout` plugins) | `tests/runner.py`, `ic/tests/e2e` |
| `dev-python/requests` | `tests/runner.py` |
| `dev-python/websocket-client` | `tests/runner.py` (imports `websocket`) |
| `dev-python/tenacity` | `tests/runner.py` |
| `pytest-playwright` + `playwright` (pip only) | `ic/tests/e2e` browser scenarios |
| `anthropic` (pip only, optional) | `ic/tests/e2e` vision scenarios |

---

## One-shot install (Podman + fcron + WeeChat)

```bash
echo 'net-firewall/iptables nftables' | sudo tee /etc/portage/package.use/lunarwing-mt
echo 'sys-apps/util-linux caps' | sudo tee -a /etc/portage/package.use/lunarwing-mt
sudo emerge \
  app-misc/jq dev-vcs/git net-misc/curl sys-apps/util-linux \
  app-containers/podman \
  sys-process/fcron \
  sys-devel/clang \
  net-irc/weechat app-misc/tmux \
  dev-python/aiohttp dev-python/cryptography dev-python/psycopg \
  dev-python/rich dev-python/questionary dev-python/pyyaml \
  dev-python/fastapi dev-python/uvicorn dev-python/websockets \
  dev-python/pydantic dev-python/httpx
echo 'unqualified-search-registries = ["docker.io"]' \
  | sudo tee /etc/containers/registries.conf.d/zz-lunarwing-docker-io.conf
```

`rustup`, `cargo-component`, `wasm-tools`, and the wasm32 targets are handled per-tenant by
`add-tenant`, so they're intentionally omitted above.

### Recommended: make `/` a shared mount (rootless podman)

Rootless podman sets up a per-container mount namespace and warns
`"/" is not a shared mount, this could cause issues or missing mounts with rootless
containers` when `/` has `private` propagation (the OpenRC default on some hosts;
systemd makes `/` `rshared` at boot). Containers still run, but to clear the warning
and avoid mount-propagation edge cases, mark `/` shared:

```bash
sudo mount --make-rshared /
# persist across reboots (OpenRC local service):
echo 'mount --make-rshared /' | sudo tee -a /etc/local.d/00-rshared.start
sudo chmod +x /etc/local.d/00-rshared.start
sudo rc-update add local default
```

Check current propagation with `findmnt -no TARGET,PROPAGATION /`.

## Already expected on a Gentoo/OpenRC host

`bash` (>= 4), `coreutils`, `findutils`, `sed`, `gawk`, `gcc`, OpenRC (`rc-service`, `rc-update`),
and a running network. These are assumed present and not listed above.

---

## Cross-reference

- Arch Linux equivalent: [`ARCHLINUX_PACKAGE_LIST.md`](ARCHLINUX_PACKAGE_LIST.md)
- Debian equivalent: [`DEBIAN_PACKAGE_LIST.md`](DEBIAN_PACKAGE_LIST.md)
- Ubuntu equivalent: [`UBUNTU_PACKAGE_LIST.md`](UBUNTU_PACKAGE_LIST.md)
- Fedora equivalent: [`FEDORA_PACKAGE_LIST.md`](FEDORA_PACKAGE_LIST.md)
- Setup walkthrough: [`../ops/MT-GENTOO-SETUP-AND-CHANGES-MADE.md`](../ops/MT-GENTOO-SETUP-AND-CHANGES-MADE.md)
- Admin quickstart: [`MT-ADMIN-QUICKSTART.md`](MT-ADMIN-QUICKSTART.md)
- Production reference: [`../ops/MULTITENANCY-PRODUCTION.md`](../ops/MULTITENANCY-PRODUCTION.md)
