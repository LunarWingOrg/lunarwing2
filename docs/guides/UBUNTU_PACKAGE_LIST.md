# Ubuntu Package List — Live Multi-Tenant Environment

Packages needed to run a live LunarWing multi-tenant deployment on **Ubuntu 22.04 /
24.04 LTS (or newer) + systemd** via `ic/scripts/lunarwing-mt-admin.sh`, including the
infra health-check + self-heal pipeline.

**Revalidated 2026-09-17 (v2.1.2.0); re-checked 2026-09-27 (v2.1.3.0), 2026-10-01 (v2.1.4.0), 2026-10-04 (v2.1.5.0), and 2026-10-14 (v2.1.6.0).** Package set re-checked against the
current mt-admin prerequisite probes; no changes were needed (the v2.1.2.0
sandbox rework added no new host packages — podman was already required, the
v2.1.4.0 read-only diagnostics verbs `psql`/`daemon-pid` reuse existing
tooling inside the tenant containers, and the v2.1.6.0 OpenRC proxy-unit
migration and worker-credential work add no new host commands).

This is the Ubuntu equivalent of [`GENTOO_PACKAGE_LIST.md`](GENTOO_PACKAGE_LIST.md); the
operational notes there (port registry, per-tenant rustup, doctor output, etc.) apply
unchanged. On Ubuntu the init system is systemd, so tenant units are `systemctl --user`
units under each tenant account (linger enabled) — see the Multi-Tenant Ops rules in the
repo `AGENTS.md`.

Unlike the Gentoo/Arch lists (written from live bring-ups), package names here were mapped
against the Ubuntu package index — live-verify with `doctor` below on first bring-up.

> Tip: run `sudo ic/scripts/lunarwing-mt-admin.sh doctor` — it reports most of these as
> `[PASS]`/`[FAIL]`. A `[FAIL] rustup installed` on the host is benign (see Build toolchain).
>
> Several Python packages below live in the **universe** repository — make sure it is
> enabled: `sudo add-apt-repository universe`.

---

## 1. Core (required)

The admin script, the per-tenant daemons, and the health/self-heal pipeline need these:

| Package | Why |
|---------|-----|
| `jq` | Port registry (`/etc/lunarwing/ports.json`), health-check JSON, self-heal, send-notification |
| `git` | Per-tenant repo clones |
| `python3` | TensorZero proxy, WeeChat WS adapter, all `health-*.sh` scripts |
| `curl` | Self-heal Gotify pages, `health-models` probe, gateway deep-checks |
| `util-linux` | Provides `flock` — build lock + self-heal single-instance lock (Essential, always present) |

```bash
sudo apt install jq git python3 curl
```

## 2. Container runtime (required) — Podman or Docker

Each tenant's PostgreSQL runs as a container, so one runtime is **required**. Podman is
preferred:

```bash
sudo apt install podman uidmap fuse-overlayfs slirp4netns
```

`uidmap` provides `newuidmap`/`newgidmap` (rootless podman); `fuse-overlayfs` and
`slirp4netns`/`passt` back rootless storage and networking.

> **22.04 (jammy) caveat:** jammy ships Podman 3.4, which is long EOL upstream. It works
> for the tenant PostgreSQL container, but 24.04+ (Podman 4.9+) is strongly recommended;
> alternatively use the upstream Podman packages.

`lunarwing-mt-admin.sh` uses the fully-qualified `docker.io/pgvector/pgvector:pg16` image, so
no `unqualified-search-registries` config is strictly required. If you override to a short
name via `LUNARWING_MT_PG_IMAGE`, add the search registry to avoid a TTY prompt under
rootless podman:

```bash
echo 'unqualified-search-registries = ["docker.io"]' \
  | sudo tee /etc/containers/registries.conf.d/zz-lunarwing-docker-io.conf
sudo podman pull pgvector/pgvector:pg16   # optional pre-pull; add-tenant does it otherwise
```

(Docker alternative: `apt install docker.io`, then `systemctl enable --now docker`.)

> The `pgvector/pgvector:pg16` **container image** is pulled, not apt-installed.

