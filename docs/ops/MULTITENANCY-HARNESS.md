# Multi-Tenancy Harness

How to run multiple isolated LunarWing instances on the same host using the test harness.

For a single instance, see [`HARNESS-SINGLE-TENANT.md`](HARNESS-SINGLE-TENANT.md).

## Canonical Two-Tenant Flow

The `mt-*` commands create two isolated roots, `${LUNARWING_MT_ROOT}-a` and
`${LUNARWING_MT_ROOT}-b`. `LUNARWING_MT_ROOT` defaults to
`${TMPDIR:-/tmp}/lunarwing-mt`; set it before `mt-init` if the state should live
elsewhere. The built-in flow assigns the following fixed, non-overlapping ports:

| Service | Tenant A | Tenant B |
|---------|----------|----------|
| PostgreSQL | 15432 | 15433 |
| TensorZero proxy | 13002 | 13003 |
| Gateway | 18765 | 18766 |
| HTTP webhook | 19098 | 19099 |
| XMPP bridge | 18787 | 18788 |
| WeeChat relay (reserved) | 19001 | 19002 |

On the Linux dev VM, use the release profile and constrain every command that
may invoke Cargo to six CPUs and six jobs:

```bash
cd ic
export LUNARWING_TEST_PROFILE=release
export CARGO_BUILD_JOBS=6

scripts/lunarwing-xmpp-test-env.sh mt-init
taskset -c 0-5 scripts/lunarwing-xmpp-test-env.sh build
taskset -c 0-5 scripts/lunarwing-xmpp-test-env.sh mt-up
scripts/lunarwing-xmpp-test-env.sh mt-verify
scripts/lunarwing-xmpp-test-env.sh mt-status
scripts/lunarwing-xmpp-test-env.sh mt-down
```

`mt-up` rebuilds the available WASM extensions once, installs them into both
tenant roots, and starts both stacks using the detected platform path. Always
run `mt-down` after the test, including after a failed `mt-verify`.

For libSQL instead of PostgreSQL, export
`LUNARWING_MT_DATABASE_KIND=libsql` before `mt-init`. To inspect gateway tokens,
run `scripts/lunarwing-xmpp-test-env.sh mt-tokens` in a private terminal; the
command prints bearer tokens and its output must not be pasted into logs or
issues.

## Secret Handling

Each generated tenant env has its own database, owner ID, gateway token, and
secrets master key. The files are mode `0600`; do not source them in a shared
shell or print their contents. Configure only the test credentials each tenant
needs through supported application/setup flows. Do **not** use
`ic_sm/scripts_4_db/insert_secret_pg.py` here: it is a migration/repair helper,
not the harness secret-management interface.

## Platform Support

The harness is cross-platform. Init system detection is automatic:

| Platform | `mt-up`/`mt-down` method | Generated service files |
|----------|--------------------------|-------------------------|
| macOS | launchd user agents | `.plist` under each tenant root, installed in `~/Library/LaunchAgents/` |
| Linux (systemd) | systemd user units | `.service` under each tenant root, installed in `~/.config/systemd/user/` |
| Linux (OpenRC) | direct PID management | None; the test harness does not install OpenRC services |
| Other | direct PID management | None |

> **OpenRC validation status:** `_mt_detect_init()` recognizes OpenRC, but the
> `mt-*` lifecycle deliberately falls back to the same direct process path as an
> unknown init. It does not exercise the production OpenRC units. This path has
> not been verified end-to-end on a current Gentoo host; treat it as unverified,
> not as evidence of production OpenRC parity.

### macOS Prerequisites

Before running `mt-up` on macOS, ensure the WASM toolchain is set up:

```bash
# 1. Both WASM targets are required (wasip1 for channels/tools, wasip2 for the daemon)
rustup target add wasm32-wasip1 wasm32-wasip2

# 2. Install WASM build tools
cargo install wasm-tools cargo-component --locked

# 3. Ensure rustup's rustc precedes Homebrew's in PATH.
#    Homebrew's rustc does NOT ship WASM targets — if it's found first, all
#    cargo component builds will fail with "can't find crate for `core`".
export PATH="$HOME/.rustup/toolchains/stable-$(rustc -vV | awk '/host/{print $2}')/bin:$HOME/.cargo/bin:$PATH"
which rustc   # must show ~/.rustup/toolchains/...
```

Docker Desktop must be running (for PostgreSQL containers). If `docker pull` fails on first run, pull the image manually:

```bash
docker pull pgvector/pgvector:pg16
```

### macOS Quick Start

```bash
cd ic
export LUNARWING_TEST_PROFILE=release
export CARGO_BUILD_JOBS=6
scripts/lunarwing-xmpp-test-env.sh mt-init
scripts/lunarwing-xmpp-test-env.sh build
scripts/lunarwing-xmpp-test-env.sh mt-up  # renders and loads tenant-scoped plists
scripts/lunarwing-xmpp-test-env.sh mt-verify
scripts/lunarwing-xmpp-test-env.sh mt-down
```

`mt-up` automatically builds and installs available WASM extensions, renders
launchd plists, and loads them; do not run a separate `build --with-wasm` or
`mt-render-launchd`. Plists are tenant-scoped (`com.lunarwing.test.mt-a.*`,
`com.lunarwing.test.mt-b.*`) so both tenants coexist in
`~/Library/LaunchAgents/`. `mt-down` unloads all agents and removes them.

### Single-tenant on macOS

Single-tenant `up`/`down` uses direct PID management on all platforms — no launchd involvement needed. Docker (or colima) must be running for the PostgreSQL container.

## Production Multi-Tenancy

For production per-user multi-tenancy with OS-level isolation, see `MULTITENANCY-PRODUCTION.md` and the admin script `ic/scripts/lunarwing-mt-admin.sh`. The production system provides dedicated OS users, registry-allocated port blocks, flock-serialized builds, and per-tenant PostgreSQL containers. It supports both systemd (user-level with linger) and OpenRC (system-level with supervise-daemon).

The test harness documented here is for development/testing only. See the comparison table in `MULTITENANCY-PRODUCTION.md` for the full list of differences.

## Future Work

- Weechat relay port integration (currently reserved, no harness plumbing yet)
- Per-instance worker container ports (Pebble, OpenCode, Kimi Code)
- Automatic port conflict detection in `doctor` command
