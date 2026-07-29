# Arch Linux Package List — Live Multi-Tenant Environment

Packages needed to run a live LunarWing multi-tenant deployment on **Arch Linux + systemd** via
`ic/scripts/lunarwing-mt-admin.sh`, including the infra health-check + self-heal pipeline.

This is the Arch equivalent of [`GENTOO_PACKAGE_LIST.md`](GENTOO_PACKAGE_LIST.md); the operational
notes there (port registry, per-tenant rustup, doctor output, etc.) apply unchanged. On Arch the
init system is systemd, so tenant units are `systemctl --user` units under each tenant account
(linger enabled) — see the Multi-Tenant Ops rules in the repo `AGENTS.md`.

> Tip: run `sudo ic/scripts/lunarwing-mt-admin.sh doctor` — it reports most of these as
> `[PASS]`/`[FAIL]`. A `[FAIL] rustup installed` on the host is benign (see Build toolchain).

---

## 1. Core (required)

The admin script, the per-tenant daemons, and the health/self-heal pipeline need these:

| Package | Why |
|---------|-----|
| `jq` | Port registry (`/etc/lunarwing/ports.json`), health-check JSON, self-heal, send-notification |
| `git` | Per-tenant repo clones |
| `python` (python3) | TensorZero proxy, WeeChat WS adapter, all `health-*.sh` scripts |
| `curl` | Self-heal Gotify pages, `health-models` probe, gateway deep-checks |
| `util-linux` | Provides `flock` — build lock + self-heal single-instance lock (in `base`, always present) |

```bash
sudo pacman -S --needed jq git python curl
```

## 2. Container runtime (required) — Podman or Docker

Each tenant's PostgreSQL runs as a container, so one runtime is **required**. Podman is
preferred:

```bash
sudo pacman -S --needed podman   # pulls netavark, aardvark-dns, crun, conmon, ...
```

Unlike Gentoo there is **no USE-flag step**: Arch's `iptables` package is already the
nftables-based variant that podman needs.

`lunarwing-mt-admin.sh` uses the fully-qualified `docker.io/pgvector/pgvector:pg16` image, so no
`unqualified-search-registries` config is strictly required. If you override to a short name via
`LUNARWING_MT_PG_IMAGE`, add the search registry to avoid a TTY prompt under rootless podman:

```bash
echo 'unqualified-search-registries = ["docker.io"]' \
  | sudo tee /etc/containers/registries.conf.d/zz-lunarwing-docker-io.conf
sudo podman pull pgvector/pgvector:pg16   # optional pre-pull; add-tenant does it otherwise
```

(Docker alternative: `pacman -S docker`, then `systemctl enable --now docker`.)

> The `pgvector/pgvector:pg16` **container image** is pulled, not pacman-installed.

> The Gentoo doc's "make `/` a shared mount" step is **not needed on Arch**: systemd mounts `/`
> `rshared` at boot, so rootless podman is happy out of the box.

## 3. Cron daemon (required for the health/self-heal schedule)

The host-global health-check → self-heal pipeline is scheduled via cron by
`ensure_health_pipeline()`. Without a cron daemon the schedule never fires:

```bash
sudo pacman -S --needed cronie
sudo systemctl enable --now cronie
```

`cronie` is the standard Arch cron daemon (`fcron` is AUR-only and unnecessary). `mt-admin`
enables the daemon when it installs the schedule (`*/5` by default).

## 4. Rust build toolchain (per-tenant builds)