> The Gentoo doc's "make `/` a shared mount" step is **not needed on Ubuntu**: systemd mounts
> `/` `rshared` at boot, so rootless podman is happy out of the box.

## 3. Cron daemon (required for the health/self-heal schedule)

The host-global health-check → self-heal pipeline is scheduled via cron by
`ensure_health_pipeline()`. Without a cron daemon the schedule never fires:

```bash
sudo apt install cron
sudo systemctl enable --now cron
```

`cron` is installed and enabled by default on standard Ubuntu installs, but minimal/cloud
images sometimes omit it — check. `mt-admin` expects some cron daemon to be running when it
installs the schedule (`*/5` by default).

## 4. Rust build toolchain (per-tenant builds)

| Package | Notes |
|---------|-------|
| `rustup` | **Optional on the host** — `add-tenant` auto-installs rustup *per tenant* (via rustup.sh into the tenant's home) and sets the stable toolchain + the `wasm32-wasip2` target. The `doctor` `[FAIL] rustup installed` for the host is therefore harmless. |
| `build-essential` | C toolchain (`gcc`, `ld`, ...) used by `-sys` crates during tenant builds. |
| `pkg-config` + `libssl-dev` | `openssl-sys` is in the build graph and probes for system OpenSSL via pkg-config; missing headers fail the tenant build. |
| `clang` + `libclang-dev` | Provides `libclang`, needed by Rust crates that use `bindgen`/`-sys` native bindings during the build. |

```bash
sudo apt install build-essential pkg-config libssl-dev clang libclang-dev
```

`cargo-component` + `wasm-tools` (for `build-tenant --with-wasm`) are installed **per
tenant** via `cargo install` by `add-tenant` — not apt packages.

## 5. WeeChat channel (optional — only if using WeeChat)

Required only if you enable the WeeChat relay channel + its services; the core MT stack and
the health pipeline do **not** need these:

| Package | Why |
|---------|-----|
| `weechat` | The IRC client the agent talks to |
| `tmux` | The `weechat-<tenant>` systemd unit runs weechat inside a tmux session |
| `python3-aiohttp` | Required by the WeeChat WS adapter (`lunarwing-weechat-adapter-<tenant>`); without it the adapter crashes on import |

```bash
sudo apt install weechat tmux python3-aiohttp
```

> If WeeChat deps are missing, `start-tenant` skips those services non-fatally (the core
> daemon does not depend on them).

## 6. Python packages (ops tooling)

The supported host-side Python tooling (multi-tenant onboard CLIs/web UI and Hermes
importer) needs these third-party modules. **The existing quickstart hints only list
`python3-aiohttp`** — the full set below is what the tooling actually imports (verified
against each tool's `requirements.txt` and source imports).

Ubuntu's system Python is PEP-668 managed, so either apt-install the `python3-*` packages
(from **universe**) **or** install the pinned requirements into a venv
(`python3 -m venv .venv && .venv/bin/pip install -r requirements.txt`) — each tool ships
one. The venv route needs `python3-venv`.

> **24.04 (noble) caveat:** noble ships `python3-pydantic` **1.10.x**, but
> `lunarwing_mt_onboard_web` requires `pydantic>=2.0` / `fastapi>=0.110`. Check with
> `apt policy python3-pydantic` — if it is 1.x, use the **venv route** for the ops tooling
> (newer releases with pydantic 2.x can use the system packages).

| Package | Consumers |
|---------|-----------|
| `python3-cryptography` | `lunarwing_mt_onboard/secrets_ops.py`, `hermes_kawarimi` (secrets crypto) |
| `python3-psycopg2` | Onboarding secret writes to tenant PostgreSQL (`pip` equivalent: `psycopg2-binary`) |
| `python3-rich` | `lunarwing_mt_onboard` CLI, `hermes_kawarimi` |
| `python3-questionary` | `lunarwing_mt_onboard` interactive CLI, `hermes_kawarimi` (universe) |
| `python3-yaml` | `hermes_kawarimi` (`config.yaml` parsing), `.gitlab/scripts/validate_config.py` (PyYAML) |
| `python3-fastapi` | `lunarwing_mt_onboard_web` (MT onboarding web UI) — check version, see caveat |
| `python3-uvicorn` | Serves `lunarwing_mt_onboard_web` |
| `python3-websockets` | `lunarwing_mt_onboard_web` (WS endpoints), `tests/mock_orchestrator` |
| `python3-pydantic` | `lunarwing_mt_onboard_web` models — check version, see caveat |
| `python3-httpx` | `lunarwing_mt_onboard_web` TestClient, `ic/tests/e2e` |
| `python3-aiohttp` | WeeChat WS adapter (see §5), `ic/tests/e2e` mock LLM |

```bash
sudo apt install \
  python3-cryptography python3-psycopg2 \
  python3-rich python3-questionary python3-yaml \
  python3-fastapi python3-uvicorn python3-websockets \
  python3-pydantic python3-httpx python3-aiohttp
```

Notes:

- `lunarwing_mt_onboard.secrets_ops.install_dependencies()` can `pip install cryptography
  psycopg2-binary` on demand when they are missing — venv installs are fine for these.
- The TensorZero proxy scripts (`tensorzero-proxy-configurations/*.py`) and the WeeChat
  relay's core path are stdlib-only beyond `aiohttp`; no extra modules needed.

### Test/CI-only Python packages

Only needed to run the repo's Python test harnesses, not for a live deployment — a venv
with the shipped requirements files is the recommended route:

| Package | Consumers |
|---------|-----------|
| `python3-pytest` (+ `python3-pytest-asyncio`, `python3-pytest-timeout` plugins) | `tests/runner.py`, `ic/tests/e2e` |
| `python3-requests` | `tests/runner.py` |
| `python3-websocket` | `tests/runner.py` (imports `websocket`; this is the `websocket-client` PyPI project) |
| `python3-tenacity` | `tests/runner.py` (universe) |
| `pytest-playwright` + `playwright` (pip only) | `ic/tests/e2e` browser scenarios |
| `anthropic` (pip only, optional) | `ic/tests/e2e` vision scenarios |

---

## One-shot install (Podman + cron + WeeChat)

```bash
sudo add-apt-repository universe   # for python3-questionary / python3-tenacity, etc.
sudo apt install \
  jq git python3 curl \
  podman uidmap fuse-overlayfs slirp4netns \
  cron \
  build-essential pkg-config libssl-dev clang libclang-dev \
  weechat tmux \
  python3-aiohttp python3-cryptography python3-psycopg2 \
  python3-rich python3-questionary python3-yaml \
  python3-fastapi python3-uvicorn python3-websockets \
  python3-pydantic python3-httpx \
  python3-venv
sudo systemctl enable --now cron
```

If `apt policy python3-pydantic` shows 1.x (noble), drop `python3-fastapi`/`python3-pydantic`
from the one-shot and use a venv for `lunarwing_mt_onboard_web` instead (see §6 caveat).

`rustup`, `cargo-component`, `wasm-tools`, and the wasm32 targets are handled per-tenant by
`add-tenant`, so they're intentionally omitted above.

## Already expected on an Ubuntu host

`bash` (>= 4), `coreutils`, `findutils`, `sed`, `mawk`/`gawk`, systemd, and a running
network. These are Essential/required priority and assumed present.

---

## Cross-reference

- Gentoo/OpenRC equivalent: [`GENTOO_PACKAGE_LIST.md`](GENTOO_PACKAGE_LIST.md)
- Arch Linux equivalent: [`ARCHLINUX_PACKAGE_LIST.md`](ARCHLINUX_PACKAGE_LIST.md)
- Debian equivalent: [`DEBIAN_PACKAGE_LIST.md`](DEBIAN_PACKAGE_LIST.md)
- Fedora equivalent: [`FEDORA_PACKAGE_LIST.md`](FEDORA_PACKAGE_LIST.md)
- Admin quickstart: [`MT-ADMIN-QUICKSTART.md`](MT-ADMIN-QUICKSTART.md)
- Production reference: [`../ops/MULTITENANCY-PRODUCTION.md`](../ops/MULTITENANCY-PRODUCTION.md)