| Package | Notes |
|---------|-------|
| `rustup` | **Optional on the host** — `add-tenant` auto-installs rustup *per tenant* (via rustup.sh into the tenant's home) and sets the stable toolchain + `wasm32-wasip1`/`wasm32-wasip2` targets. The `doctor` `[FAIL] rustup installed` for the host is therefore harmless. |
| `clang` | Provides `libclang`, needed by Rust crates that use `bindgen`/`-sys` native bindings during the build. (`gcc` is already in `base-devel`.) |

`cargo-component` + `wasm-tools` (for `build-tenant --with-wasm`) are installed **per tenant** via
`cargo install` by `add-tenant` — not pacman packages.

## 5. WeeChat channel (optional — only if using WeeChat)

Required only if you enable the WeeChat relay channel + its services; the core MT stack and the
health pipeline do **not** need these:

| Package | Why |
|---------|-----|
| `weechat` | The IRC client the agent talks to |
| `tmux` | The `weechat-<tenant>` systemd unit runs weechat inside a tmux session |
| `python-aiohttp` | Required by the WeeChat WS adapter (`lunarwing-weechat-adapter-<tenant>`); without it the adapter crashes on import |

```bash
sudo pacman -S --needed weechat tmux python-aiohttp
```

> If WeeChat deps are missing, `start-tenant` skips those services non-fatally (the core daemon
> does not depend on them).

## 6. Python packages (ops tooling)

The host-side Python tooling (multi-tenant onboard CLIs/web UI, Hermes importer, `ic_sm` secret
scripts) needs these third-party modules. Arch's system Python is PEP-668 managed, so either
pacman-install the `python-*` packages **or** install the pinned requirements into a venv
(`python3 -m venv .venv && .venv/bin/pip install -r requirements.txt`) — each tool ships one.

| Package | Consumers |
|---------|-----------|
| `python-cryptography` | `ic_sm/scripts_4_db/*.py`, `lunarwing_mt_onboard/secrets_ops.py`, `hermes_kawarimi` (secrets crypto) |
| `python-psycopg2` | Live tenant DB writes from the same secret tooling (`pip` equivalent: `psycopg2-binary`) |
| `python-rich` | `lunarwing_mt_onboard` CLI, `hermes_kawarimi` |
| `python-questionary` | `lunarwing_mt_onboard` interactive CLI, `hermes_kawarimi` |
| `python-yaml` | `hermes_kawarimi` (`config.yaml` parsing), `.gitlab/scripts/validate_config.py` (PyYAML) |
| `python-fastapi` | `lunarwing_mt_onboard_web` (MT onboarding web UI) |
| `uvicorn` | Serves `lunarwing_mt_onboard_web` |
| `python-websockets` | `lunarwing_mt_onboard_web` (WS endpoints), `tests/mock_orchestrator` |
| `python-pydantic` | `lunarwing_mt_onboard_web` models |
| `python-httpx` | `lunarwing_mt_onboard_web` TestClient, `ic/tests/e2e` |
| `python-aiohttp` | WeeChat WS adapter (see §5), `ic/tests/e2e` mock LLM |

```bash
sudo pacman -S --needed \
  python-cryptography python-psycopg2 \
  python-rich python-questionary python-yaml \
  python-fastapi uvicorn python-websockets \
  python-pydantic python-httpx python-aiohttp
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
| `python-pytest` (+ `python-pytest-asyncio`, `python-pytest-timeout` plugins) | `tests/runner.py`, `ic/tests/e2e` |
| `python-requests` | `tests/runner.py` |
| `python-websocket-client` | `tests/runner.py` (imports `websocket`) |
| `python-tenacity` | `tests/runner.py` |
| `pytest-playwright` + `playwright` (pip only) | `ic/tests/e2e` browser scenarios |
| `anthropic` (pip only, optional) | `ic/tests/e2e` vision scenarios |

---

## One-shot install (Podman + cronie + WeeChat)

```bash
sudo pacman -S --needed \
  jq git python curl \
  podman \
  cronie \
  clang \
  weechat tmux \
  python-aiohttp python-cryptography python-psycopg2 \
  python-rich python-questionary python-yaml \
  python-fastapi uvicorn python-websockets \
  python-pydantic python-httpx
sudo systemctl enable --now cronie
```

`rustup`, `cargo-component`, `wasm-tools`, and the wasm32 targets are handled per-tenant by
`add-tenant`, so they're intentionally omitted above.

## Already expected on an Arch host

`bash` (>= 4), `coreutils`, `findutils`, `sed`, `gawk`, `gcc`/`base-devel`, systemd, and a running
network. These are in `base`/`base-devel` and assumed present.

---

## Cross-reference

- Gentoo/OpenRC equivalent: [`GENTOO_PACKAGE_LIST.md`](GENTOO_PACKAGE_LIST.md)
- Admin quickstart: [`MT-ADMIN-QUICKSTART.md`](MT-ADMIN-QUICKSTART.md)
- Production reference: [`../ops/MULTITENANCY-PRODUCTION.md`](../ops/MULTITENANCY-PRODUCTION.md)
