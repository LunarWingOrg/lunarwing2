#!/usr/bin/env bash
# shellcheck disable=SC2178  # DarkIRC argument parsers use intentional array namerefs.
set -euo pipefail

# ── Production Multi-Tenant Admin ─────────────────────────────────────────────
#
# Creates and manages OS-level LunarWing tenants. Each tenant is a real system
# user with its own repo clone, build artifacts, services, PostgreSQL container,
# TensorZero proxy, and XMPP bridge.
#
# Must be run as root (or via sudo).

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
LUNARWING_ROOT="$(cd -- "${REPO_ROOT}/.." && pwd)"

PORTS_REGISTRY="/etc/lunarwing/ports.json"
PORT_RANGE_START=10000
PORT_RANGE_END=19999
PORT_BLOCK_SIZE=10
BUILD_LOCK="/var/lock/lunarwing-build.lock"
TENANT_LIFECYCLE_LOCK_DIR="${LUNARWING_MT_LIFECYCLE_LOCK_DIR:-/run/lock/lunarwing-tenants}"
unset LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_TENANT
PORTS_LOCK_HELD="false"
PROFILE="${LUNARWING_MT_PROFILE:-release}"
# ── BUG-005 resource bounding ────────────────────────────────────────────────
# Transient heavy workloads (agent shell commands, compiles) must not live in
# the daemon's own service cgroup. On systemd, the daemon spawns them as
# transient scopes in WORKLOAD_SLICE (see ic/src/tools/process.rs) and
# build_tenant runs cargo in BUILD_SLICE; both slices are rendered per-tenant
# with the caps below. The daemon unit itself gets DAEMON_* caps so a runaway
# daemon is bounded independently of its workloads.
WORKLOAD_SLICE="${LUNARWING_MT_WORKLOAD_SLICE:-lunarwing-workloads.slice}"
WORKLOAD_MEMORY_HIGH="${LUNARWING_MT_WORKLOAD_MEMORY_HIGH:-6G}"
WORKLOAD_MEMORY_MAX="${LUNARWING_MT_WORKLOAD_MEMORY_MAX:-8G}"
WORKLOAD_CPU_QUOTA="${LUNARWING_MT_WORKLOAD_CPU_QUOTA:-800%}"
WORKLOAD_TASKS_MAX="${LUNARWING_MT_WORKLOAD_TASKS_MAX:-2048}"
BUILD_SLICE="${LUNARWING_MT_BUILD_SLICE:-lunarwing-build.slice}"
BUILD_JOBS="${LUNARWING_MT_BUILD_JOBS:-$(nproc)}"
BUILD_MEMORY_HIGH="${LUNARWING_MT_BUILD_MEMORY_HIGH:-16G}"
BUILD_MEMORY_MAX="${LUNARWING_MT_BUILD_MEMORY_MAX:-24G}"
DAEMON_MEMORY_HIGH="${LUNARWING_MT_DAEMON_MEMORY_HIGH:-1536M}"
DAEMON_MEMORY_MAX="${LUNARWING_MT_DAEMON_MEMORY_MAX:-2G}"
DAEMON_TASKS_MAX="${LUNARWING_MT_DAEMON_TASKS_MAX:-512}"
SOURCE_REPO="${LUNARWING_MT_SOURCE_REPO:-$LUNARWING_ROOT}"
DARKIRC_SOURCE="${LUNARWING_MT_DARKIRC_SOURCE:-}"
# Managed exchange accepts only this pinned, root-attested DarkFi profile. Build
# recipes execute in a disposable tree as a dedicated unprivileged account.
DARKIRC_REPO="${LUNARWING_MT_DARKIRC_REPO:-https://github.com/darkrenaissance/darkfi}"
DARKIRC_APPROVED_REV="a05956d412a091e8b54c1cd4f4264c33b941203d"
DARKIRC_REV="${LUNARWING_MT_DARKIRC_REV:-$DARKIRC_APPROVED_REV}"
DARKIRC_BUILD_USER="${LUNARWING_MT_DARKIRC_BUILD_USER:-lunarwing-build}"
DARKIRC_BUILD_HOME="${LUNARWING_MT_DARKIRC_BUILD_HOME:-/var/cache/lunarwing/build-home}"
DARKIRC_BUILD_ROOT="${LUNARWING_MT_DARKIRC_BUILD_ROOT:-/var/cache/lunarwing/darkirc-builds}"
DARKIRC_KEY_HELPER_BUILD_ROOT="${LUNARWING_MT_DARKIRC_KEY_HELPER_BUILD_ROOT:-/var/cache/lunarwing/darkirc-key-helper-target}"
DARKIRC_RELEASE_ROOT="${LUNARWING_MT_DARKIRC_RELEASE_ROOT:-/usr/local/lib/lunarwing/darkirc}"
DARKIRC_RELEASES_DIR="$DARKIRC_RELEASE_ROOT/releases"
DARKIRC_RELEASE_SELECTOR="$DARKIRC_RELEASE_ROOT/current"
DARKIRC_RELEASE_DAEMON="$DARKIRC_RELEASE_SELECTOR/darkirc"
DARKIRC_WRITER_LOCK_DIR="${LUNARWING_MT_DARKIRC_WRITER_LOCK_DIR:-/run/lock/lunarwing-darkirc}"
DARKIRC_WRITER_LOCK_HELD="false"
DARKIRC_WRITER_LOCK_FD=""
DARKIRC_WRITER_SCOPE=""
DARKIRC_WRITER_LOCK_INHERITED="false"
OPENRC_ENV_EXEC_SRC="$SCRIPT_DIR/lunarwing-openrc-env-exec.sh"
OPENRC_ENV_EXEC="/usr/local/libexec/lunarwing-openrc-env-exec"
OPENRC_INITD_DIR="${LUNARWING_MT_OPENRC_INITD_DIR:-/etc/init.d}"
OPENRC_CONFD_DIR="${LUNARWING_MT_OPENRC_CONFD_DIR:-/etc/conf.d}"
OPENRC_RUNLEVELS_DIR="${LUNARWING_MT_OPENRC_RUNLEVELS_DIR:-/etc/runlevels}"
TEMPLATES_DIR="${SCRIPT_DIR}/templates"
DEFAULT_TENSORZERO_URL="${LUNARWING_MT_TENSORZERO_URL:-http://192.168.1.157:3000/openai/v1}"
# Fleet-wide default VL (vision-language) backend URL the OCR sidecar proxies to.
# Empty = sidecar comes up with VL disabled (vl_available=false), preserving the
# pre-VL behavior for deployments without a local vision server. Override with
# LUNARWING_MT_VL_URL. The host.containers.internal hostname is the rootless
# podman host bridge (169.254.1.2) — verified reachable from inside tenant
# sidecar containers on this box.
DEFAULT_VL_URL="${LUNARWING_MT_VL_URL:-http://host.containers.internal:8080/v1/chat/completions}"
# Fleet-wide default for the daemon's LLM endpoint (LLM_BASE_URL). Empty = fall
# back to each tenant's local TensorZero proxy. Set this (or --llm-base-url per
# tenant) to point new tenants straight at a gateway as the proxy is phased out.
DEFAULT_LLM_BASE_URL="${LUNARWING_MT_LLM_BASE_URL:-}"
DEFAULT_GOTIFY_URL="${LUNARWING_MT_GOTIFY_URL:-}"
DEFAULT_GOTIFY_TITLE="${LUNARWING_MT_GOTIFY_TITLE:-}"
VOICE_MODELS_DIR="${LUNARWING_MT_VOICE_MODELS_DIR:-/var/lib/lunarwing/voice-models}"
DEFAULT_EXTERNAL_WORKER_TIMEOUT_MS="${LUNARWING_MT_EXTERNAL_WORKER_TIMEOUT_MS:-3600000}"
LEGACY_EXTERNAL_WORKER_TIMEOUT_MS=300000
MIGRATE_LEGACY_EXTERNAL_WORKER_TIMEOUTS="${LUNARWING_MT_MIGRATE_LEGACY_EXTERNAL_WORKER_TIMEOUTS:-true}"

# ── Health-check / self-heal pipeline (host-global) ──────────────────────────
# The infra health-check + self-heal pipeline auto-discovers every tenant from
# /etc/init.d and the port registry, so ONE host-global scheduled run covers all
# current and future tenants. Enabled by default for new tenants on OpenRC; opt
# out per add-tenant with --no-health, or fleet-wide with
# LUNARWING_MT_HEALTH_ENABLED=false.
DEFAULT_HEALTH_ENABLED="${LUNARWING_MT_HEALTH_ENABLED:-true}"
# Resolved after init detection: OpenRC defaults to 5 minutes, systemd to 15.
# An explicit override applies to either init system.
HEALTH_INTERVAL_MIN="${LUNARWING_MT_HEALTH_INTERVAL_MIN:-}"
HEALTH_BASE_DIR="${LUNARWING_MT_HEALTH_BASE_DIR:-/var/lib/lunarwing-health}"
HEALTH_SRC_DIR="$LUNARWING_ROOT/ic-infrastructure-health-check"
HEALTH_LIB_DIR="/usr/local/lib/lunarwing-health"
HEALTH_ENV_FILE="/etc/lunarwing/health.env"
HEALTH_LAUNCHER="/usr/local/sbin/lunarwing-mt-health"
# Gotify for self-heal escalations (token must NOT be committed — supply via env;
# it is written only to $HEALTH_ENV_FILE, mode 0600).
HEALTH_GOTIFY_URL="${LUNARWING_MT_GOTIFY_URL:-}"
HEALTH_GOTIFY_TOKEN="${LUNARWING_MT_GOTIFY_TOKEN:-}"
HEALTH_OPT_OUT=false   # set true by --no-health

# ── SSH harness defaults ──────────────────────────────────────────────────────
# SSH is enabled by default for new tenants: the harness provisions an ed25519
# key pair, configures a localhost SSH host in config.toml, and uploads the
# private key to the encrypted secrets store after the daemon starts. Workers
# get the SSH agent socket bind-mounted so they can authenticate over SSH
# without ever holding key material on disk. Opt out per-tenant with --no-ssh,
# or fleet-wide with LUNARWING_MT_SSH_ENABLED=false.
DEFAULT_SSH_ENABLED="${LUNARWING_MT_SSH_ENABLED:-true}"
SSH_OPT_OUT=false   # set true by --no-ssh

WEECHAT_BOOTSTRAP_OPT_OUT=false   # set true by --no-weechat-bootstrap

# ── Per-tenant PostgreSQL image ──────────────────────────────────────────────
# Fully-qualified (registry host included) so rootless podman resolves it WITHOUT
# depending on the host's unqualified-search-registries: docker silently defaults
# short names to docker.io, but rootless podman errors ("short-name ... did not
# resolve to an alias and no unqualified-search registries are defined"). Override
# for a local mirror via LUNARWING_MT_PG_IMAGE.
PG_IMAGE="${LUNARWING_MT_PG_IMAGE:-docker.io/pgvector/pgvector:pg16}"

# ── Per-tenant PostgreSQL backups ────────────────────────────────────────────
# pg_dump each tenant's DB (custom -Fc format) to $BACKUP_DIR/<tenant>/. Keep the
# most recent $BACKUP_KEEP dumps per tenant (0 = keep all).
BACKUP_DIR="${LUNARWING_MT_BACKUP_DIR:-/var/lib/lunarwing-backups}"
BACKUP_KEEP="${LUNARWING_MT_BACKUP_KEEP:-7}"

# ── Helpers ───────────────────────────────────────────────────────────────────

# ── Library sourcing ─────────────────────────────────────────────────────────
#
# Phase 2 of the mt-admin refactor (sunburst/mtadmin-refactor-v2.0.6.0):
# common, detect, ports-registry, tenant-build, owner-scope, postgres, and
# health-pipeline are extracted. Remaining libraries (containers,
# env-generation, units-systemd, units-openrc, worker-config, tenant-lifecycle)
# land in later phases; until then their functions still live inline below.
#
# Libraries define functions only — nothing runs at source time — so load
# order is not load-bearing. The composition root defines all constants and
# `set -euo pipefail` before any library is sourced.
#
# `selfcheck` (added in a later phase) will assert the expected symbols
# survived the load; `doctor` will run it too.

MT_ADMIN_LIB_DIR="${SCRIPT_DIR}/mt-admin-lib"
[[ -d "$MT_ADMIN_LIB_DIR" ]] || {
  printf 'error: mt-admin library directory not found: %s\n' "$MT_ADMIN_LIB_DIR" >&2
  exit 1
}

# shellcheck source=mt-admin-lib/00-common.sh
source "$MT_ADMIN_LIB_DIR/00-common.sh"

# shellcheck source=mt-admin-lib/01-detect.sh
source "$MT_ADMIN_LIB_DIR/01-detect.sh"

# shellcheck source=mt-admin-lib/02-containers.sh
source "$MT_ADMIN_LIB_DIR/02-containers.sh"

# shellcheck source=mt-admin-lib/03-ports-registry.sh
source "$MT_ADMIN_LIB_DIR/03-ports-registry.sh"

# shellcheck source=mt-admin-lib/04-tenant-lifecycle.sh
source "$MT_ADMIN_LIB_DIR/04-tenant-lifecycle.sh"

# shellcheck source=mt-admin-lib/05-tenant-build.sh
source "$MT_ADMIN_LIB_DIR/05-tenant-build.sh"

# shellcheck source=mt-admin-lib/06-env-generation.sh
source "$MT_ADMIN_LIB_DIR/06-env-generation.sh"

# shellcheck source=mt-admin-lib/07-units-systemd.sh
source "$MT_ADMIN_LIB_DIR/07-units-systemd.sh"

# shellcheck source=mt-admin-lib/08-units-openrc.sh
source "$MT_ADMIN_LIB_DIR/08-units-openrc.sh"

# shellcheck source=mt-admin-lib/09-worker-config.sh
source "$MT_ADMIN_LIB_DIR/09-worker-config.sh"

# shellcheck source=mt-admin-lib/10-owner-scope.sh
source "$MT_ADMIN_LIB_DIR/10-owner-scope.sh"

# shellcheck source=mt-admin-lib/11-postgres.sh
source "$MT_ADMIN_LIB_DIR/11-postgres.sh"

# shellcheck source=mt-admin-lib/12-health-pipeline.sh
source "$MT_ADMIN_LIB_DIR/12-health-pipeline.sh"

usage() {
  cat <<'EOF'
Usage:
  sudo lunarwing-mt-admin.sh <command> [args...]

Production multi-tenant administration for LunarWing.
Manages OS users, port allocation, per-tenant services, and builds.

Commands:
  add-tenant <name> [options]      Create user, allocate ports, clone repo,
                                   generate env, render and install services
    --docker-group                 Add user to docker/podman group
    --xmpp-jid <jid>              XMPP JID for this tenant
    --xmpp-password <pass>        XMPP password (generated if omitted)
    --llm-api-key <key>            API key for the LLM backend provider
    --llm-base-url <url>           LLM endpoint the daemon dials (LLM_BASE_URL).
                                   Default: this tenant's local TensorZero proxy
    --tensorzero-url <url>         Upstream TensorZero URL
    --gotify-url <url>             Custom Gotify server URL (e.g. https://gotify.example.com)
    --no-health                    Don't enable the host-global health/self-heal pipeline
    --no-ssh                       Don't provision SSH harness (key pair, config, agent)
    --no-weechat-bootstrap         Don't run WeeChat relay auto-bootstrap (still writes
                                   minimal weechat.env; services remain rendered)
    --enable-darkirc               Provision DarkIRC daemon + adapter for this tenant
                                   (disabled by default; darkirc services are NOT created)
    --with-pebble                  Select the pebble worker for this tenant (as above)
    --with-opencode                Select the opencode worker for this tenant (as above)
    --with-kimi                    Select the kimi worker for this tenant (as above)
    --opencode-model <model>       Override the opencode worker's LLM model
                                   (written to lunarwing.env as OPENCODE_MODEL)
    --opencode-base-url <url>      Override the opencode worker's TensorZero baseURL
                                   (written to lunarwing.env as OPENCODE_BASE_URL)
    --kimi-model <model>           Override the kimi worker's LLM model
                                   (written to lunarwing.env as KIMI_MODEL)
    --kimi-base-url <url>          Override the kimi worker's API base URL
                                   (written to lunarwing.env as KIMI_BASE_URL)
    --kimi-api-key-file <path>     Read kimi API key from a file, never argv
                                   (written to lunarwing.env as KIMI_API_KEY)
    --llm-model <model>            Override LLM_MODEL (default:
                                   tensorzero::function_name::lunarwing)
    --gateway-host <host>          Override GATEWAY_HOST bind address (default:
                                   127.0.0.1; use 0.0.0.0 for LAN access)
    --xmpp-allow-from <jids>       Comma-separated extra XMPP JIDs allowed to DM
                                   the agent (added to the tenant's own JID;
                                   written to both lunarwing.env and xmpp-bridge.env)

   add-tenants <names> [options]    Comma-separated list (e.g. "Ruffles,Miyuki")
     (same options as add-tenant apply to all, including --enable-darkirc,
       --opencode-model/--opencode-base-url, --kimi-model/--kimi-base-url/
       --kimi-api-key-file,
      --llm-model, --gateway-host, --xmpp-allow-from, and --no-weechat-bootstrap)

  remove-tenant <name>             Stop services, deallocate ports
    --purge                        Also delete OS user and home directory

  build-tenant <name>             Build binaries for one tenant (OOM-safe flock)
    --with-wasm                    Also build WASM extensions
    --require-wasm                 Fail instead of preserving stale WASM (requires --with-wasm)
    --with-pebble                  Also build the pebble worker Docker image
    --with-opencode                Also build the opencode worker Docker image
    --with-kimi                    Also build the kimi worker Docker image
    --with-toolchains              Include toolchains in selected pebble, opencode,
                                   and kimi worker images (default: slim)

  build-all                        Build each tenant sequentially
    --with-wasm                    Also build WASM extensions
    --with-pebble                  Also build the pebble worker Docker image
    --with-opencode                Also build the opencode worker Docker image
    --with-kimi                    Also build the kimi worker Docker image
    --with-toolchains              Include toolchains in selected pebble, opencode,
                                   and kimi worker images (default: slim)

  build-pebble-worker             Build the pebble worker Docker image
    --no-cache                     Force a full rebuild without Docker cache
    --with-toolchains              Include Rust/Go/C++ toolchains (default: slim)

  build-opencode-worker            Build the opencode worker Docker image
    --no-cache                     Force a full rebuild without Docker cache
                                   (Podman also prunes dangling images and avoids
                                   caching intermediate build layers)
    --with-toolchains              Include Rust/Go/C++ toolchains (default: slim)

  build-kimi-worker                Build the kimi worker Docker image
    --no-cache                     Force a full rebuild without container cache
    --with-toolchains              Include Rust/Go/C++ toolchains (default: slim)
    --kimi-code-version <version>  Override kimi-code package version

  build-vision-sidecar             Build the LunarVision OCR sidecar Docker image
  build-voice-sidecar              Build the LunarVoice ONNX sidecar container image
  enable-voice <name>              Configure/start local voice; restart tenant to apply
                                   (verified models required)
  disable-voice <name>             Disable/stop local voice; restart tenant to apply

  build-darkirc                   Build darkirc daemon from external source
  ensure-darkirc-release <name>   Build the atomic release set when required
                                   (shared binary, not per-tenant)

  install-wasm <name>             Install built WASM tools/channels into tenant state dir
  install-wasm-all                Install WASM for all tenants

  start-tenant <name>             Start all services for a tenant
  stop-tenant <name>              Stop all services for a tenant
  cleanup-legacy-weechat <name>   Remove obsolete weechat-<name> service
  restart-weechat-adapter <name>  Restart adapter through detected init system
  stop-tenant-for-upgrade <name>  Verify settled DarkIRC state and stop atomically
  migration-export-boundary <name> [export options]
                                  Run export under retained lifecycle/DarkIRC locks
  restart-tenant <name>           Stop then start
  start-pg <name>                 Start only the tenant's PostgreSQL (daemon stays down)
  stop-pg <name>                  Stop only the tenant's PostgreSQL
  start-worker <name> <worker>    Start one enabled external worker
  stop-worker <name> <worker>     Gracefully stop one external worker
  restart-worker <name> <worker>  Restart one external worker
  kill-worker <name> <worker>     Emergency zero-grace stop; leaves worker stopped
                                   Workers: pebble, opencode, kimi
  render-units <name>             Re-render a tenant's service units from the current
                                  generator (no restart; applies init-script changes)
  upgrade-tenant <name> --target <ref>
                                  In-place upgrade: backup, fetch, clean-tree gate,
                                  stop, checkout <ref> (as tenant user), rebuild
                                   with WASM and selected OpenCode/Kimi workers, re-render
                                  units, patch env, start.
                                  Long-running — run inside tmux.
    --source-repo <path>           Point the tenant's git origin at this repo first
    --no-backup                    Skip the pre-upgrade Postgres backup
    --skip-render                  Keep existing unit files (run render-units later;
                                   must happen before v2.0.0 for pre-1.1.9 tenants)
  rotate-pg-password <name>       Generate a new random PG password (ALTER ROLE + env update)

  configure-gotify <name> <url>    Set custom Gotify URL for a tenant
                                   (updates workspace config + capabilities)

  configure-pebble <name>          Configure pebble worker for a tenant
    --nanogpt-api-key <key>        NanoGPT API key
    --model <model>                Pebble model (default: openai/gpt-5.2)

  configure-opencode <name>        Set opencode worker LLM overrides for a tenant
    --model <model>                TensorZero model (OPENCODE_MODEL; any string)
    --base-url <url>               TensorZero baseURL (OPENCODE_BASE_URL; full URL)
                                   (restart the worker after: stop-tenant && start-tenant)

  configure-kimi <name>            Set kimi worker LLM overrides for a tenant
    --model <model>                Kimi model (KIMI_MODEL; any string)
    --base-url <url>               Kimi API base URL (KIMI_BASE_URL; full URL)
    --api-key-file <path>          Read Kimi API key from a file, never argv
                                   (restart the worker after: stop-tenant && start-tenant)

  configure-study-mcp <name>       Configure lw2studymcp for LunarWing and
    --home <absolute-path>          its opencode worker using a shared host checkout
    [--allow-write]                 Enable approved core-agent writes using separate state
    [--state-home <absolute-path>]  State path (default: tenant lunarwing/study-notes-state)
                                   (restart the tenant after configuration)

   configure-ssh <name>             Provision SSH harness for an existing tenant
     --host <host>                  SSH host (default: 127.0.0.1)
     --user <user>                  SSH user (default: tenant name)
                                   (restart the tenant after to upload the key: restart-tenant)

  configure-weechat-relay <name>   Generate WeeChat relay config for a tenant.
                                   Preserves existing config: fails on any
                                   non-empty ~/.config/weechat without modifying it.

  disable-weechat <name>           Disable WeeChat for a tenant: stops its (optional)
  enable-weechat <name>            weechat units and removes their boot persistence
                                   (systemd disable / OpenRC runlevel removal), and
                                   health checks report them `skipped` when down so
                                   self-heal will not restart-storm them. start-tenant
                                   leaves them off. enable-weechat re-enables + starts
                                   them. Default is enabled (absent flag == enabled).

  patch-env <name>                 Add missing env vars (e.g. ORCHESTRATOR_PORT)
  patch-env-all                    Patch env for all registered tenants

  darkirc-contact list|status|doctor|adopt ...
                                   Inventory/adopt contacts using fingerprints only
  darkirc-contact prepare|respond|complete|cancel|exchanges ...
                                   Public-only Stage A exchange; complete is deferred
  darkirc-contact export-migration|stage-migration|import-migration|migration-ready|recover ...
                                   Structured, settled-state DarkIRC migration
  darkirc-contact validate-migration --in - [--unbound]
                                   Validate a manifest without tenant-state writes
  darkirc-health <tenant> --strict [--json]
                                   Diagnostic daemon/adapter connectivity check

  migrate-owner-scope <name>       Rekey DB data from 'default' to tenant scope
    --from <old_scope>             Old owner_id (default: 'default')
                                   (run after patch-env adds LUNARWING_OWNER_ID)
  owner-scopes <name>              Print restored owner scopes and row counts

  list-tenants                     Show all tenants with ports and status
  status <name>                    Detailed status for one tenant
  tokens [name]                    Print gateway auth tokens (all or one)
  doctor                           System dependency and health checks

  backup-tenant <name>             pg_dump a tenant's DB (custom format) to
                                   $LUNARWING_MT_BACKUP_DIR/<name>/
  backup-all                       Back up every registered tenant
  list-backups [name]              List existing backups (all tenants or one)
  restore-tenant <name> <file>     Restore a tenant DB from a dump (DESTRUCTIVE)
    --yes                          Required: confirm the DROP+recreate restore
                                   (stop the tenant daemon first)

Environment:
  LUNARWING_SERVICE_MANAGER        Override: systemd or openrc
  LUNARWING_CONTAINER_RUNTIME      Override: docker or podman (persisted to /etc/lunarwing/container-runtime on first explicit use; later runs need no env var)
  LUNARWING_MT_PROFILE             Build profile: release (default) or debug
  LUNARWING_MT_SOURCE_REPO         Path to source repo to clone from
  LUNARWING_MT_TENSORZERO_URL      Default upstream TensorZero URL
  LUNARWING_MT_LLM_BASE_URL        Fleet-wide default LLM_BASE_URL for new tenants
                                   (empty = each tenant's local TensorZero proxy)
  LUNARWING_MT_GOTIFY_URL          Default Gotify server URL for new tenants
  LUNARWING_MT_GOTIFY_TITLE        Default Gotify notification title for new tenants
  LUNARWING_MT_BACKUP_DIR          Backup directory (default /var/lib/lunarwing-backups)
  LUNARWING_MT_BACKUP_KEEP         Keep last N dumps per tenant (default 7; 0 = keep all)

Per-tenant LLM tunables (written to each tenant's lunarwing.env with the
fleet defaults below; edit the file + restart-tenant to override — an existing
value is preserved across re-provision):
  LLM_CIRCUIT_BREAKER_THRESHOLD    Consecutive fully-retried LLM failures before
                                   the breaker opens and fast-fails (default 7)
  LLM_CIRCUIT_BREAKER_RECOVERY_SECS  Seconds the breaker stays open before it
                                   probes for recovery (default 45)
EOF
}

# ── Template rendering ────────────────────────
#
# render_template <template-file> <output-file> [var=value ...]
#
# Reads a template file, replaces __VARNAME__ placeholders with their values
# (supplied as var=value pairs on the command line), and writes the result.
# Template files live under $TEMPLATES_DIR. If the template path is relative
# (no leading /), it is resolved against $TEMPLATES_DIR.

# ── Init system detection ────────────────────────────────────────────────────

_ctr() {
  local name="$1"; shift
  ensure_container_runtime
  if [[ "$MT_ROOTLESS" == "true" ]]; then
    local uid home
    uid="$(id -u "$name")" || die "cannot resolve uid for tenant '$name'"
    home="$(getent passwd "$name" | cut -d: -f6)"
    # Run from a tenant-traversable CWD (F10): `sudo -u` keeps the caller's cwd, so
    # when mt-admin runs from an admin dir the tenant can't enter (e.g. ~dame, 0700)
    # `sudo -u` aborts with "cannot chdir ... Permission denied" BEFORE the runtime
    # runs — which silently broke the rootless pg readiness gate (it always timed
    # out). `/` is always traversable; no _ctr call passes a cwd-relative path. exec
    # preserves the exit code and the stdin/stdout redirects used by exec/pg_dump.
    ( cd / && exec sudo -u "$name" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" "$CONTAINER_RT" "$@" )
  else
    "$CONTAINER_RT" "$@"
  fi
}

# Ensure a worker image is available to whoever will run the tenant's container.
# Rootful (docker): the shared root store already has it — just verify presence.
# Rootless (podman): the image lives in the tenant's OWN store; if absent, copy it
# from the admin (root) store via save|load (per-tenant, ~minutes for large images;
# a shared additionalimagestore would avoid the N copies but isn't wired yet).
# Returns non-zero if the image can't be made available (caller should skip).
_ensure_tenant_image() {
  local name="$1" image="$2"

  # The admin (root) store's image ID is the source of truth for "current".
  local admin_id=""
  if "$CONTAINER_RT" image inspect -f '{{.Id}}' "$image" &>/dev/null; then
    admin_id="$("$CONTAINER_RT" image inspect -f '{{.Id}}' "$image")"
  fi

  # rootful: the admin store IS the runtime store, so presence there suffices.
  if [[ "$MT_ROOTLESS" != "true" ]]; then
    [[ -n "$admin_id" ]] && return 0
    return 1   # rootful + not built yet -> caller skips (build first)
  fi

  # rootless: each tenant has its own store. Skip only when the tenant already
  # holds the CURRENT image (same ID as the admin store) — not merely when the
  # name exists — so a rebuilt worker image actually reaches tenants instead of
  # being silently held back by a stale same-named copy.
  local tenant_id=""
  if _ctr "$name" image inspect -f '{{.Id}}' "$image" &>/dev/null; then
    tenant_id="$(_ctr "$name" image inspect -f '{{.Id}}' "$image")"
  fi
  if [[ -n "$tenant_id" && "$tenant_id" == "$admin_id" ]]; then
    return 0   # tenant already has the current image
  fi
  [[ -n "$admin_id" ]] || return 1   # rootless, but the admin store has no source image to copy

  if [[ -n "$tenant_id" ]]; then
    say "refreshing stale image $image in ${name}'s rootless store (save|load — minutes for large images) ..."
  else
    say "distributing image $image into ${name}'s rootless store (save|load — minutes for large images) ..."
  fi
  if "$CONTAINER_RT" save "$image" | _ctr "$name" load >/dev/null 2>&1; then
    # Drop the previous (now-untagged) image if the load re-pointed the tag, so
    # repeated worker-image updates don't accumulate GBs of stale layers in the
    # tenant's rootless store.
    _ctr "$name" image prune -f >/dev/null 2>&1 || true
    say "image $image available in ${name}'s store"
    return 0
  fi
  say "WARNING: failed to load $image into ${name}'s store"
  return 1
}

# Render a dedicated OpenRC unit for a tenant's supported external worker,
# modeled on the lunarwing-pg-<t> unit. Health-aware status() checks the worker's
# /health endpoint (curl is present in both worker images) so the host self-heal
# pipeline — which auto-discovers /etc/init.d/lunarwing-* units — can detect and
# remediate a crashed OR hung worker, and so it survives reboot. OpenRC only.

# Install (or refresh) the babysitter helper binary from this repo to /usr/local/sbin.
# Idempotent: skips if the on-disk copy is byte-identical (mtime/perm-check), so
# `start_tenant` is safe to call repeatedly without churning the file. This
# decouples helper provisioning from the watchdog installer — a tenant created
# before the watchdog is installed still gets a working babysitter.
ensure_babysitter_helper() {
  local src="${SCRIPT_DIR}/lunarwing-ctr-babysit.sh"
  local dst="/usr/local/sbin/lunarwing-ctr-babysit"
  [[ -f "$src" ]] || {
    say "WARNING: babysitter helper source missing: $src (skipping install)"
    return 0
  }
  if [[ -f "$dst" ]] && cmp -s "$src" "$dst"; then
    return 0  # up to date; don't touch mtime/perm
  fi
  install -o root -g root -m 0755 "$src" "$dst" \
    && say "Installed babysitter helper: $dst" \
    || say "WARNING: failed to install babysitter helper to $dst"
}

# Render a supervised babysitter OpenRC unit for a rootless container.
# The babysitter blocks on `podman wait <container>` and respawns via supervise-daemon
# when the container exits, providing docker-parity crash recovery (~seconds, not minutes).
# Only applicable to rootless podman on OpenRC (systemd uses Quadlet).
# Usage: render_container_babysitter_unit <tenant> <container-type> <container-name> <uid> <home>
# Example: render_container_babysitter_unit acme pg lunarwing-pg-acme 1001 /home/acme
render_container_babysitter_unit() {
  local name="$1" type="$2" container="$3" uid="$4" home="$5"
  ensure_init_system
  [[ "$INIT_SYSTEM" == "openrc" ]] || return 0
  [[ "$MT_ROOTLESS" == "true" ]] || return 0
  # The pg path renders directly (bypassing _register_babysitter), so install the
  # helper here too — otherwise a worker-less tenant renders a -sup unit whose
  # required_files=<helper> never exists and supervise-daemon silently never starts it.
  ensure_babysitter_helper
  local babysitter="/etc/init.d/${container}-sup"
  # Log into the tenant's existing logs dir (created at add-tenant, tenant-owned),
  # exactly like every other unit (lunarwing/proxy/xmpp-bridge). The old
  # /var/log/lunarwing/<t> path had no parent on a fresh host, so the non-recursive
  # `checkpath -d` failed, supervise-daemon could not open output_log/error_log, and
  # the babysitter never stayed up (landed in /run/openrc/failed/).
  local log_dir
  log_dir="$(tenant_lw_root "$name")/logs"

  cat >"$babysitter" <<INITEOF
#!/sbin/openrc-run

description="LunarWing ${type} container babysitter ($name)"

: "\${babysitter_container:=$container}"
: "\${babysitter_user:=$name}"
: "\${babysitter_home:=$home}"
: "\${babysitter_uid:=$uid}"
: "\${babysitter_respawn_delay:=2}"
: "\${babysitter_respawn_max:=10}"
: "\${babysitter_respawn_period:=120}"
: "\${babysitter_log_dir:=$log_dir}"
: "\${babysitter_output_log:=\${babysitter_log_dir}/${type}-babysitter.log}"
: "\${babysitter_error_log:=\${babysitter_log_dir}/${type}-babysitter.err}"

supervisor="supervise-daemon"
command="/usr/local/sbin/lunarwing-ctr-babysit"
command_args="\${babysitter_container}"
command_user="\${babysitter_user}:\${babysitter_user}"
respawn_delay="\${babysitter_respawn_delay}"
respawn_max="\${babysitter_respawn_max}"
respawn_period="\${babysitter_respawn_period}"
output_log="\${babysitter_output_log}"
error_log="\${babysitter_error_log}"
required_files="\${command}"

depend() {
    need net localmount
    after firewall
}

start_pre() {
    checkpath -d -m 0700 -o "\${babysitter_user}:\${babysitter_user}" "/run/user/\${babysitter_uid}"
    checkpath -d -m 0750 -o "\${babysitter_user}:\${babysitter_user}" "\${babysitter_log_dir}"
    checkpath -f -m 0640 -o "\${babysitter_user}:\${babysitter_user}" "\${babysitter_output_log}"
    checkpath -f -m 0640 -o "\${babysitter_user}:\${babysitter_user}" "\${babysitter_error_log}"

    # Export rootless environment. Verified at runtime on OpenRC 0.63.1: across its
    # setuid, supervise-daemon re-sets HOME to the tenant's passwd home and leaves
    # XDG_RUNTIME_DIR untouched, so the supervised podman-wait process already runs
    # with HOME=/home/<t> + XDG_RUNTIME_DIR=/run/user/<uid>. These exports are belt-
    # and-suspenders; no sudo -u wrapper is needed. (No backticks in this heredoc:
    # it is unquoted, so backticks would be executed at render time.)
    if [ -n "\${babysitter_home}" ]; then
        export HOME="\${babysitter_home}"
    fi
    if [ -n "\${babysitter_uid}" ]; then
        export XDG_RUNTIME_DIR="/run/user/\${babysitter_uid}"
    fi
}
INITEOF
  chmod 0755 "$babysitter"
}

# Register a babysitter unit for a container (render + boot-enable + start).
# Idempotent: safe to call multiple times.
_register_babysitter() {
  local name="$1" type="$2" container="$3" uid="$4" home="$5"
  ensure_init_system
  [[ "$INIT_SYSTEM" == "openrc" ]] || return 0
  [[ "$MT_ROOTLESS" == "true" ]] || return 0
  # render_container_babysitter_unit installs the helper itself (covers pg + workers).
  render_container_babysitter_unit "$name" "$type" "$container" "$uid" "$home"
  rc-update add "${container}-sup" default >/dev/null 2>&1 || true
  rc-service "${container}-sup" start >/dev/null 2>&1 || true
}

# Deregister a babysitter unit (stop + boot-disable + remove).
_deregister_babysitter() {
  local container="$1"
  ensure_init_system
  [[ "$INIT_SYSTEM" == "openrc" ]] || return 0
  [[ -f "/etc/init.d/${container}-sup" ]] || return 0
  rc-service "${container}-sup" stop >/dev/null 2>&1 || true
  rc-update del "${container}-sup" default >/dev/null 2>&1 || true
  rm -f "/etc/init.d/${container}-sup"
}



# ── Environment file generation ──────────────────────────────────────────────

# Echo the existing VALUE of KEY from a tenant env file (empty if file/key absent).
# Used to PRESERVE secrets across re-runs so re-provisioning never rotates them.
_env_existing() {  # <env_file> <KEY>
  _read_env_value "$1" "$2"
}

_env_set() {  # <env_file> <KEY> <VALUE>
  local path="$1" key="$2" value="$3" tmp
  [[ "$key" =~ ^[A-Z][A-Z0-9_]*$ ]] || die "invalid environment key: $key"
  [[ "$value" != *$'\n'* && "$value" != *$'\r'* ]] \
    || die "environment value for $key contains a newline"
  [[ -f "$path" && ! -L "$path" ]] || die "environment file missing or unsafe: $path"
  tmp="$(mktemp "$(dirname "$path")/.env-set.XXXXXX")"
  awk -v prefix="$key=" -v replacement="$key=$value" '
    index($0, prefix) != 1 { print }
    END { print replacement }
  ' "$path" >"$tmp"
  chmod --reference="$path" "$tmp"
  chown --reference="$path" "$tmp"
  mv -T -- "$tmp" "$path"
}

validate_study_notes_path() {  # <absolute path> <label>
  [[ "$1" =~ ^/[A-Za-z0-9._/-]+$ ]] \
    || die "$2 must be an absolute path without whitespace or shell metacharacters"
}

validate_study_notes_home() {  # <absolute checkout path>
  local home="$1"
  validate_study_notes_path "$home" "Study Notes home"
  [[ -d "$home" ]] || die "Study Notes home directory missing: $home"
  [[ -f "$home/study_notes_mcp.py" ]] || die "Study Notes file missing: $home/study_notes_mcp.py"
  [[ -f "$home/memorydb.py" ]] || die "Study Notes file missing: $home/memorydb.py"
  [[ -f "$home/agent-notes.sqlite3" ]] || die "Study Notes file missing: $home/agent-notes.sqlite3"
  [[ -d "$home/agent-notes-by-agent" ]] || die "Study Notes directory missing: $home/agent-notes-by-agent"
  [[ -f "$home/skills/study-notes/SKILL.md" ]] || die "Study Notes skill missing: $home/skills/study-notes/SKILL.md"
  local home_real relative
  home_real="$(readlink -f -- "$home")"
  for relative in \
    study_notes_mcp.py \
    memorydb.py \
    agent-notes.sqlite3 \
    agent-notes-by-agent \
    skills/study-notes/SKILL.md; do
    [[ "$(readlink -f -- "$home/$relative")" == "$home_real/$relative" ]] \
      || die "Study Notes code path must not be a symlink: $home/$relative"
  done
}

validate_study_notes_state_home() {  # <absolute state path>
  local state_home="$1"
  validate_study_notes_path "$state_home" "Study Notes state home"
  [[ -d "$state_home" ]] || die "Study Notes state directory missing: $state_home"
  [[ -f "$state_home/agent-notes.sqlite3" ]] \
    || die "Study Notes state database missing: $state_home/agent-notes.sqlite3"
  [[ -d "$state_home/agent-notes-by-agent" ]] \
    || die "Study Notes state notes directory missing: $state_home/agent-notes-by-agent"
  local state_real database_real notes_real
  state_real="$(readlink -f -- "$state_home")"
  database_real="$(readlink -f -- "$state_home/agent-notes.sqlite3")"
  notes_real="$(readlink -f -- "$state_home/agent-notes-by-agent")"
  [[ "$database_real" == "$state_real/agent-notes.sqlite3" ]] \
    || die "Study Notes state database must not be a symlink"
  [[ "$notes_real" == "$state_real/agent-notes-by-agent" ]] \
    || die "Study Notes notes directory must not be a symlink"
}

tenant_study_notes_home() {  # <tenant>
  _env_existing "$(tenant_env_dir "$1")/lunarwing.env" LW2STUDYMCP_HOME
}

tenant_study_notes_state_home() {  # <tenant>
  _env_existing "$(tenant_env_dir "$1")/lunarwing.env" LW2STUDYMCP_STATE_HOME
}

tenant_study_notes_allow_write() {  # <tenant>
  _env_existing "$(tenant_env_dir "$1")/lunarwing.env" LW2STUDYMCP_ALLOW_WRITE
}

study_notes_write_enabled() {  # <raw boolean>
  case "${1:-}" in
    1|true|TRUE|yes|YES|on|ON) return 0 ;;
    ""|0|false|FALSE|no|NO|off|OFF) return 1 ;;
    *) die "LW2STUDYMCP_ALLOW_WRITE must be a boolean" ;;
  esac
}

# Resolve a tenant's XMPP JID: an explicit <jid> wins; else PRESERVE the one
# already in lunarwing.env; else fall back to <name>@xmpp.localhost (first-time
# provision). lunarwing.env is the authoritative source — the bridge binary
# reads only XMPP_BRIDGE_{BIND,TOKEN,MAX_MESSAGES} and receives the JID from the
# daemon via POST /v1/configure, so a JID lost here silently repoints the tenant.
# Without this, re-running add-tenant without --xmpp-domain/--xmpp-jid reset a
# live tenant to the non-resolving xmpp.localhost default and the bridge sat in
# an endless connect-retry while /v1/status still reported configured+running.
resolve_tenant_xmpp_jid() {  # <name> [explicit_jid]
  local name="$1" explicit="${2:-}"
  if [[ -n "$explicit" ]]; then
    printf '%s\n' "$explicit"
    return 0
  fi
  local existing
  existing="$(_env_existing "$(tenant_env_dir "$name")/lunarwing.env" XMPP_JID)"
  printf '%s\n' "${existing:-$name@xmpp.localhost}"
}

# Build the XMPP_ALLOW_FROM comma-separated value: owner JID first, then any
# extra JIDs from --xmpp-allow-from (comma-separated), deduped (owner JID and
# duplicate extras collapse), surrounding whitespace trimmed. <owner_jid> may
# be empty only in error paths; <extras_csv> is the raw flag value.
build_xmpp_allow_from() {  # <owner_jid> <extras_csv>
  local owner="$1"
  local extras="$2"
  local seen=""
  local result=""
  local jid
  # Owner first (skip if empty, though it normally isn't). `seen` uses leading
  # and trailing commas so substring matching on ",<jid>," is unambiguous.
  if [[ -n "$owner" ]]; then
    result="$owner"
    seen=",$owner,"
  fi
  # Extras: split on comma, trim whitespace, dedupe
  if [[ -n "$extras" ]]; then
    local IFS=','
    read -ra parts <<< "$extras"
    for jid in "${parts[@]}"; do
      jid="$(echo "$jid" | xargs)"   # trim leading/trailing whitespace
      [[ -n "$jid" ]] || continue
      [[ "$seen" == *",$jid,"* ]] && continue
      result="${result:+$result,}$jid"
      seen="${seen}$jid,"
    done
  fi
  echo "$result"
}

# Build the XMPP_ALLOW_FROM_JSON value: same semantics as build_xmpp_allow_from
# but emits a JSON array. Each JID is wrapped in double quotes; no escaping is
# applied (XMPP JIDs do not contain characters that require JSON escaping under
# the XEP-0029 node/domain rules in normal use). Output is a single line.
build_xmpp_allow_from_json() {  # <owner_jid> <extras_csv>
  local owner="$1"
  local extras="$2"
  local csv
  csv="$(build_xmpp_allow_from "$owner" "$extras")"
  local IFS=','
  local parts=()
  [[ -n "$csv" ]] && read -ra parts <<< "$csv"
  local quoted=()
  local jid
  for jid in "${parts[@]}"; do
    quoted+=("\"$jid\"")
  done
  echo "[${quoted[*]}]" | tr ' ' ',' | sed 's/,,*/,/g; s/^\[,/[/; s/,\]$/\]/'
}

# ── WeeChat relay auto-bootstrap ──────────────────────────────────────────────

# WeeChat home directory for a tenant.
tenant_weechat_home() {  # <tenant>
  printf '%s/.config/weechat' "$(tenant_home "$1")"
}

# Read the LAST literal value of KEY from an env file without sourcing it.
# Returns empty if file or key is absent. Never prints the file's other content.
_read_env_value() {  # <file> <KEY>
  [[ -f "$1" ]] || return 0
  local value
  value="$(sed -n "s/^$2=//p" "$1" | tail -1)"
  # Current writers use systemd-compatible double quotes. Keep decoding the
  # older onboarding client's single-quoted representation during migration.
  if [[ "$value" =~ ^\"(.*)\"$ ]]; then
    value="${BASH_REMATCH[1]}"
    value="${value//\\\"/\"}"
    value="${value//\\\\/\\}"
  elif [[ "$value" =~ ^\'(.*)\'$ ]]; then
    value="${BASH_REMATCH[1]}"
    value="${value//\\\'/\'}"
    value="${value//\\\\/\\}"
  fi
  printf '%s\n' "$value"
}

# Read one value without opening tenant-controlled paths as root. The tenant-side
# process rejects unsafe ownership, permissions, symlinks, and shared hard links.
read_tenant_env_value() {  # <tenant> <file> <KEY>
  local name="$1" target="$2" key="$3" value
  [[ "$key" =~ ^[A-Z][A-Z0-9_]*$ ]] || die "invalid tenant env key"
  darkirc_path_components_safe "$target" || die "unsafe tenant env path: $target"
  value="$(sudo -u "$name" env -i PATH="/usr/bin:/bin" \
    TARGET_PATH="$target" TARGET_KEY="$key" bash -c '
      set -euo pipefail
      target="$TARGET_PATH"
      parent="${target%/*}"
      uid="$(id -u)"
      [[ -d "$parent" && ! -L "$parent" ]] || exit 73
      parent_owner="$(stat -c %u "$parent" 2>/dev/null || printf -- -1)"
      parent_mode="$(stat -c %a "$parent" 2>/dev/null || true)"
      [[ "$parent_owner" == "$uid" && "$parent_mode" =~ ^[0-7]+$ ]] || exit 73
      (( (8#$parent_mode & 077) == 0 )) || exit 73
      [[ ! -e "$target" ]] && exit 0
      [[ -f "$target" && ! -L "$target" ]] || exit 73
      owner="$(stat -c %u "$target" 2>/dev/null || printf -- -1)"
      mode="$(stat -c %a "$target" 2>/dev/null || true)"
      links="$(stat -c %h "$target" 2>/dev/null || printf 0)"
      [[ "$owner" == "$uid" && "$links" == 1 && "$mode" =~ ^[0-7]+$ ]] || exit 73
      (( (8#$mode & 077) == 0 && (8#$mode & 0400) != 0 )) || exit 73
      prefix="$TARGET_KEY="
      value=""
      while IFS= read -r line || [[ -n "$line" ]]; do
        [[ "$line" == "$prefix"* ]] && value="${line#"$prefix"}"
      done <"$target"
      printf "%s\n" "$value"
    ')" || return 1
  if [[ "$value" =~ ^\"(.*)\"$ ]]; then
    value="${BASH_REMATCH[1]}"
    value="${value//\\\"/\"}"
    value="${value//\\\\/\\}"
  elif [[ "$value" =~ ^\'(.*)\'$ ]]; then
    value="${BASH_REMATCH[1]}"
    value="${value//\\\'/\'}"
    value="${value//\\\\/\\}"
  fi
  printf '%s\n' "$value"
}

# Write stdin to a tenant-owned file through a same-directory candidate. Root
# never opens the destination for writing and secret contents never enter argv.
write_tenant_file_atomic() {  # <tenant> <file> [mode]
  local name="$1" target="$2" mode="${3:-600}"
  [[ "$mode" =~ ^[0-7]{3,4}$ ]] || die "invalid tenant file mode"
  darkirc_path_components_safe "$target" || die "unsafe tenant file path: $target"
  sudo -u "$name" env -i PATH="/usr/bin:/bin" \
    TARGET_PATH="$target" TARGET_MODE="$mode" bash -c '
      set -euo pipefail
      target="$TARGET_PATH"
      parent="${target%/*}"
      base="${target##*/}"
      uid="$(id -u)"
      [[ -d "$parent" && ! -L "$parent" ]] || exit 73
      parent_owner="$(stat -c %u "$parent" 2>/dev/null || printf -- -1)"
      parent_mode="$(stat -c %a "$parent" 2>/dev/null || true)"
      [[ "$parent_owner" == "$uid" && "$parent_mode" =~ ^[0-7]+$ ]] || exit 73
      (( (8#$parent_mode & 077) == 0 )) || exit 73
      [[ ! -L "$target" && ( ! -e "$target" || -f "$target" ) ]] || exit 73
      if [[ -e "$target" ]]; then
        owner="$(stat -c %u "$target" 2>/dev/null || printf -- -1)"
        links="$(stat -c %h "$target" 2>/dev/null || printf 0)"
        [[ "$owner" == "$uid" && "$links" == 1 ]] || exit 73
      fi
      umask 077
      tmp="$(mktemp "$parent/.${base}.next.XXXXXX")"
      trap '"'"'rm -f -- "$tmp"'"'"' EXIT
      cat >"$tmp"
      chmod "$TARGET_MODE" "$tmp"
      mv -T -f -- "$tmp" "$target"
      trap - EXIT
    '
}

_quote_env_value() {  # <one-line value>
  local value="$1"
  [[ "$value" != *$'\n'* && "$value" != *$'\r'* ]] \
    || die "environment values cannot contain newlines"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  printf '"%s"' "$value"
}

_read_secret_file() {  # <path> <description>
  local path="$1" description="$2" value
  [[ -f "$path" && -r "$path" ]] || die "$description file is not readable: $path"
  value="$(<"$path")"
  [[ -n "$value" ]] || die "$description file is empty: $path"
  [[ "$value" != *$'\n'* && "$value" != *$'\r'* ]] \
    || die "$description file must contain exactly one line"
  printf '%s\n' "$value"
}

# Success when <dir> contains any entry (normal or dotfile).
_weechat_config_dir_has_entries() {  # <dir>
  [[ -d "$1" ]] || return 1
  local count
  count="$(ls -A "$1" 2>/dev/null | wc -l)"
  [[ "$count" -gt 0 ]]
}

# Validate a generated WeeChat relay config directory.
# Requires relay.conf with the literal ${env:RELAY_PASSWORD} expression,
# bind_address=127.0.0.1, ipv6=off, allow_empty_password=off, [api] section with
# api=<port>, and rejects any occurrence of the plaintext password.
_weechat_validate_relay_config() {  # <dir> <port> <plaintext_password>
  local dir="$1" port="$2" plaintext="$3"
  local conf="$dir/relay.conf"
  [[ -f "$conf" ]] || return 1
  # Reject plaintext password leak
  if [[ -n "$plaintext" ]] && grep -qF -- "$plaintext" "$conf"; then
    return 1
  fi
  # Require literal env expression
  grep -qF '${env:RELAY_PASSWORD}' "$conf" || return 1
  # Require loopback bind
  grep -q 'bind_address.*127\.0\.0\.1' "$conf" || return 1
  # Require allow_empty_password = off
  grep -q 'allow_empty_password.*off' "$conf" || return 1
  # An IPv4 loopback bind is invalid while WeeChat's relay IPv6 mode is enabled.
  grep -q '^[[:space:]]*ipv6[[:space:]]*=[[:space:]]*off[[:space:]]*$' "$conf" || return 1
  # Require [api] section
  grep -q '\[api\]' "$conf" || return 1
  # Require api = <port>
  grep -q "^[[:space:]]*api[[:space:]]*=[[:space:]]*${port}\$" "$conf" || return 1
  return 0
}

# Invoke WeeChat as the tenant to generate a relay.conf in <temp_dir>.
# The plaintext password is read from <lunarwing_env> and passed via
# environment inheritance only; never in argv.
_weechat_generate_relay_config() {  # <tenant> <temp_dir> <port> <lunarwing_env>
  local tenant="$1" temp_dir="$2" relay_port="$3" lunarwing_env="$4"
  local relay_password weechat_bin
  relay_password="$(_read_env_value "$lunarwing_env" RELAY_PASSWORD)"
  [[ -n "$relay_password" ]] || return 1
  weechat_bin="$(command -v weechat 2>/dev/null || true)"
  if [[ -z "$weechat_bin" ]]; then
    printf 'error: required command not found: weechat\n' >&2
    return 1
  fi
  # The backslash before ${env:RELAY_PASSWORD} is mandatory: WeeChat evaluates
  # --run-command arguments before /set stores the value, so the escape ensures
  # the literal expression is written to relay.conf, not the resolved password.
  RELAY_PASSWORD="$relay_password" sudo --preserve-env=RELAY_PASSWORD -u "$tenant" \
    "$weechat_bin" --dir "$temp_dir" \
    --run-command '/set relay.network.password "\${env:RELAY_PASSWORD}"' \
    --run-command '/set relay.network.allow_empty_password off' \
    --run-command '/set relay.network.ipv6 off' \
    --run-command '/set relay.network.bind_address "127.0.0.1"' \
    --run-command "/relay add api ${relay_port}" \
    --run-command '/set weechat.look.save_layout_on_exit buffers' \
    --run-command '/save' \
    --run-command '/quit'
}

# Full one-shot relay bootstrap for a tenant: generate, validate, atomically
# promote. Rejects non-empty existing config. Cleans all failure paths.
configure_weechat_relay() {  # <tenant>
  local tenant="$1"
  local target_home relay_port lunarwing_env relay_password

  target_home="$(tenant_weechat_home "$tenant")"
  relay_port="$(ports_get "$tenant" weechat)"
  if [[ -z "$relay_port" || "$relay_port" == "0" ]]; then
    printf 'error: no weechat port allocated for tenant %q\n' "$tenant" >&2
    return 1
  fi
  lunarwing_env="$(tenant_env_dir "$tenant")/lunarwing.env"
  if [[ ! -f "$lunarwing_env" ]]; then
    printf 'error: tenant env file not found: %s\n' "$lunarwing_env" >&2
    return 1
  fi
  relay_password="$(_read_env_value "$lunarwing_env" RELAY_PASSWORD)"
  if [[ -z "$relay_password" ]]; then
    printf 'error: RELAY_PASSWORD missing from %s\n' "$lunarwing_env" >&2
    return 1
  fi

  # Invariant: the weechat.env credential file must exist before the
  # preserve-and-fail check so legacy existing configs receive the file
  # required by the rendered systemd/OpenRC service unit.
  #
  # Non-empty legacy config (preserve-and-fail): backfill weechat.env only
  # when it does not exist yet, then return 1 — existing env values must
  # survive alongside the existing config.
  local weechat_env_file
  weechat_env_file="$(tenant_env_dir "$tenant")/weechat.env"
  if [[ -d "$target_home" ]] && _weechat_config_dir_has_entries "$target_home"; then
    if [[ ! -f "$weechat_env_file" ]]; then
      if ! _write_weechat_env "$tenant" "$relay_password"; then
        printf 'error: unable to write the dedicated WeeChat credential environment\n' >&2
        return 1
      fi
    fi
    printf 'error: WeeChat config already exists at %s — configure-weechat-relay never overwrites existing config\n' "$target_home" >&2
    return 1
  fi

  # Fresh generation path: atomically synchronize weechat.env to the current
  # lunarwing.env password before generating config. This overwrites stale
  # values left from a prior tenant or aborted run.
  if ! _write_weechat_env "$tenant" "$relay_password"; then
    printf 'error: unable to write the dedicated WeeChat credential environment\n' >&2
    return 1
  fi

  # Temp directory as a sibling of the target (same filesystem for atomic rename).
  local config_parent temp_home
  config_parent="$(dirname "$target_home")"
  if ! mkdir -p "$config_parent" || ! chown "$tenant:$tenant" "$config_parent"; then
    printf 'error: unable to prepare WeeChat config parent: %s\n' "$config_parent" >&2
    return 1
  fi
  temp_home="$(mktemp -d "${config_parent}/.weechat-bootstrap.XXXXXX")" || {
    printf 'error: unable to create WeeChat bootstrap directory under %s\n' "$config_parent" >&2
    return 1
  }

  (
    trap '[[ -n "${temp_home:-}" && -d "$temp_home" ]] && rm -rf -- "$temp_home"' EXIT
    if ! chown "$tenant:$tenant" "$temp_home"; then
      return 1
    fi

    # Generate via WeeChat one-shot.
    if ! _weechat_generate_relay_config "$tenant" "$temp_home" "$relay_port" "$lunarwing_env"; then
      return 1
    fi

    # Validate the generated config.
    if ! _weechat_validate_relay_config "$temp_home" "$relay_port" "$relay_password"; then
      say "generated WeeChat relay config failed validation"
      return 1
    fi

    # Atomically promote: remove empty target if present, then rename.
    if [[ -d "$target_home" ]]; then
      if ! rmdir "$target_home" 2>/dev/null; then
        printf 'error: target %s exists and is not empty (cannot promote)\n' "$target_home" >&2
        return 1
      fi
    fi
    if ! mv -T -- "$temp_home" "$target_home"; then
      return 1
    fi
    temp_home=""
  )

  local rc=$?
  if [[ $rc -ne 0 ]]; then
    return $rc
  fi

  if ! chown -R "$tenant:$tenant" "$target_home"; then
    printf 'error: unable to set WeeChat config ownership: %s\n' "$target_home" >&2
    return 1
  fi
  say "WeeChat relay configured for tenant '$tenant' (port $relay_port, loopback)"
}

# Write a dedicated mode-0600 weechat.env containing only RELAY_PASSWORD.
# Uses a temp file in the same directory then atomically renames.
_write_weechat_env() {  # <tenant> <relay_password>
  local tenant="$1" relay_password="$2"
  local env_dir env_file tmp_file
  env_dir="$(tenant_env_dir "$tenant")"
  env_file="$env_dir/weechat.env"
  mkdir -p "$env_dir" || return 1
  tmp_file="$(mktemp "$env_dir/.weechat.env.XXXXXX")" || return 1
  (
    trap '[[ -n "${tmp_file:-}" && -f "$tmp_file" ]] && rm -f -- "$tmp_file"' EXIT
    umask 077
    printf 'RELAY_PASSWORD=%s\n' "$relay_password" >"$tmp_file" || return 1
    chmod 0600 "$tmp_file" || return 1
    chown "$tenant:$tenant" "$tmp_file" || return 1
    mv -T -- "$tmp_file" "$env_file" || return 1
    tmp_file=""
  )
}

write_tenant_lunarwing_env() {
  local name="$1"
  # Empty $2 means "caller supplied none" -> preserve an existing JID (see
  # resolve_tenant_xmpp_jid); only a first write falls back to xmpp.localhost.
  local xmpp_jid
  xmpp_jid="$(resolve_tenant_xmpp_jid "$name" "${2:-}")"
  local xmpp_password="${3:-}"   # resolved below (preserve an existing one on re-run)
  local tensorzero_url="${4:-$DEFAULT_TENSORZERO_URL}"
  local llm_api_key="${5:-}"
  local llm_base_url="${6:-}"
  local llm_model="${7:-}"
  local gateway_host="${8:-}"
  local xmpp_allow_from="${9:-}"
  local opencode_model="${10:-}"
  local opencode_base_url="${11:-}"
  local kimi_model="${12:-}"
  local kimi_base_url="${13:-}"
  local kimi_api_key="${14:-}"

  local path gateway_port http_port bridge_port pg_port proxy_port weechat_port weechat_adapter_port orchestrator_port pebble_wss_port opencode_wss_port kimi_wss_port
  path="$(tenant_env_dir "$name")/lunarwing.env"
  gateway_port="$(ports_get "$name" gateway)"
  http_port="$(ports_get "$name" http)"
  bridge_port="$(ports_get "$name" bridge)"
  pg_port="$(ports_get "$name" postgres)"
  proxy_port="$(ports_get "$name" proxy)"
  weechat_port="$(ports_get "$name" weechat)"
  weechat_adapter_port="$(ports_get "$name" weechat_adapter)"
  orchestrator_port="$(ports_get "$name" orchestrator)"
  pebble_wss_port="$(ports_get "$name" pebble_wss)"
  opencode_wss_port="$(ports_get "$name" opencode_wss)"
  kimi_wss_port="$(ports_get "$name" kimi_wss)"

  local state_dir run_dir
  state_dir="$(tenant_state_dir "$name")"
  run_dir="$(tenant_run_dir "$name")"

  # Idempotent on re-run (F4-A/F4-B): PRESERVE existing secrets when lunarwing.env
  # already exists. Regenerating SECRETS_MASTER_KEY would permanently orphan the
  # tenant's encrypted DB secrets (it is the AES-256-GCM vault key); rotating the
  # tokens would break live clients/workers; minting a fresh XMPP_PASSWORD would
  # break the already-registered XMPP account. Generate fresh ONLY on first write.
  local gateway_token bridge_token relay_password secrets_key webhook_secret pg_password darkirc_adapter_secret
  local coord_enabled coord_agent_id coord_agent_aliases coord_channels coord_admin_users coord_admin_token coord_admin_port
  local study_notes_home study_notes_state_home study_notes_allow_write
  gateway_token="$(_env_existing "$path" GATEWAY_AUTH_TOKEN)";   gateway_token="${gateway_token:-$(generate_token)}"
  bridge_token="$(_env_existing "$path" XMPP_BRIDGE_TOKEN)";     bridge_token="${bridge_token:-$(generate_token | cut -c1-32)}"
  relay_password="$(_env_existing "$path" RELAY_PASSWORD)";      relay_password="${relay_password:-$(generate_token | cut -c1-32)}"
  secrets_key="$(_env_existing "$path" SECRETS_MASTER_KEY)";     secrets_key="${secrets_key:-$(generate_token)}"
  webhook_secret="$(_env_existing "$path" HTTP_WEBHOOK_SECRET)"; webhook_secret="${webhook_secret:-$(generate_token)}"
  coord_enabled="$(_env_existing "$path" COORD_ENABLED)"; coord_enabled="${coord_enabled:-false}"
  coord_agent_id="$(_env_existing "$path" COORD_AGENT_ID)"; coord_agent_id="${coord_agent_id:-$name}"
  coord_agent_aliases="$(_env_existing "$path" COORD_AGENT_ALIASES)"
  coord_channels="$(_env_existing "$path" COORD_CHANNELS)"
  coord_admin_users="$(_env_existing "$path" COORD_ADMIN_USERS)"
  coord_admin_token="$(_env_existing "$path" COORD_ADMIN_TOKEN)"; coord_admin_token="${coord_admin_token:-$(generate_token)}"
  coord_admin_port="$(_env_existing "$path" COORD_ADMIN_PORT)"
  if [[ -z "$coord_admin_port" ]]; then
    coord_admin_port="$(ports_get "$name" coord_admin || true)"
    [[ -n "$coord_admin_port" ]] || die "coordination admin port missing for tenant '$name'"
  fi
  if tenant_darkirc_enabled "$name"; then
    darkirc_adapter_secret="$(_env_existing "$path" DARKIRC_ADAPTER_SECRET)"
  fi
  # XMPP password: an explicit --xmpp-password wins; else preserve an existing one;
  # else mint a fresh one (first-time provision).
  [[ -n "$xmpp_password" ]] || { xmpp_password="$(_env_existing "$path" XMPP_PASSWORD)"; xmpp_password="${xmpp_password:-$(generate_token | cut -c1-32)}"; }
  [[ -n "$opencode_model" ]]    || opencode_model="$(_env_existing "$path" OPENCODE_MODEL)"
  [[ -n "$opencode_base_url" ]] || opencode_base_url="$(_env_existing "$path" OPENCODE_BASE_URL)"
  [[ -n "$kimi_model" ]]        || kimi_model="$(_env_existing "$path" KIMI_MODEL)"
  [[ -n "$kimi_base_url" ]]     || kimi_base_url="$(_env_existing "$path" KIMI_BASE_URL)"
  [[ -n "$kimi_api_key" ]]      || kimi_api_key="$(_env_existing "$path" KIMI_API_KEY)"
  study_notes_home="$(_env_existing "$path" LW2STUDYMCP_HOME)"
  study_notes_state_home="$(_env_existing "$path" LW2STUDYMCP_STATE_HOME)"
  study_notes_allow_write="$(_env_existing "$path" LW2STUDYMCP_ALLOW_WRITE)"
  local transcription_enabled transcription_provider transcription_model voice_enabled
  voice_enabled="$(_env_existing "$(tenant_env_dir "$name")/voice.env" VOICE_ENABLED)"
  voice_enabled="${voice_enabled:-false}"
  transcription_enabled="$(_env_existing "$path" TRANSCRIPTION_ENABLED)"
  transcription_enabled="${transcription_enabled:-$voice_enabled}"
  transcription_provider="$(_env_existing "$path" TRANSCRIPTION_PROVIDER)"
  transcription_provider="${transcription_provider:-lunarvoice}"
  transcription_model="$(_env_existing "$path" TRANSCRIPTION_MODEL)"
  transcription_model="${transcription_model:-whisper-tiny}"
  # LLM circuit breaker: fleet defaults (open after 7 fully-retried failures,
  # probe again after 45s). These are tunables, so preserve an operator's
  # per-tenant override across re-provision rather than clobbering it.
  local cb_threshold cb_recovery
  cb_threshold="$(_env_existing "$path" LLM_CIRCUIT_BREAKER_THRESHOLD)";  cb_threshold="${cb_threshold:-7}"
  cb_recovery="$(_env_existing "$path" LLM_CIRCUIT_BREAKER_RECOVERY_SECS)"; cb_recovery="${cb_recovery:-45}"
  # Stable + migration-safe; resolved before the heredoc so it can read an
  # existing DATABASE_URL (preserving an already-initialised DB's password).
  pg_password="$(tenant_pg_password "$name")"

  # LLM endpoint the daemon's OpenAI-compatible client dials. When the proxy is
  # enabled, defaults to this tenant's local TensorZero proxy. When disabled,
  # defaults to the upstream TENSORZERO_URL directly. An explicit value (from
  # --llm-base-url or LUNARWING_MT_LLM_BASE_URL) always overrides.
  local llm_base_url_effective
  if tenant_proxy_enabled "$name"; then
    llm_base_url_effective="${llm_base_url:-http://127.0.0.1:${proxy_port}/v1}"
  else
    llm_base_url_effective="${llm_base_url:-$tensorzero_url}"
  fi

  # Idempotent overrides for the configurable flags (--llm-model,
  # --gateway-host, --xmpp-allow-from). An explicit flag value wins; else an
  # existing file value is preserved on re-run; else the hardcoded default
  # (LLM_MODEL, GATEWAY_HOST) or the owner JID alone (XMPP_ALLOW_FROM).
  local llm_model_effective gateway_host_effective xmpp_allow_from_effective
  if [[ -n "$llm_model" ]]; then
    llm_model_effective="$llm_model"
  else
    llm_model_effective="$(_env_existing "$path" LLM_MODEL)"
    llm_model_effective="${llm_model_effective:-tensorzero::function_name::lunarwing}"
  fi
  if [[ -n "$gateway_host" ]]; then
    gateway_host_effective="$gateway_host"
  else
    gateway_host_effective="$(_env_existing "$path" GATEWAY_HOST)"
    gateway_host_effective="${gateway_host_effective:-127.0.0.1}"
  fi
  if [[ -n "$xmpp_allow_from" ]]; then
    xmpp_allow_from_effective="$(build_xmpp_allow_from "$xmpp_jid" "$xmpp_allow_from")"
  else
    # Preserve an existing list; fall back to owner JID only on first write.
    xmpp_allow_from_effective="$(_env_existing "$path" XMPP_ALLOW_FROM)"
    xmpp_allow_from_effective="${xmpp_allow_from_effective:-$xmpp_jid}"
  fi

  (
    umask 077
    cat >"$path" <<ENVEOF
LUNARWING_BASE_DIR=$state_dir
IRONCLAW_BASE_DIR=$state_dir
LUNARWING_SOCKET=$run_dir/lunarwing.sock
IRONCLAW_SOCKET=$run_dir/lunarwing.sock

# Database
DATABASE_BACKEND=postgres
DATABASE_URL=postgres://lunarwing:${pg_password}@127.0.0.1:${pg_port}/lunarwing
DATABASE_SSLMODE=disable
PGSSLMODE=disable

# LLM — OpenAI-compatible endpoint (defaults to the local TensorZero proxy)
LLM_BACKEND=openai_compatible
LLM_BASE_URL=${llm_base_url_effective}
LLM_API_KEY=${llm_api_key:-token-${name}}
LLM_MODEL=$llm_model_effective
ALLOW_PRIVATE_IPS=1
# Circuit breaker: fast-fail once the LLM backend is degraded, then auto-probe
# to recover. Guards against a sick gateway amplifying transient blips into
# tenant-wide slowness. Backend-agnostic (wraps whatever provider is built).
LLM_CIRCUIT_BREAKER_THRESHOLD=$cb_threshold
LLM_CIRCUIT_BREAKER_RECOVERY_SECS=$cb_recovery

# Runtime identity
AGENT_NAME=$name
LUNARWING_OWNER_ID=$name
SECRETS_MASTER_KEY=$secrets_key

# Multi-agent coordination (disabled until channels are configured explicitly)
COORD_ENABLED=$coord_enabled
COORD_AGENT_ID=$coord_agent_id
COORD_AGENT_ALIASES=$coord_agent_aliases
COORD_CHANNELS=$coord_channels
COORD_ADMIN_USERS=$coord_admin_users
COORD_ADMIN_TOKEN=$coord_admin_token
COORD_ADMIN_PORT=$coord_admin_port

# XMPP
XMPP_BRIDGE_URL=http://127.0.0.1:${bridge_port}
XMPP_BRIDGE_TOKEN=$bridge_token
XMPP_JID=$xmpp_jid
XMPP_PASSWORD=$xmpp_password
XMPP_DM_POLICY=allowlist
XMPP_ALLOW_FROM=$xmpp_allow_from_effective
XMPP_ALLOW_ROOMS=
XMPP_ENCRYPTED_ROOMS=
XMPP_OMEMO_DEVICE_ID=0
XMPP_OMEMO_STORE_DIR=$state_dir/xmpp
XMPP_ALLOW_PLAINTEXT_FALLBACK=true
XMPP_RESOURCE=$name

# WASM
WASM_ENABLED=true
WASM_CHANNELS_ENABLED=true
WASM_TOOLS_DIR=$state_dir/tools
WASM_CHANNELS_DIR=$state_dir/channels

# Gateway
GATEWAY_ENABLED=true
GATEWAY_HOST=$gateway_host_effective
GATEWAY_PORT=$gateway_port
GATEWAY_AUTH_TOKEN=$gateway_token

# HTTP webhook (bound to localhost only; secret-protected)
HTTP_HOST=127.0.0.1
HTTP_PORT=$http_port
HTTP_WEBHOOK_SECRET=$webhook_secret

# Orchestrator (sandbox container callback)
ORCHESTRATOR_PORT=$orchestrator_port

# Opencode worker (WebSocket port for agent communication)
OPENCODE_WSS_PORT=$opencode_wss_port

# Kimi worker (WebSocket port for agent communication)
KIMI_WSS_PORT=$kimi_wss_port

# Pebble worker (WebSocket port for agent communication)
PEBBLE_WSS_PORT=$pebble_wss_port

# WeeChat relay + adapter
# RELAY_URL / WS_ADAPTER_URL are full URLs consumed by the in-process WASM
# channel (via the capabilities 'env' source). ADAPTER_PORT/WEECHAT_ADAPTER_PORT
# are the bare port consumed by the standalone ws_adapter.py process.
RELAY_URL=http://127.0.0.1:${weechat_port}
RELAY_PASSWORD=$relay_password
ADAPTER_PORT=$weechat_adapter_port
WEECHAT_ADAPTER_PORT=$weechat_adapter_port
WS_ADAPTER_URL=http://127.0.0.1:${weechat_adapter_port}

# Engine V2 (parallel execution path with streaming, gates, missions)
ENGINE_V2=true

# Daemon mode
CLI_ENABLED=false
ONBOARD_COMPLETED=true
HEARTBEAT_ENABLED=false
RUST_LOG=lunarwing=info
ENVEOF
  )
  if tenant_darkirc_enabled "$name"; then
    local darkirc_adapter_port
    darkirc_adapter_port="$(ports_get "$name" darkirc_adapter)" || true
    darkirc_adapter_secret="${darkirc_adapter_secret:-$(generate_token | cut -c1-32)}"
    printf '\nDARKIRC_ADAPTER_URL=http://127.0.0.1:%s\nDARKIRC_ADAPTER_SECRET=%s\n' \
      "$darkirc_adapter_port" "$darkirc_adapter_secret" >> "$path"
  fi

  # LunarVision OCR/vision sidecar
  local vision_port vision_token
  vision_port="$(ports_get "$name" vision_service)" || true
  if [[ -n "$vision_port" ]]; then
    vision_token="$(_env_existing "$(tenant_env_dir "$name")/vision.env" LUNARWING_AUTH_TOKEN)"
    vision_token="${vision_token:-$(generate_token)}"
    printf '\nVISION_SERVICE_URL=http://127.0.0.1:%s\nVISION_AUTH_TOKEN=%s\n' \
      "$vision_port" "$vision_token" >> "$path"
  fi
  # LunarVoice is provisioned but disabled until explicitly enabled. Preserve
  # any operator-selected cloud transcription provider on re-render.
  local voice_port voice_token
  voice_port="$(ports_get "$name" voice_service)" || true
  if [[ -n "$voice_port" ]]; then
    voice_token="$(_env_existing "$(tenant_env_dir "$name")/voice.env" VOICE_AUTH_TOKEN)"
    voice_token="${voice_token:-$(generate_token)}"
    printf '\nVOICE_SERVICE_URL=http://127.0.0.1:%s\nVOICE_AUTH_TOKEN=%s\n' \
      "$voice_port" "$voice_token" >> "$path"
    printf 'TRANSCRIPTION_ENABLED=%s\nTRANSCRIPTION_PROVIDER=%s\nTRANSCRIPTION_MODEL=%s\n' \
      "$transcription_enabled" "$transcription_provider" "$transcription_model" >> "$path"
  fi
  [[ -n "$opencode_model" ]]    && printf '\nOPENCODE_MODEL=%s\n'    "$opencode_model"     >> "$path"
  [[ -n "$opencode_base_url" ]] && printf 'OPENCODE_BASE_URL=%s\n' "$opencode_base_url" >> "$path"
  [[ -n "$kimi_model" ]]        && printf '\nKIMI_MODEL=%s\n'        "$kimi_model"        >> "$path"
  [[ -n "$kimi_base_url" ]]     && printf 'KIMI_BASE_URL=%s\n'     "$kimi_base_url"     >> "$path"
  [[ -n "$kimi_api_key" ]] \
    && printf 'KIMI_API_KEY=%s\n' "$(_quote_env_value "$kimi_api_key")" >> "$path"
  if [[ -n "$study_notes_home" ]]; then
    printf '\n# Shared LunarWing Study Notes MCP\nLW2STUDYMCP_HOME=%s\n' \
      "$study_notes_home" >> "$path"
    [[ -z "$study_notes_state_home" ]] \
      || printf 'LW2STUDYMCP_STATE_HOME=%s\n' "$study_notes_state_home" >> "$path"
    [[ -z "$study_notes_allow_write" ]] \
      || printf 'LW2STUDYMCP_ALLOW_WRITE=%s\n' "$study_notes_allow_write" >> "$path"
  fi
  chown "$name:$name" "$path"
  say "wrote: $path"
}

write_tenant_bridge_env() {
  local name="$1"
  # Same preservation rule as write_tenant_lunarwing_env: keep the daemon's JID
  # rather than resetting this file to the xmpp.localhost default on a re-run.
  local xmpp_jid
  xmpp_jid="$(resolve_tenant_xmpp_jid "$name" "${2:-}")"
  local xmpp_password="${3:-}"
  local xmpp_allow_from="${4:-}"

  local path bridge_port
  path="$(tenant_env_dir "$name")/xmpp-bridge.env"
  bridge_port="$(ports_get "$name" bridge)"

  local state_dir bridge_token
  state_dir="$(tenant_state_dir "$name")"
  bridge_token="$(_read_env_value "$(tenant_env_dir "$name")/lunarwing.env" XMPP_BRIDGE_TOKEN)"
  [[ -n "$bridge_token" ]] || bridge_token="$(generate_token | cut -c1-32)"

  local xmpp_pass_val
  xmpp_pass_val="$(_read_env_value "$(tenant_env_dir "$name")/lunarwing.env" XMPP_PASSWORD)"
  [[ -n "$xmpp_pass_val" ]] || xmpp_pass_val="${xmpp_password:-$(generate_token | cut -c1-32)}"

  # Idempotent XMPP allow-from JSON: an explicit flag wins; else re-derive from
  # the daemon's XMPP_ALLOW_FROM (CSV) if present so re-runs without the flag
  # keep the operator-set list in sync; else fall back to owner JID only.
  local xmpp_allow_from_json_effective
  if [[ -n "$xmpp_allow_from" ]]; then
    xmpp_allow_from_json_effective="$(build_xmpp_allow_from_json "$xmpp_jid" "$xmpp_allow_from")"
  else
    local daemon_csv
    daemon_csv="$(_read_env_value "$(tenant_env_dir "$name")/lunarwing.env" XMPP_ALLOW_FROM)"
    if [[ -n "$daemon_csv" && "$daemon_csv" != "$xmpp_jid" ]]; then
      # Daemon has extras: rebuild JSON from owner + the extras (everything
      # after the leading owner entry, which build_xmpp_allow_from re-prepends).
      local extras="${daemon_csv#$xmpp_jid}"
      extras="${extras#,}"   # strip a single leading comma if present
      xmpp_allow_from_json_effective="$(build_xmpp_allow_from_json "$xmpp_jid" "$extras")"
    else
      xmpp_allow_from_json_effective="$(build_xmpp_allow_from_json "$xmpp_jid" "")"
    fi
  fi

  (
    umask 077
    cat >"$path" <<ENVEOF
LUNARWING_BASE_DIR=$state_dir
IRONCLAW_BASE_DIR=$state_dir
XMPP_BRIDGE_BIND=127.0.0.1:${bridge_port}
XMPP_BRIDGE_TOKEN=$bridge_token
XMPP_BRIDGE_MAX_MESSAGES=256
RUST_LOG=xmpp_bridge=info,info

XMPP_JID=$xmpp_jid
XMPP_PASSWORD=$xmpp_pass_val
XMPP_DM_POLICY=allowlist
XMPP_ALLOW_FROM_JSON=$xmpp_allow_from_json_effective
XMPP_ALLOW_ROOMS_JSON=[]
XMPP_ENCRYPTED_ROOMS_JSON=[]
XMPP_DEVICE_ID=0
XMPP_OMEMO_STORE_DIR=$state_dir/xmpp
XMPP_ALLOW_PLAINTEXT_FALLBACK=true
XMPP_RESOURCE=$name
XMPP_BRIDGE_WAIT_SECONDS=15
ENVEOF
  )
  chown "$name:$name" "$path"
  say "wrote: $path"
}

write_tenant_proxy_env() {
  local name="$1"
  local tensorzero_url="${2:-$DEFAULT_TENSORZERO_URL}"

  local path proxy_port
  path="$(tenant_env_dir "$name")/proxy.env"
  proxy_port="$(ports_get "$name" proxy)"

  (
    umask 077
    cat >"$path" <<ENVEOF
PROXY_PORT=$proxy_port
PROXY_BIND=127.0.0.1
TENSORZERO_URL=$tensorzero_url
ENVEOF
  )
  chown "$name:$name" "$path"
  say "wrote: $path"
}

write_tenant_darkirc_adapter_env() {
  local name="$1" lock_owned="false"
  if [[ "$DARKIRC_WRITER_LOCK_HELD" != "true" ]]; then
    darkirc_writer_lock "$name"
    lock_owned="true"
  fi

  local path adapter_port darkirc_irc_port lunarwing_env adapter_secret
  path="$(tenant_env_dir "$name")/darkirc-adapter.env"
  lunarwing_env="$(tenant_env_dir "$name")/lunarwing.env"
  adapter_port="$(ports_get "$name" darkirc_adapter)" || true
  darkirc_irc_port="$(ports_get "$name" darkirc_irc)" || true
  adapter_secret="$(read_tenant_env_value "$name" "$lunarwing_env" DARKIRC_ADAPTER_SECRET)" \
    || die "could not safely read DarkIRC adapter secret for tenant '$name'"
  [[ -n "$adapter_secret" ]] || adapter_secret="$(generate_token | cut -c1-32)"

  if [[ -f "$TEMPLATES_DIR/darkirc-adapter.env.template" ]]; then
    render_template_content \
      "darkirc-adapter.env.template" \
      "TENANT_NAME=$name" \
      "DARKIRC_IRC_PORT=$darkirc_irc_port" \
      "DARKIRC_ADAPTER_PORT=$adapter_port" \
      "ADAPTER_SECRET=$adapter_secret" \
      | write_tenant_file_atomic "$name" "$path" 600
  else
    cat <<ENVEOF | write_tenant_file_atomic "$name" "$path" 600
DARKIRC_HOST=127.0.0.1
DARKIRC_PORT=$darkirc_irc_port
DARKIRC_NICK=${name}-bridge
DARKIRC_USER=$name
DARKIRC_REALNAME="LunarWing DarkIRC Bridge ($name)"
ADAPTER_HOST=127.0.0.1
ADAPTER_PORT=$adapter_port
ADAPTER_SECRET=$adapter_secret
ADAPTER_LOG_LEVEL=INFO
ENVEOF
  fi
  say "wrote: $path"
  if [[ "$lock_owned" == "true" ]]; then
    darkirc_writer_unlock
  fi
}

darkirc_scope_id() {
  local name="$1"
  jq -r --arg name "$name" '.tenants[$name].darkirc_scope_id // empty' \
    "$PORTS_REGISTRY" 2>/dev/null
}

validate_darkirc_scope_id() {
  local scope_id="$1"
  [[ "$scope_id" =~ ^[0-9a-f]{32}$ && "$scope_id" != "00000000000000000000000000000000" ]] \
    || die "invalid DarkIRC scope ID"
}

darkirc_writer_lock() {
  local name="$1" scope_id lock_file mode owner links enabled current_scope current_enabled
  [[ "$PORTS_LOCK_HELD" != "true" ]] \
    || die "DarkIRC writer lock must be acquired before registry lock"
  acquire_ports_lock
  scope_id="$(darkirc_scope_id "$name")"
  enabled="$(jq -r --arg name "$name" '.tenants[$name].enable_darkirc // false' "$PORTS_REGISTRY" 2>/dev/null || true)"
  release_ports_lock
  [[ "$enabled" == true ]] || die "DarkIRC is disabled for tenant '$name'"
  validate_darkirc_scope_id "$scope_id"
  if [[ "$DARKIRC_WRITER_LOCK_HELD" == "true" ]]; then
    [[ "$DARKIRC_WRITER_SCOPE" == "$scope_id" ]] \
      || die "cannot nest DarkIRC writer locks for different tenants"
    return 0
  fi
  require_cmd flock
  darkirc_path_components_safe "$(dirname "$DARKIRC_WRITER_LOCK_DIR")" \
    || die "unsafe DarkIRC writer lock parent"
  install -d -o root -g root -m 0700 "$DARKIRC_WRITER_LOCK_DIR"
  mode="$(stat -c '%a' "$DARKIRC_WRITER_LOCK_DIR" 2>/dev/null || true)"
  owner="$(stat -c '%u' "$DARKIRC_WRITER_LOCK_DIR" 2>/dev/null || true)"
  [[ "$mode" == 700 && "$owner" == 0 && ! -L "$DARKIRC_WRITER_LOCK_DIR" ]] \
    || die "unsafe DarkIRC writer lock directory"
  lock_file="$DARKIRC_WRITER_LOCK_DIR/$scope_id.lock"
  [[ ! -L "$lock_file" && ( ! -e "$lock_file" || -f "$lock_file" ) ]] \
    || die "unsafe DarkIRC writer lock file"
  if [[ ! -e "$lock_file" ]]; then
    (umask 077; : >"$lock_file")
  fi
  chmod 0600 "$lock_file"
  owner="$(stat -c '%u' "$lock_file" 2>/dev/null || true)"
  mode="$(stat -c '%a' "$lock_file" 2>/dev/null || true)"
  links="$(stat -c '%h' "$lock_file" 2>/dev/null || true)"
  [[ "$owner" == 0 && "$mode" == 600 && "$links" == 1 ]] \
    || die "unsafe DarkIRC writer lock file"
  if [[ -n "${LUNARWING_MT_INHERITED_DARKIRC_LOCK_FD:-}" ]]; then
    local inherited_fd="$LUNARWING_MT_INHERITED_DARKIRC_LOCK_FD"
    local inherited_scope="${LUNARWING_MT_INHERITED_DARKIRC_LOCK_SCOPE:-}"
    [[ "$inherited_scope" == "$scope_id" ]] \
      || die "invalid inherited DarkIRC writer lock"
    inherited_lock_fd_is_held "$inherited_fd" "$lock_file" \
      || die "inherited DarkIRC writer lock does not match tenant scope"
    DARKIRC_WRITER_LOCK_FD="$inherited_fd"
    DARKIRC_WRITER_LOCK_INHERITED="true"
  else
    exec {DARKIRC_WRITER_LOCK_FD}>"$lock_file"
    flock -x -w 10 "$DARKIRC_WRITER_LOCK_FD" \
      || die "DarkIRC config update is busy for tenant '$name'"
  fi
  acquire_ports_lock
  current_scope="$(darkirc_scope_id "$name")"
  current_enabled="$(jq -r --arg name "$name" '.tenants[$name].enable_darkirc // false' "$PORTS_REGISTRY" 2>/dev/null || true)"
  release_ports_lock
  [[ "$current_enabled" == true && "$current_scope" == "$scope_id" ]] \
    || die "DarkIRC tenant identity changed while acquiring writer lock"
  DARKIRC_WRITER_LOCK_HELD="true"
  DARKIRC_WRITER_SCOPE="$scope_id"
}

darkirc_writer_unlock() {
  [[ "$DARKIRC_WRITER_LOCK_HELD" == "true" ]] || return 0
  if [[ "$DARKIRC_WRITER_LOCK_INHERITED" != "true" ]]; then
    flock -u "$DARKIRC_WRITER_LOCK_FD"
    eval "exec ${DARKIRC_WRITER_LOCK_FD}>&-"
  fi
  DARKIRC_WRITER_LOCK_HELD="false"
  DARKIRC_WRITER_LOCK_FD=""
  DARKIRC_WRITER_SCOPE=""
  DARKIRC_WRITER_LOCK_INHERITED="false"
}

with_darkirc_writer_lock() {
  local name="$1" result=0
  shift
  darkirc_writer_lock "$name"
  "$@" || result=$?
  darkirc_writer_unlock
  return "$result"
}

darkirc_load_compatibility() {
  require_cmd jq
  require_cmd sha256sum
  darkirc_resolve_release || die "DarkIRC release selector is missing or unsafe"
  darkirc_sync_release_durability "$DARKIRC_PINNED_RELEASE" \
    || die "DarkIRC release durability repair failed"
  darkirc_fsync_path "$DARKIRC_RELEASE_ROOT" \
    || die "DarkIRC selector durability repair failed"
  local attestation="$DARKIRC_PINNED_RELEASE/attestation.json"
  local shape schema release_id profile digest helper_digest helper_source_revision
  local key_format source_revision expected_release_id actual helper_actual
  shape="$(jq -r 'if type == "object" and
    (keys == ["daemon_sha256","helper_sha256","helper_source_revision","key_format","profile_id","release_id","schema","source_revision"])
    then "valid" else empty end' "$attestation" 2>/dev/null || true)"
  schema="$(jq -r '.schema // empty' "$attestation" 2>/dev/null || true)"
  release_id="$(jq -r '.release_id // empty' "$attestation" 2>/dev/null || true)"
  profile="$(jq -r '.profile_id // empty' "$attestation" 2>/dev/null || true)"
  digest="$(jq -r '.daemon_sha256 // empty' "$attestation" 2>/dev/null || true)"
  helper_digest="$(jq -r '.helper_sha256 // empty' "$attestation" 2>/dev/null || true)"
  helper_source_revision="$(jq -r '.helper_source_revision // empty' "$attestation" 2>/dev/null || true)"
  key_format="$(jq -r '.key_format // empty' "$attestation" 2>/dev/null || true)"
  source_revision="$(jq -r '.source_revision // empty' "$attestation" 2>/dev/null || true)"
  expected_release_id="${source_revision}-${helper_source_revision:0:16}-${digest:7:16}-${helper_digest:7:16}"
  [[ "$shape" == valid \
    && "$schema" == "lunarwing.darkirc-compat/v2" \
    && "$release_id" == "${DARKIRC_PINNED_RELEASE##*/}" \
    && "$profile" == "darkfi-a05956d41-chacha-v1" \
    && "$key_format" == "darkfi-chacha-base58-32" \
    && "$source_revision" == "$DARKIRC_APPROVED_REV" \
    && "$helper_source_revision" =~ ^[0-9a-f]{40}$ \
    && "$digest" =~ ^sha256:[0-9a-f]{64}$ \
    && "$helper_digest" =~ ^sha256:[0-9a-f]{64}$ \
    && "$release_id" == "$expected_release_id" ]] \
    || die "unsupported DarkIRC compatibility attestation"
  actual="$(sha256sum "$DARKIRC_PINNED_RELEASE/darkirc")"
  actual="sha256:${actual%% *}"
  [[ "$actual" == "$digest" ]] || die "DarkIRC binary digest does not match attestation"
  helper_actual="$(sha256sum "$DARKIRC_PINNED_RELEASE/lunarwing-darkirc-key-helper")"
  helper_actual="sha256:${helper_actual%% *}"
  [[ "$helper_actual" == "$helper_digest" ]] \
    || die "DarkIRC helper digest does not match attestation"
  DARKIRC_PINNED_HELPER="$DARKIRC_PINNED_RELEASE/lunarwing-darkirc-key-helper"
  DARKIRC_PINNED_HELPER_SOURCE_REVISION="$helper_source_revision"
  DARKIRC_COMPAT_ARGS=(
    --generator-profile "$profile"
    --binary-sha256 "$digest"
    --binary-path "$DARKIRC_PINNED_RELEASE/darkirc"
    --helper-sha256 "$helper_digest"
    --release-id "$release_id"
    --key-format "$key_format"
    --source-revision "$source_revision"
  )
}

darkirc_release_matches_current_helper_source() {
  local current_revision
  darkirc_validate_clean_root_checkout "$LUNARWING_ROOT" || return 1
  current_revision="$(darkirc_current_helper_source_revision)" || return 1
  [[ "${DARKIRC_PINNED_HELPER_SOURCE_REVISION:-}" == "$current_revision" ]]
}

run_darkirc_manifest_validator() {
  local unbound="${1:-false}"
  darkirc_load_compatibility
  if [[ "$unbound" == "true" ]]; then
    "$DARKIRC_PINNED_HELPER" validate-migration --json --unbound
  else
    "$DARKIRC_PINNED_HELPER" validate-migration --json "${DARKIRC_COMPAT_ARGS[@]}"
  fi
}

run_darkirc_key_helper() {
  local name="$1"
  shift
  darkirc_load_compatibility
  local uid gid home
  uid="$(id -u "$name")" || die "cannot resolve uid for tenant '$name'"
  gid="$(id -g "$name")" || die "cannot resolve gid for tenant '$name'"
  home="$(tenant_home "$name")"
  (
    cd "$home"
    exec sudo -u "$name" env -i HOME="$home" PATH="/usr/bin:/bin" \
      "$DARKIRC_PINNED_HELPER" "$@" "${DARKIRC_COMPAT_ARGS[@]}" \
      --expected-uid "$uid" --expected-gid "$gid"
  )
}

darkirc_contact_command() {
  local operation="${1:-}" name="${2:-}"
  if [[ "$operation" == doctor && "$name" == --all ]]; then
    [[ $# -eq 2 ]] || die "darkirc-contact doctor --all accepts no extra arguments"
    darkirc_contact_doctor_all
    return
  fi
  [[ -n "$operation" && -n "$name" ]] \
    || die "usage: darkirc-contact <operation> <tenant> [contact] [options]"
  shift 2
  name="$(sanitize_name "$name")"
  case "$operation" in
    list|status|doctor|exchanges)
      _darkirc_contact_command_locked "$operation" "$name" "$@"
      ;;
    *)
      with_tenant_lifecycle_lock "$name" _darkirc_contact_command_locked \
        "$operation" "$name" "$@"
      ;;
  esac
}

restore_darkirc_migration_locked() {
  local name="$1" scope_id="$2"
  run_darkirc_key_helper "$name" stage-migration \
    --tenant "$name" --scope-id "$scope_id"
  run_darkirc_key_helper "$name" import-migration \
    --tenant "$name" --scope-id "$scope_id"
}

_darkirc_contact_command_locked() {
  local operation="$1" name="$2" contact=""
  shift 2
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  tenant_darkirc_enabled "$name" || die "DarkIRC is disabled for tenant '$name'"
  local scope_id
  scope_id="$(darkirc_scope_id "$name")"
  validate_darkirc_scope_id "$scope_id"

  local -a helper_args=("$operation" --tenant "$name" --scope-id "$scope_id")
  case "$operation" in
    status)
      contact="${1:-}"
      [[ -n "$contact" && $# -eq 1 ]] || die "darkirc-contact status requires exactly one contact name"
      helper_args+=(--contact "$contact" --json)
      ;;
    prepare)
      contact="${1:-}"
      [[ -n "$contact" ]] || die "darkirc-contact prepare requires contact name"
      shift
      darkirc_exchange_prepare_args helper_args "$contact" "$@"
      ;;
    respond)
      contact="${1:-}"
      [[ -n "$contact" ]] || die "darkirc-contact respond requires contact name"
      shift
      darkirc_exchange_respond_args helper_args "$contact" "$@"
      ;;
    complete)
      contact="${1:-}"
      [[ -n "$contact" ]] || die "darkirc-contact complete requires contact name"
      shift
      darkirc_exchange_complete_args helper_args "$contact" "$@"
      ;;
    cancel)
      darkirc_exchange_cancel_args helper_args "$@"
      ;;
    list|doctor|exchanges)
      [[ $# -eq 0 ]] || die "darkirc-contact $operation accepts no extra arguments"
      helper_args+=(--json)
      ;;
    adopt)
      [[ $# -eq 1 && "$1" == --yes ]] || die "adoption requires --yes"
      helper_args+=(--yes --json)
      ;;
    recover|migration-ready|export-migration|import-migration)
      [[ $# -eq 0 ]] || die "darkirc-contact $operation accepts no extra arguments"
      helper_args+=(--json)
      ;;
    stage-migration)
      [[ $# -eq 2 && "$1" == --in && "$2" == - ]] \
        || die "stage-migration requires --in -"
      ;;
    restore-migration)
      [[ $# -eq 0 ]] || die "restore-migration accepts no extra arguments"
      ;;
    *) die "unsupported DarkIRC contact operation '$operation'" ;;
  esac

  if [[ "$operation" == restore-migration ]]; then
    with_darkirc_writer_lock "$name" restore_darkirc_migration_locked "$name" "$scope_id"
    return 0
  fi

  case "$operation" in
    adopt|prepare|respond|complete|cancel|recover|migration-ready|export-migration|stage-migration|import-migration)
      with_darkirc_writer_lock "$name" run_darkirc_key_helper "$name" "${helper_args[@]}"
      ;;
    list|status|doctor|exchanges)
      run_darkirc_key_helper "$name" "${helper_args[@]}"
      ;;
  esac
}

darkirc_exchange_prepare_args() {
  # shellcheck disable=SC2178  # args_ref is intentionally an array nameref.
  local -n args_ref="$1"
  local contact="$2" out="" expires="" json=false
  shift 2
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --out) [[ $# -ge 2 && -z "$out" ]] || die "prepare requires one --out value"; out="$2"; shift 2 ;;
      --expires) [[ $# -ge 2 && -z "$expires" ]] || die "prepare accepts one --expires value"; expires="$2"; shift 2 ;;
      --json) [[ "$json" == false ]] || die "duplicate --json"; json=true; shift ;;
      *) die "unknown prepare argument: $1" ;;
    esac
  done
  [[ -n "$out" ]] || die "prepare requires --out <file|->"
  args_ref+=(--contact "$contact" --out "$out")
  [[ -z "$expires" ]] || args_ref+=(--expires "$expires")
  [[ "$json" == false ]] || args_ref+=(--json)
}

darkirc_exchange_respond_args() {
  # shellcheck disable=SC2178  # args_ref is intentionally an array nameref.
  local -n args_ref="$1"
  local contact="$2" input="" out="" fingerprint="" expires="" json=false
  shift 2
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --in) [[ $# -ge 2 && -z "$input" ]] || die "respond requires one --in value"; input="$2"; shift 2 ;;
      --out) [[ $# -ge 2 && -z "$out" ]] || die "respond requires one --out value"; out="$2"; shift 2 ;;
      --expect-peer-fingerprint) [[ $# -ge 2 && -z "$fingerprint" ]] || die "respond requires one fingerprint"; fingerprint="$2"; shift 2 ;;
      --expires) [[ $# -ge 2 && -z "$expires" ]] || die "respond accepts one --expires value"; expires="$2"; shift 2 ;;
      --json) [[ "$json" == false ]] || die "duplicate --json"; json=true; shift ;;
      *) die "unknown respond argument: $1" ;;
    esac
  done
  [[ -n "$input" && -n "$out" && -n "$fingerprint" ]] \
    || die "respond requires --in, --out, and --expect-peer-fingerprint"
  args_ref+=(--contact "$contact" --in "$input" --out "$out" --expect-peer-fingerprint "$fingerprint")
  [[ -z "$expires" ]] || args_ref+=(--expires "$expires")
  [[ "$json" == false ]] || args_ref+=(--json)
}

darkirc_exchange_complete_args() {
  # shellcheck disable=SC2178  # args_ref is intentionally an array nameref.
  local -n args_ref="$1"
  local contact="$2" input="" exchange_id="" fingerprint="" json=false
  shift 2
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --in) [[ $# -ge 2 && -z "$input" ]] || die "complete requires one --in value"; input="$2"; shift 2 ;;
      --exchange-id) [[ $# -ge 2 && -z "$exchange_id" ]] || die "complete requires one exchange ID"; exchange_id="$2"; shift 2 ;;
      --expect-peer-fingerprint) [[ $# -ge 2 && -z "$fingerprint" ]] || die "complete requires one fingerprint"; fingerprint="$2"; shift 2 ;;
      --defer-apply) shift ;;
      --json) [[ "$json" == false ]] || die "duplicate --json"; json=true; shift ;;
      *) die "unknown complete argument: $1" ;;
    esac
  done
  [[ -n "$input" && -n "$exchange_id" && -n "$fingerprint" ]] \
    || die "complete requires --in, --exchange-id, and --expect-peer-fingerprint"
  args_ref+=(--contact "$contact" --in "$input" --exchange-id "$exchange_id" \
    --expect-peer-fingerprint "$fingerprint" --defer-apply)
  [[ "$json" == false ]] || args_ref+=(--json)
}

darkirc_exchange_cancel_args() {
  # shellcheck disable=SC2178  # args_ref is intentionally an array nameref.
  local -n args_ref="$1"
  local exchange_id="" json=false
  shift
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --exchange-id) [[ $# -ge 2 && -z "$exchange_id" ]] || die "cancel requires one exchange ID"; exchange_id="$2"; shift 2 ;;
      --json) [[ "$json" == false ]] || die "duplicate --json"; json=true; shift ;;
      *) die "unknown cancel argument: $1" ;;
    esac
  done
  [[ -n "$exchange_id" ]] || die "cancel requires --exchange-id"
  args_ref+=(--exchange-id "$exchange_id")
  [[ "$json" == false ]] || args_ref+=(--json)
}

darkirc_contact_doctor_all() {
  local name
  while IFS= read -r name; do
    [[ -n "$name" ]] || continue
    if tenant_darkirc_enabled "$name"; then
      _darkirc_contact_command_locked doctor "$name"
    fi
  done < <(all_tenant_names)
}

darkirc_prepare_tenant_dirs() {
  local name="$1" state_dir config_dir datastore_dir path
  state_dir="$(tenant_state_dir "$name")"
  config_dir="$state_dir/darkirc"
  datastore_dir="$config_dir/datastore"
  for path in "$state_dir" "$config_dir" "$datastore_dir"; do
    darkirc_path_components_safe "$path" \
      || die "unsafe DarkIRC state path for tenant '$name': $path"
  done
  sudo -u "$name" mkdir -p "$config_dir" "$datastore_dir" \
    || die "could not create DarkIRC state directories for '$name'"
  sudo -u "$name" chmod 0700 "$config_dir" "$datastore_dir" \
    || die "could not secure DarkIRC state directories for '$name'"
  for path in "$state_dir" "$config_dir" "$datastore_dir"; do
    darkirc_path_components_safe "$path" \
      || die "unsafe DarkIRC state path for tenant '$name': $path"
  done
  [[ -d "$config_dir" && ! -L "$config_dir" \
    && -d "$datastore_dir" && ! -L "$datastore_dir" ]] \
    || die "DarkIRC state path is not a regular directory for '$name'"
}

generate_darkirc_config() {
  local name="$1" state_dir config_dir irc_port rpc_port log_dir scope_id baseline
  local registry_lock_owned="false" writer_lock_owned="false"
  state_dir="$(tenant_state_dir "$name")"
  config_dir="$state_dir/darkirc"

  if [[ "$DARKIRC_WRITER_LOCK_HELD" != "true" ]]; then
    darkirc_writer_lock "$name"
    writer_lock_owned="true"
  fi
  if [[ "$PORTS_LOCK_HELD" != "true" ]]; then
    acquire_ports_lock
    registry_lock_owned="true"
  fi
  scope_id="$(ensure_darkirc_scope_id "$name")"
  irc_port="$(ports_get "$name" darkirc_irc)" || die "no darkirc_irc port for tenant '$name'"
  rpc_port="$(ports_get "$name" darkirc_rpc)" || die "no darkirc_rpc port for tenant '$name'"
  log_dir="$(tenant_log_dir "$name")"

  # Keep the registry snapshot locked through the semantic config commit so its
  # scope and target ports cannot diverge from the generated baseline.
  darkirc_prepare_tenant_dirs "$name"
  [[ -f "$TEMPLATES_DIR/darkirc_config.toml.template" ]] \
    || die "darkirc config template not found at $TEMPLATES_DIR/darkirc_config.toml.template"
  baseline="$(render_template_content \
    "darkirc_config.toml.template" \
    "TENANT_NAME=$name" \
    "IRC_PORT=$irc_port" \
    "RPC_PORT=$rpc_port" \
    "CONFIG_DIR=$config_dir" \
    "LOG_DIR=$log_dir")"
  printf '%s' "$baseline" | run_darkirc_key_helper "$name" rewrite-baseline \
    --tenant "$name" --scope-id "$scope_id" >/dev/null \
    || die "DarkIRC semantic config update failed for tenant '$name'"
  [[ -f "$config_dir/darkirc_config.toml" && ! -L "$config_dir/darkirc_config.toml" ]] \
    || die "DarkIRC helper did not produce a config for '$name'"
  if [[ "$registry_lock_owned" == "true" ]]; then
    release_ports_lock
  fi
  if [[ "$writer_lock_owned" == "true" ]]; then
    darkirc_writer_unlock
  fi
  say "darkirc config updated for $name (irc=$irc_port rpc=$rpc_port scope=$scope_id)"
}

# ── External-worker config.toml generation ────────────────────────────────────
#
# External workers speak the lunarwing-agent-v1 WebSocket
# protocol (legacy alias ironclaw-agent-v1 is still accepted for back-compat)
# and are routed by the agent's `create_job(mode: "<worker>")` tool.
# The daemon discovers them from `[[sandbox.external_workers]]` blocks in
# `config.toml` under the tenant's LUNARWING_BASE_DIR (the state dir). Without
# this block the agent has nothing to route `create_job(mode: "<worker>")` to
# and never logs `External workers configured: <worker>` on startup.
#
# The worker container binds 127.0.0.1:<wss_port> (path /ws/agent) and is
# launched with AGENT_AUTH_TOKEN set to the tenant's GATEWAY_AUTH_TOKEN, so the
# daemon side must present that same token as the
# WebSocket bearer — hence auth_token mirrors GATEWAY_AUTH_TOKEN here.
#
# Idempotent: a tenant entry is written once and skipped on re-runs. Exact legacy
# 5-minute deadlines migrate to the current default. Set
# LUNARWING_MT_MIGRATE_LEGACY_EXTERNAL_WORKER_TIMEOUTS=false to preserve an
# intentional 5-minute value. Other explicit values remain untouched.

_nanocode_openrc_service_known() {
  local svc="$1" link
  [[ -e "$OPENRC_INITD_DIR/$svc" || -e "$OPENRC_CONFD_DIR/$svc" ]] && return 0
  for link in "$OPENRC_RUNLEVELS_DIR"/*/"$svc"; do
    [[ -e "$link" || -L "$link" ]] && return 0
  done
  return 1
}

_stop_nanocode_supervision() {
  local name="$1"
  local unit="lunarwing-nanocode-${name}.service"
  local container="lunarwing-nanocode-${name}" load_state="" user_unit_dir
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    user_unit_dir="$(tenant_home "$name")/.config/systemd/user"
    if id -u "$name" >/dev/null 2>&1; then
      load_state="$(_systemctl_user "$name" show --value -p LoadState "$unit" 2>/dev/null || true)"
      if [[ -e "$(tenant_quadlet_dir "$name")/${container}.container" \
          || -e "$user_unit_dir/$unit" || "$load_state" == "loaded" ]]; then
        _systemctl_user "$name" stop "$unit" >/dev/null 2>&1 \
          || { say "failed to stop legacy $unit" >&2; return 1; }
        if _systemctl_user "$name" is-active --quiet "$unit" 2>/dev/null; then
          say "legacy $unit remained active after stop" >&2
          return 1
        fi
      fi
    elif [[ -e "$(tenant_quadlet_dir "$name")/${container}.container" \
        || -e "$user_unit_dir/$unit" ]]; then
      say "tenant user '$name' absent; cannot stop legacy $unit" >&2
      return 1
    fi
    return 0
  fi

  local svc
  for svc in "${container}-sup" "$container"; do
    _nanocode_openrc_service_known "$svc" || continue
    rc-service "$svc" stop >/dev/null 2>&1 \
      || { say "failed to stop legacy OpenRC service $svc" >&2; return 1; }
  done
}

_stop_nanocode_container_store() {
  local label="$1" container="$2"
  shift 2
  local -a runtime=("$@")
  "${runtime[@]}" inspect "$container" >/dev/null 2>&1 || return 0
  if ! "${runtime[@]}" stop --time 30 "$container" >/dev/null 2>&1; then
    say "failed to stop legacy $container in $label" >&2
    return 1
  fi
  if [[ "$("${runtime[@]}" inspect -f '{{.State.Running}}' "$container" 2>/dev/null || true)" == "true" ]]; then
    say "legacy $container still running in $label" >&2
    return 1
  fi
  say "stopped legacy $container in $label"
}

_stop_nanocode_containers() {
  local name="$1"
  local container="lunarwing-nanocode-${name}" uid home
  if command -v docker >/dev/null 2>&1; then
    _stop_nanocode_container_store "root Docker store" "$container" docker || return 1
  fi
  if command -v podman >/dev/null 2>&1; then
    _stop_nanocode_container_store "root Podman store" "$container" podman || return 1
    if id -u "$name" >/dev/null 2>&1; then
      uid="$(id -u "$name")"
      home="$(getent passwd "$name" | cut -d: -f6)"
      _stop_nanocode_container_store "tenant $name rootless Podman store" "$container" \
        sudo -u "$name" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" podman || return 1
    fi
  fi
}

_remove_nanocode_supervision() {
  local name="$1"
  local container="lunarwing-nanocode-${name}" svc link runlevel
  rm -f -- \
    "$(tenant_quadlet_dir "$name")/${container}.container" \
    "$(tenant_home "$name")/.config/systemd/user/${container}.service" \
    "$(tenant_home "$name")/.config/systemd/user/default.target.wants/${container}.service" \
    "$(_hash_dir "$name")/${container}.hash"
  if [[ "$INIT_SYSTEM" == "systemd" ]] && id -u "$name" >/dev/null 2>&1; then
    _systemctl_user "$name" daemon-reload >/dev/null 2>&1 \
      || { say "WARNING: could not reload tenant user manager after Nanocode retirement (non-fatal)" >&2; true; }
  fi

  for svc in "${container}-sup" "$container"; do
    for link in "$OPENRC_RUNLEVELS_DIR"/*/"$svc"; do
      [[ -e "$link" || -L "$link" ]] || continue
      runlevel="$(basename "$(dirname "$link")")"
      if command -v rc-update >/dev/null 2>&1; then
        rc-update del "$svc" "$runlevel" >/dev/null 2>&1 \
          || { say "failed to remove $svc from OpenRC runlevel $runlevel" >&2; return 1; }
      else
        rm -f -- "$link"
      fi
    done
    rm -f -- "$OPENRC_INITD_DIR/$svc" "$OPENRC_CONFD_DIR/$svc"
  done
}

_archive_nanocode_env() {
  local name="$1" env_dir env_path legacy archive backup temp rc=0
  env_dir="$(tenant_env_dir "$name")"
  env_path="$env_dir/lunarwing.env"
  legacy="$env_dir/nanocode.env"
  require_cmd python3
  if [[ -e "$env_path" || -L "$env_path" ]]; then
    backup="$env_path.pre-nanocode-retirement"
    temp="$(mktemp "${env_path}.tmp.XXXXXX")"
    python3 - "$env_path" "$temp" "$backup" <<'PY' || rc=$?
import os
import stat
import sys
from pathlib import Path

source, target, backup = map(Path, sys.argv[1:4])
source_fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW)
source_stat = os.fstat(source_fd)
if not stat.S_ISREG(source_stat.st_mode):
    os.close(source_fd)
    raise RuntimeError("tenant env is not a regular file")
with os.fdopen(source_fd, encoding="utf-8") as handle:
    text = handle.read()
lines = text.splitlines(keepends=True)
if not any(line.startswith("NANOCODE_") for line in lines):
    raise SystemExit(3)
result = "".join(line for line in lines if not line.startswith("NANOCODE_"))

def write_secure(path: Path, content: str, flags: int) -> None:
    fd = os.open(path, flags | os.O_NOFOLLOW, 0o600)
    try:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise RuntimeError(f"unsafe retirement output: {path}")
        os.fchmod(fd, 0o600)
        os.fchown(fd, source_stat.st_uid, source_stat.st_gid)
        with os.fdopen(fd, "w", encoding="utf-8", closefd=False) as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
    finally:
        os.close(fd)

if backup.exists() or backup.is_symlink():
    if not stat.S_ISREG(os.lstat(backup).st_mode):
        raise RuntimeError(f"unsafe retirement backup: {backup}")
else:
    write_secure(backup, text, os.O_WRONLY | os.O_CREAT | os.O_EXCL)
write_secure(target, result, os.O_WRONLY | os.O_TRUNC)
os.replace(target, source)
PY
    if [[ "$rc" -eq 3 ]]; then
      rm -f -- "$temp"
    elif [[ "$rc" -ne 0 ]]; then
      rm -f -- "$temp"
      say "failed to remove retired NANOCODE_* settings from $env_path" >&2
      return 1
    else
      say "removed retired NANOCODE_* settings from $env_path"
    fi
  fi
  if [[ -e "$legacy" || -L "$legacy" ]]; then
    archive="$legacy.retired"
    [[ ! -e "$archive" ]] || archive="$archive.$(date -u +%Y%m%dT%H%M%SZ).$$"
    python3 - "$legacy" "$archive" <<'PY' || return 1
import os
import stat
import sys
from pathlib import Path

source, archive = map(Path, sys.argv[1:3])
fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW)
try:
    source_stat = os.fstat(fd)
    if not stat.S_ISREG(source_stat.st_mode):
        raise RuntimeError("legacy env is not a regular file")
    os.fchmod(fd, 0o600)
    current = os.lstat(source)
    if (current.st_dev, current.st_ino) != (source_stat.st_dev, source_stat.st_ino):
        raise RuntimeError("legacy env changed during retirement")
    os.link(source, archive, follow_symlinks=False)
    os.unlink(source)
finally:
    os.close(fd)
PY
    say "archived legacy Nanocode env as $archive"
  fi
}

_retire_tenant_nanocode() {
  local name="$1"
  _stop_nanocode_supervision "$name" || return 1
  _stop_nanocode_containers "$name" || return 1
  _remove_nanocode_supervision "$name" || return 1
  remove_external_worker_config "$name" nanocode || return 1
  _archive_nanocode_env "$name" || return 1
  ports_retire_nanocode "$name" || return 1
  say "Nanocode retired for tenant '$name' (data, stopped containers, and images preserved)"
}

retire_tenant_nanocode() {
  local name="$1"
  with_tenant_lifecycle_lock "$name" _retire_tenant_nanocode "$name"
}

# ── SSH harness config.toml generation ────────────────────────────────────────
#
# Appends a [[ssh.hosts]] block to the tenant's config.toml so the gateway
# initializes the SSH bridge + agent server at boot. The host defaults to
# localhost (the tenant's own user) for self-referential worker SSH access.
# Keys are stored in the encrypted secrets store (uploaded after the daemon
# starts via upload_tenant_ssh_key), NOT in config.toml.
#
# Idempotent: skips if an SSH host entry already exists in config.toml.
# APPENDS only — never overwrites existing config.toml content.
ensure_ssh_config() {
  local name="$1"
  local ssh_host="${2:-127.0.0.1}"
  local ssh_user="${3:-$name}"
  local state_dir config_path

  state_dir="$(tenant_state_dir "$name")"
  config_path="$state_dir/config.toml"

  # Already configured? Skip so re-runs are idempotent.
  if [[ -f "$config_path" ]] && grep -q '\[\[ssh\.hosts\]\]' "$config_path" 2>/dev/null; then
    say "SSH host config already present in $config_path (skipping)"
    return 0
  fi

  # Create config.toml with a header if it doesn't exist yet (same pattern as
  # ensure_external_worker_config). APPEND only — never truncate.
  if [[ ! -f "$config_path" ]]; then
    sudo -u "$name" mkdir -p "$state_dir"
    (
      umask 077
      printf '# LunarWing tenant configuration (auto-generated by lunarwing-mt-admin.sh).\n'
    ) >"$config_path"
  fi

  # Append the SSH host block. This is valid TOML because [[ssh.hosts]] is an
  # array-of-tables and we only append when no entry exists yet.
  {
    printf '\n# SSH harness — centralized SSH host config for worker integration.\n'
    printf '# Keys are stored in the encrypted secrets store, not on disk.\n'
    printf '[[ssh.hosts]]\n'
    printf 'host = "%s"\n' "$ssh_host"
    printf 'port = 22\n'
    printf 'user = "%s"\n' "$ssh_user"
    printf 'key_type = "ed25519"\n'
    printf 'host_key_mode = "AcceptFirst"\n'
  } >>"$config_path"

  chown "$name:$name" "$config_path"
  chmod 600 "$config_path"
  say "wrote SSH host config to $config_path (host=$ssh_host user=$ssh_user)"
}

# Print the host values of every [[ssh.hosts]] block in the tenant's
# config.toml, one per line. mt-admin writes this file itself (ensure_ssh_config),
# so the shape is known: a `host = "..."` line inside each [[ssh.hosts]] block.
_ssh_hosts_from_config() {
  local name="$1" config_path
  config_path="$(tenant_state_dir "$name")/config.toml"
  [[ -f "$config_path" ]] || return 0
  awk '
    /^\[\[ssh\.hosts\]\]/ { inblk = 1; next }
    /^\[/                 { inblk = 0 }
    inblk && /^host[ ]*=[ ]*"/ {
      line = $0
      sub(/^host[ ]*=[ ]*"/, "", line)
      sub(/".*$/, "", line)
      print line
    }
  ' "$config_path"
}

# Point the installed WASM ssh tool's capability allowlist at the tenant's
# configured [[ssh.hosts]] hosts (the sidecar ships with a "myhost" placeholder).
# Idempotent: always derived from config.toml. Warn-and-continue on any failure.
# Point the installed WASM ssh tool's capability allowlist at the tenant's
# configured [[ssh.hosts]] hosts (the sidecar ships with a "myhost" placeholder).
# Idempotent: always derived from config.toml. Warn-and-continue on any failure:
# every internal step is guarded so a failure can never errexit the script.
patch_ssh_tool_allowlist() {
  local name="$1" caps_path hosts_json tmp
  caps_path="$(tenant_state_dir "$name")/tools/ssh-tool.capabilities.json"

  if [[ ! -f "$caps_path" ]]; then
    say "  ssh-tool allowlist: ssh-tool not installed — nothing to patch"
    return 0
  fi

  command -v jq >/dev/null 2>&1 \
    || { say "WARNING: jq not installed; ssh-tool allowlist left unchanged" >&2; return 0; }

  hosts_json="$(_ssh_hosts_from_config "$name" | jq -R . | jq -s . 2>/dev/null)" \
    || { say "WARNING: could not derive ssh hosts for $name; ssh-tool allowlist left unchanged" >&2; return 0; }
  if [[ -z "$hosts_json" || "$hosts_json" == "[]" ]]; then
    say "  ssh-tool allowlist: no [[ssh.hosts]] in config.toml — leaving sidecar as shipped"
    return 0
  fi

  # Same-directory temp file so mv is atomic (same pattern as
  # configure_gotify_capabilities).
  tmp="$(mktemp "${caps_path}.tmp.XXXXXX")" \
    || { say "WARNING: mktemp failed; ssh-tool allowlist left unchanged" >&2; return 0; }
  if jq --argjson hosts "$hosts_json" '.capabilities.ssh.allowed_hosts = $hosts' \
       "$caps_path" >"$tmp" 2>/dev/null \
    && mv "$tmp" "$caps_path" \
    && chown "$name:$name" "$caps_path"; then
    say "  ssh-tool allowlist set to: $(jq -c . <<<"$hosts_json" 2>/dev/null || printf '%s' "$hosts_json")"
  else
    rm -f "$tmp"
    say "WARNING: failed to patch ssh-tool allowlist at $caps_path (edit .capabilities.ssh.allowed_hosts manually)" >&2
  fi
}

# True if a TCP connect to host:port succeeds within 2s (pure bash /dev/tcp).
_probe_tcp() {
  local host="$1" port="$2"
  timeout 2 bash -c "exec 3<>/dev/tcp/${host}/${port}" 2>/dev/null
}

# Patch the WeeChat WASM channel capabilities with the tenant's allocated ports
# from the registry. The source caps file (channels-src/weechat/*.capabilities.json)
# has generic default ports (9001/6681) that are wrong for every multi-tenant host.
# This is called after install-wasm and on tenant start so the ports are always
# correct. Idempotent: always derived from the registry.
patch_weechat_caps() {
  local name="$1" caps_path weechat_port adapter_port tmp
  caps_path="$(tenant_state_dir "$name")/channels/weechat.capabilities.json"

  if [[ ! -f "$caps_path" ]]; then
    say "  weechat caps: weechat channel not installed — nothing to patch"
    return 0
  fi

  command -v jq >/dev/null 2>&1 \
    || { say "WARNING: jq not installed; weechat caps left unchanged" >&2; return 0; }

  weechat_port="$(ports_get "$name" weechat)" \
    || { say "WARNING: could not read weechat port for $name; caps left unchanged" >&2; return 0; }
  adapter_port="$(ports_get "$name" weechat_adapter)" \
    || { say "WARNING: could not read weechat_adapter port for $name; caps left unchanged" >&2; return 0; }

  local relay_url="http://127.0.0.1:${weechat_port}"
  local adapter_url="http://127.0.0.1:${adapter_port}"

  # Same-directory temp file so mv is atomic.
  tmp="$(mktemp "${caps_path}.tmp.XXXXXX")" \
    || { say "WARNING: mktemp failed; weechat caps left unchanged" >&2; return 0; }
  if jq --arg relay "$relay_url" --arg adapter "$adapter_url" \
       '.config.relay_url = $relay | .config.ws_adapter_url = $adapter' \
       "$caps_path" >"$tmp" 2>/dev/null \
    && mv "$tmp" "$caps_path" \
    && chown "$name:$name" "$caps_path"; then
    say "  weechat caps patched: relay=$relay_url adapter=$adapter_url"
  else
    rm -f "$tmp"
    say "WARNING: failed to patch weechat caps at $caps_path" >&2
  fi
}

# Warn (never fail) if the tenant's configured loopback SSH host has no sshd
# listening. Only 127.0.0.1 entries are probed: remote hosts may legitimately
# be unreachable from this box (firewalls, jump hosts).
warn_if_sshd_unreachable() {
  local name="$1" host
  while IFS= read -r host; do
    [[ "$host" == "127.0.0.1" ]] || continue
    if ! _probe_tcp "$host" 22; then
      say "WARNING: no sshd listening on ${host}:22 — the tenant's SSH tools target this host." >&2
      say "         Enable it with: systemctl enable --now sshd   (or 'ssh' on Debian/Ubuntu)" >&2
    fi
  done < <(_ssh_hosts_from_config "$name")
  # Explicit: this probe is warn-only and must never fail its (bare-statement)
  # callers under set -e, regardless of future edits above.
  return 0
}

# ── SSH key provisioning ──────────────────────────────────────────────────────
#
# Generates an ed25519 key pair for the tenant and adds the public key to the
# tenant's authorized_keys. The private key is staged in the tenant's env dir
# (mode 0600) for upload to the secrets store via upload_tenant_ssh_key after
# the daemon starts. The staged copy is deleted after upload so no key material
# is left on disk.
provision_tenant_ssh_key() {
  local name="$1"
  local ssh_host="${2:-127.0.0.1}"
  local ssh_user="${3:-$name}"
  local tenant_home ssh_dir key_path pubkey_path staged_key

  tenant_home="$(tenant_home "$name")"
  ssh_dir="$tenant_home/.ssh"
  key_path="$ssh_dir/id_ed25519_lunarwing"
  pubkey_path="${key_path}.pub"
  staged_key="$(tenant_env_dir "$name")/ssh_key_staged"

  # Generate a key pair if one doesn't already exist (idempotent).
  if [[ ! -f "$key_path" ]]; then
    sudo -u "$name" mkdir -p "$ssh_dir"
    sudo -u "$name" chmod 700 "$ssh_dir"
    sudo -u "$name" ssh-keygen -t ed25519 -f "$key_path" -N "" \
      -C "lunarwing-ssh-harness-$name" 2>/dev/null
    say "generated ed25519 SSH key pair for $name"
  else
    say "SSH key already exists for $name (reusing)"
  fi

  # Add the public key to authorized_keys (idempotent: skip if already present).
  local authorized_keys="$ssh_dir/authorized_keys"
  sudo -u "$name" touch "$authorized_keys"
  sudo -u "$name" chmod 600 "$authorized_keys"
  if ! sudo -u "$name" grep -qF "$(cat "$pubkey_path" 2>/dev/null)" "$authorized_keys" 2>/dev/null; then
    sudo -u "$name" bash -c "cat '$pubkey_path' >> '$authorized_keys'"
    say "added public key to authorized_keys for $name"
  fi

  # Stage the private key for upload (mode 0600, tenant-owned).
  # Deleted by upload_tenant_ssh_key after the daemon ingests it.
  sudo -u "$name" cp "$key_path" "$staged_key"
  sudo -u "$name" chmod 600 "$staged_key"
  chown "$name:$name" "$staged_key"
  say "staged SSH private key for upload to secrets store"
}

# Upload the staged SSH private key to the secrets store via the gateway's SSH
# API. Called AFTER the daemon starts (the API is served by the gateway on the
# HTTP port). Deletes the staged key after upload so no key material persists
# on disk. Init-system-agnostic — uses HTTP, works on both systemd and OpenRC.
upload_tenant_ssh_key() {
  local name="$1"
  local ssh_host="${2:-127.0.0.1}"
  local ssh_user="${3:-$name}"
  local http_port staged_key

  http_port="$(ports_get "$name" http)"
  staged_key="$(tenant_env_dir "$name")/ssh_key_staged"

  [[ -f "$staged_key" ]] || { say "no staged SSH key for $name (skipping upload)"; return 0; }

  # Wait for the gateway to be reachable (it may still be starting up).
  local i=0
  while ! curl -sf --max-time 2 "http://127.0.0.1:${http_port}/agent/status" >/dev/null 2>&1; do
    i=$((i + 1))
    [[ $i -lt 15 ]] || { say "WARNING: gateway not reachable on port $http_port after 30s; SSH key not uploaded (upload manually via the API)" >&2; return 1; }
    sleep 2
  done

  # Upload the key via the SSH API. The key data is sent as a JSON string
  # (base64 not needed — the API accepts raw PEM/OpenSSH format).
  local key_data upload_result
  key_data="$(cat "$staged_key")"
  upload_result="$(jq -n --arg key "$key_data" '{key_data: $key}' | \
    curl -sf -X POST "http://127.0.0.1:${http_port}/hosts/${ssh_host}/key" \
      -H "Content-Type: application/json" -d @- 2>&1)" || true

  if echo "$upload_result" | jq -e '.success == true' >/dev/null 2>&1; then
    say "SSH key uploaded to secrets store for host $ssh_host"
    # Delete the staged key — it's now in the encrypted secrets store only.
    rm -f "$staged_key"
    say "staged SSH key deleted (key material now only in secrets store)"
  else
    say "WARNING: SSH key upload failed: $upload_result" >&2
    say "         staged key remains at $staged_key (upload manually or re-run)" >&2
  fi
}

# Upload the DarkIRC adapter secret to the secrets store via the gateway's
# extension setup API. Called AFTER the daemon starts (the API is served by the
# gateway on the tenant gateway port). The secret is optional — if absent from the tenant
# env, this is a no-op. Idempotent: ExtensionManager::configure overwrites an
# existing secret and refreshes/activates the WASM channel, so re-running
# start_tenant is safe and no daemon restart is required (the credential is read
# live per-request from the secrets store). Init-system-agnostic — uses HTTP,
# works on both systemd and OpenRC. Best-effort: a failed upload warns and
# continues; the daemon keeps running.
upload_tenant_darkirc_secret() {
  local name="$1"
  local env_path gateway_port gateway_token darkirc_adapter_secret upload_result

  # Only tenants provisioned with DarkIRC carry this secret.
  tenant_darkirc_enabled "$name" || return 0

  env_path="$(tenant_env_dir "$name")/lunarwing.env"

  darkirc_adapter_secret="$(_read_env_value "$env_path" DARKIRC_ADAPTER_SECRET)"
  # The secret is optional — nothing to upload if it is not set.
  [[ -n "$darkirc_adapter_secret" ]] || return 0

  gateway_token="$(_read_env_value "$env_path" GATEWAY_AUTH_TOKEN)"
  if [[ -z "$gateway_token" ]]; then
    say "WARNING: GATEWAY_AUTH_TOKEN not found in $env_path; DarkIRC adapter secret not uploaded (upload manually via the API)" >&2
    return 1
  fi

  gateway_port="$(ports_get "$name" gateway)"

  # Wait for the gateway to be reachable (it may still be starting up).
  if ! _wait_tenant_gateway "$name"; then
    say "WARNING: gateway not reachable on port $gateway_port; DarkIRC adapter secret not uploaded (upload manually via the API)" >&2
    return 1
  fi

  # Upload the secret via the extension setup API. The adapter secret is sent
  # on stdin (never argv), and the gateway auth token is sent via curl -K
  # reading an anonymous process-substitution FD (never argv) so neither
  # credential appears in /proc/<pid>/cmdline or shell history.
  local auth_header="Authorization: Bearer ${gateway_token}"
  upload_result="$(jq -n --arg secret "$darkirc_adapter_secret" \
    '{secrets:{darkirc_adapter_secret:$secret},fields:{}}' | \
    curl -sf -X POST "http://127.0.0.1:${gateway_port}/api/extensions/darkirc/setup" \
      -H "Content-Type: application/json" \
      -K <(printf 'header = \"%s\"\\n' "$auth_header") \
      -d @- 2>&1)" || true

  if echo "$upload_result" | jq -e '.success == true' >/dev/null 2>&1; then
    say "DarkIRC adapter secret uploaded to secrets store"
    return 0
  else
    # Do NOT echo $upload_result: the gateway or adapter may echo the secret
    # back in the error message. Emit a generic warning instead.
    say "WARNING: DarkIRC adapter secret upload failed (upload manually via the API)" >&2
    return 1
  fi
}

patch_tenant_coordination_env() {
  local name="$1" env_path="$2"
  local coord_admin_port coord_admin_token
  chmod 0600 "$env_path"
  coord_admin_port="$(_env_existing "$env_path" COORD_ADMIN_PORT)"
  if [[ -z "$coord_admin_port" ]]; then
    coord_admin_port="$(ports_get "$name" coord_admin || true)"
    [[ -n "$coord_admin_port" ]] || die "coordination admin port missing for tenant '$name'"
  fi
  coord_admin_token="$(_env_existing "$env_path" COORD_ADMIN_TOKEN)"
  [[ -n "$coord_admin_token" ]] || coord_admin_token="$(generate_token)"
  if ! grep -q '^COORD_ENABLED=' "$env_path"; then
    printf '\n# Multi-agent coordination (disabled until channels are configured explicitly)\nCOORD_ENABLED=false\n' >>"$env_path"
    say "added COORD_ENABLED=false to $env_path"
  fi
  if ! grep -q '^COORD_AGENT_ID=' "$env_path"; then
    printf 'COORD_AGENT_ID=%s\n' "$name" >>"$env_path"
    say "added COORD_AGENT_ID=$name to $env_path"
  fi
  if ! grep -q '^COORD_CHANNELS=' "$env_path"; then
    printf 'COORD_CHANNELS=\n' >>"$env_path"
    say "added COORD_CHANNELS to $env_path"
  fi
  if ! grep -q '^COORD_AGENT_ALIASES=' "$env_path"; then
    printf 'COORD_AGENT_ALIASES=\n' >>"$env_path"
    say "added COORD_AGENT_ALIASES to $env_path"
  fi
  if ! grep -q '^COORD_ADMIN_USERS=' "$env_path"; then
    printf 'COORD_ADMIN_USERS=\n' >>"$env_path"
    say "added COORD_ADMIN_USERS to $env_path"
  fi
  if grep -q '^COORD_ADMIN_TOKEN=$' "$env_path"; then
    sed -i "s/^COORD_ADMIN_TOKEN=$/COORD_ADMIN_TOKEN=$coord_admin_token/" "$env_path"
    say "repaired empty COORD_ADMIN_TOKEN in $env_path"
  elif ! grep -q '^COORD_ADMIN_TOKEN=' "$env_path"; then
    printf 'COORD_ADMIN_TOKEN=%s\n' "$coord_admin_token" >>"$env_path"
    say "added COORD_ADMIN_TOKEN to $env_path"
  fi
  if grep -q '^COORD_ADMIN_PORT=$' "$env_path"; then
    sed -i "s/^COORD_ADMIN_PORT=$/COORD_ADMIN_PORT=$coord_admin_port/" "$env_path"
    say "repaired empty COORD_ADMIN_PORT=$coord_admin_port in $env_path"
  elif ! grep -q '^COORD_ADMIN_PORT=' "$env_path"; then
    printf 'COORD_ADMIN_PORT=%s\n' "$coord_admin_port" >>"$env_path"
    say "added COORD_ADMIN_PORT=$coord_admin_port to $env_path"
  fi
}

patch_tenant_env() {
  local name="$1"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  retire_tenant_nanocode "$name" \
    || die "failed to retire legacy Nanocode resources for tenant '$name'"

  local env_path
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$env_path" ]] || die "env file not found: $env_path"

  # LUNARWING_OWNER_ID: sets the daemon's DB-scoping owner_id so sessions,
  # memory, and settings are isolated per tenant. Without it the daemon
  # defaults to "default", sharing state across all tenants on the host.
  if grep -q '^LUNARWING_OWNER_ID=' "$env_path"; then
    say "LUNARWING_OWNER_ID already set in $env_path (skipping)"
  else
    printf '\n# Runtime identity (DB scope — must match tenant name)\nLUNARWING_OWNER_ID=%s\n' "$name" >>"$env_path"
    say "added LUNARWING_OWNER_ID=$name to $env_path"
    # Auto-migrate existing DB data from 'default' scope to the tenant's scope
    # so the daemon doesn't lose access to conversations, memory, and settings.
    if _owner_scope_needs_migration "$name"; then
      say "  found 'default'-scoped DB data; migrating to '$name' scope"
      migrate_owner_scope "$name" || say "  WARNING: owner-scope migration failed (run 'migrate-owner-scope $name' manually)"
    fi
  fi

  local orchestrator_port
  orchestrator_port="$(ports_get "$name" orchestrator)"

  if grep -q '^ORCHESTRATOR_PORT=' "$env_path"; then
    say "ORCHESTRATOR_PORT already set in $env_path (skipping)"
  else
    printf '\n# Orchestrator (sandbox container callback)\nORCHESTRATOR_PORT=%s\n' "$orchestrator_port" >>"$env_path"
    say "added ORCHESTRATOR_PORT=$orchestrator_port to $env_path"
  fi

  local pebble_wss_port
  pebble_wss_port="$(ports_get "$name" pebble_wss)"
  if [[ -n "$pebble_wss_port" ]]; then
    if grep -q '^PEBBLE_WSS_PORT=' "$env_path"; then
      say "PEBBLE_WSS_PORT already set in $env_path (skipping)"
    else
      printf '\n# Pebble worker (WebSocket port for agent communication)\nPEBBLE_WSS_PORT=%s\n' "$pebble_wss_port" >>"$env_path"
      say "added PEBBLE_WSS_PORT=$pebble_wss_port to $env_path"
    fi
  fi

  local opencode_wss_port
  opencode_wss_port="$(ports_get "$name" opencode_wss)" || true
  if [[ -n "$opencode_wss_port" ]]; then
    if grep -q '^OPENCODE_WSS_PORT=' "$env_path"; then
      say "OPENCODE_WSS_PORT already set in $env_path (skipping)"
    else
      printf '\n# Opencode worker (WebSocket port for agent communication)\nOPENCODE_WSS_PORT=%s\n' "$opencode_wss_port" >>"$env_path"
      say "added OPENCODE_WSS_PORT=$opencode_wss_port to $env_path"
    fi
  fi

  local kimi_wss_port
  kimi_wss_port="$(ports_get "$name" kimi_wss)" || true
  if [[ -n "$kimi_wss_port" ]]; then
    if grep -q '^KIMI_WSS_PORT=' "$env_path"; then
      say "KIMI_WSS_PORT already set in $env_path (skipping)"
    else
      printf '\n# Kimi worker (WebSocket port for agent communication)\nKIMI_WSS_PORT=%s\n' "$kimi_wss_port" >>"$env_path"
      say "added KIMI_WSS_PORT=$kimi_wss_port to $env_path"
    fi
  fi

  local weechat_adapter_port
  weechat_adapter_port="$(ports_get "$name" weechat_adapter)"
  if [[ -n "$weechat_adapter_port" ]]; then
    if grep -q '^WEECHAT_ADAPTER_PORT=' "$env_path"; then
      say "WEECHAT_ADAPTER_PORT already set in $env_path (skipping)"
    else
      printf '\n# WeeChat adapter (local HTTP adapter bridging WeeChat WS relay to WASM)\nWEECHAT_ADAPTER_PORT=%s\n' "$weechat_adapter_port" >>"$env_path"
      say "added WEECHAT_ADAPTER_PORT=$weechat_adapter_port to $env_path"
    fi
    # WS_ADAPTER_URL is the full adapter URL consumed by the in-process WASM
    # channel (via the capabilities `env` source). Without it the channel
    # falls back to the hardcoded :6681 default and silently fails.
    if grep -q '^WS_ADAPTER_URL=' "$env_path"; then
      say "WS_ADAPTER_URL already set in $env_path (skipping)"
    else
      printf 'WS_ADAPTER_URL=http://127.0.0.1:%s\n' "$weechat_adapter_port" >>"$env_path"
      say "added WS_ADAPTER_URL=http://127.0.0.1:$weechat_adapter_port to $env_path"
    fi
  fi

  local weechat_port
  weechat_port="$(ports_get "$name" weechat)"
  if [[ -n "$weechat_port" ]]; then
    if grep -q '^RELAY_URL=' "$env_path"; then
      say "RELAY_URL already set in $env_path (skipping)"
    else
      printf '\n# WeeChat relay URL consumed by the in-process WASM channel\nRELAY_URL=http://127.0.0.1:%s\n' "$weechat_port" >>"$env_path"
      say "added RELAY_URL=http://127.0.0.1:$weechat_port to $env_path"
    fi
  fi

  patch_tenant_coordination_env "$name" "$env_path"

  if tenant_darkirc_enabled "$name"; then
    local darkirc_adapter_port
    darkirc_adapter_port="$(ports_get "$name" darkirc_adapter)" || true
    if [[ -n "$darkirc_adapter_port" ]]; then
      if grep -q '^DARKIRC_ADAPTER_URL=' "$env_path"; then
        say "DARKIRC_ADAPTER_URL already set in $env_path (skipping)"
      else
        printf '\nDARKIRC_ADAPTER_URL=http://127.0.0.1:%s\n' "$darkirc_adapter_port" >>"$env_path"
        say "added DARKIRC_ADAPTER_URL=http://127.0.0.1:$darkirc_adapter_port to $env_path"
      fi
      if grep -q '^DARKIRC_ADAPTER_SECRET=' "$env_path"; then
        say "DARKIRC_ADAPTER_SECRET already set in $env_path (skipping)"
      else
        local darkirc_adapter_secret
        darkirc_adapter_secret="$(generate_token | cut -c1-32)"
        printf 'DARKIRC_ADAPTER_SECRET=%s\n' "$darkirc_adapter_secret" >>"$env_path"
        say "added DARKIRC_ADAPTER_SECRET to $env_path"
      fi
      write_tenant_darkirc_adapter_env "$name"
      if ports_get "$name" darkirc_irc >/dev/null 2>&1; then
        generate_darkirc_config "$name"
      fi
    fi
  fi

  # Wire supported external workers into config.toml so
  # existing tenants get create_job(mode: ...) routing without a hand-edited
  # config file.
  ensure_external_worker_config "$name" "pebble" "pebble_wss"
  ensure_external_worker_config "$name" "opencode" "opencode_wss"
  ensure_external_worker_config "$name" "kimi" "kimi_wss"
}

# ── Owner-scope DB migration ──────────────────────────────────────────────────
#
# Rekeys all user_id='default' (or a specified old scope) rows to user_id=<name>
# in the tenant's PostgreSQL. Used when a tenant that was originally created
# without LUNARWING_OWNER_ID (running as 'default') is upgraded to have its own
# owner_id scope. Handles unique-constraint collisions by preserving the
# existing tenant-scoped row and copying content from the old row if the new
# one is empty.
#
extract_host_from_url() {
  printf '%s' "$1" | sed -E 's|^https?://||; s|[:/].*||'
}

write_tenant_gotify_config() {
  local name="$1"
  local gotify_url="${2:-}"
  local gotify_title="${3:-}"

  [[ -n "$gotify_url" ]] || return 0

  local state_dir config_dir config_path
  state_dir="$(tenant_state_dir "$name")"
  config_dir="$state_dir/workspace/config"
  config_path="$config_dir/gotify.json"

  sudo -u "$name" mkdir -p "$config_dir"
  gotify_url="$(printf '%s' "$gotify_url" | sed 's|/$||')"
  if [[ -n "$gotify_title" ]]; then
    printf '{"url": "%s", "title": "%s"}\n' "$gotify_url" "$gotify_title" >"$config_path"
  else
    printf '{"url": "%s"}\n' "$gotify_url" >"$config_path"
  fi
  chown "$name:$name" "$config_path"
  say "wrote: $config_path"
}

configure_gotify_capabilities() {
  local name="$1"
  local gotify_url="${2:-}"

  [[ -n "$gotify_url" ]] || return 0
  require_cmd jq

  local tools_dir caps_path host
  tools_dir="$(tenant_state_dir "$name")/tools"
  caps_path="$tools_dir/gotify-tool.capabilities.json"

  [[ -f "$caps_path" ]] || return 0

  host="$(extract_host_from_url "$gotify_url")"
  [[ -n "$host" ]] || return 0

  local tmp
  tmp="$(mktemp "$caps_path.tmp.XXXXXX")"
  jq --arg host "$host" '
    .capabilities.http.allowlist[0].host = $host |
    .capabilities.http.credentials.gotify.host_patterns = [$host]
  ' "$caps_path" >"$tmp"
  mv "$tmp" "$caps_path"
  chown "$name:$name" "$caps_path"
  say "  configured gotify capabilities for host: $host"
}

# ── Pebble worker configuration ──────────────────────────────────────────────

# ── Pebble worker container ──────────────────────────────────────────────────

# ── LunarVision OCR/vision sidecar ──────────────────────────────────────────

VISION_SIDECAR_IMAGE=lunarwing/vision-service:latest
VISION_SIDECAR_INTERNAL_PORT=8088
VISION_SIDECAR_HEALTH_PORT=8089

write_tenant_voice_env() {
  local name="$1"
  local env_dir env_path token enabled stt_model stt_language enable_vad enable_tts
  env_dir="$(tenant_env_dir "$name")"
  env_path="$env_dir/voice.env"
  token="$(_env_existing "$env_path" VOICE_AUTH_TOKEN)"
  token="${token:-$(generate_token)}"
  enabled="$(jq -r --arg name "$name" '.tenants[$name].enable_voice // false' "$PORTS_REGISTRY" 2>/dev/null)"
  [[ "$enabled" == true ]] || enabled=false
  stt_model="$(_env_existing "$env_path" VOICE_STT_MODEL)"; stt_model="${stt_model:-whisper-tiny}"
  stt_language="$(_env_existing "$env_path" VOICE_STT_LANGUAGE)"; stt_language="${stt_language:-auto}"
  enable_vad="$(_env_existing "$env_path" VOICE_ENABLE_VAD)"; enable_vad="${enable_vad:-true}"
  enable_tts="$(_env_existing "$env_path" VOICE_ENABLE_TTS)"; enable_tts="${enable_tts:-true}"
  mkdir -p "$env_dir"
  (
    umask 077
    cat >"$env_path" <<ENVEOF
VOICE_ENABLED=$enabled
VOICE_AUTH_TOKEN=$token
VOICE_BIND_HOST=0.0.0.0
VOICE_PORT=8090
VOICE_HEALTH_PORT=8091
VOICE_MODEL_DIR=/models
VOICE_STT_MODEL=$stt_model
VOICE_STT_LANGUAGE=$stt_language
VOICE_ENABLE_VAD=$enable_vad
VOICE_ENABLE_TTS=$enable_tts
VOICE_REQUIRE_STT=true
VOICE_MAX_AUDIO_SECS=300
VOICE_MAX_AUDIO_BYTES=52428800
VOICE_MAX_TTS_AUDIO_SECS=300
VOICE_REQUEST_TIMEOUT_SECS=120
VOICE_MAX_CONCURRENT_INFERENCE=2
VOICE_RATE_LIMIT_PER_SEC=5
VOICE_CACHE_TTL_SECS=600
VOICE_CACHE_MAX_ENTRIES=5000
ORT_NUM_THREADS=4
ENVEOF
  )
  chown "$name:$name" "$env_path"
  printf '%s' "$token"
}

tenant_voice_enabled() {
  [[ "$(jq -r --arg name "$1" '.tenants[$name].enable_voice // false' "$PORTS_REGISTRY" 2>/dev/null)" == "true" ]]
}

voice_models_ready() {
  local name="$1" env_path key value default_path host_path stt_model marker_hash
  env_path="$(tenant_env_dir "$name")/voice.env"
  stt_model="$(_env_existing "$env_path" VOICE_STT_MODEL)"
  stt_model="${stt_model:-whisper-tiny}"
  [[ "$stt_model" =~ ^[A-Za-z0-9._-]+$ ]] || return 1
  [[ -f "$VOICE_MODELS_DIR/.lunarvoice-${stt_model}.sha256" ]] || return 1
  marker_hash="$(tr -d '\r\n' <"$VOICE_MODELS_DIR/.lunarvoice-${stt_model}.sha256")"
  [[ "$marker_hash" =~ ^[0-9a-f]{64}$ ]] || return 1
  while IFS='|' read -r key default_path; do
    value="$(_env_existing "$env_path" "$key")"
    value="${value:-$default_path}"
    [[ "$value" == /models/* ]] || return 1
    host_path="$VOICE_MODELS_DIR/${value#/models/}"
    [[ -f "$host_path" && -r "$host_path" ]] || return 1
  done <<'MODEL_PATHS'
VOICE_STT_ENCODER|/models/sherpa-onnx-whisper-tiny/tiny-encoder.int8.onnx
VOICE_STT_DECODER|/models/sherpa-onnx-whisper-tiny/tiny-decoder.int8.onnx
VOICE_STT_TOKENS|/models/sherpa-onnx-whisper-tiny/tiny-tokens.txt
MODEL_PATHS
}



# Render the WeeChat OpenRC init script into <out_file>.

# ── OpenRC service units ─────────────────────────────────────────────────────





cleanup_legacy_weechat_systemd() {
  local name="$1" unit unit_dir unit_path link load_state known=false
  local changed=false
  unit="weechat-${name}.service"
  unit_dir="$(tenant_home "$name")/.config/systemd/user"
  unit_path="$unit_dir/$unit"

  load_state="$(_systemctl_user "$name" show --value -p LoadState "$unit" 2>/dev/null)" \
    || return 1
  [[ "$load_state" == not-found ]] || known=true
  if [[ -e "$unit_path" || -L "$unit_path" ]]; then
    [[ -f "$unit_path" && ! -L "$unit_path" ]] \
      || die "refusing to operate on non-regular legacy unit: $unit_path"
    known=true
  fi
  if [[ "$known" == true ]]; then
    _systemctl_user "$name" disable --now "$unit" || return 1
    changed=true
  fi
  if [[ -e "$unit_path" || -L "$unit_path" ]]; then
    sudo -u "$name" -- rm -- "$unit_path" || return 1
  fi

  for link in "$unit_dir"/*.wants/"$unit" "$unit_dir"/*.requires/"$unit"; do
    [[ -e "$link" || -L "$link" ]] || continue
    [[ -L "$link" ]] || die "refusing to remove non-symlink legacy enablement: $link"
    sudo -u "$name" -- rm -- "$link" || return 1
    changed=true
  done

  if [[ "$changed" == true ]]; then
    _systemctl_user "$name" daemon-reload || return 1
    say "removed legacy systemd user unit $unit for $name"
  else
    say "legacy systemd user unit $unit absent for $name"
  fi
}


cleanup_legacy_weechat() {
  local name="$1"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  ensure_init_system
  case "$INIT_SYSTEM" in
    systemd) cleanup_legacy_weechat_systemd "$name" ;;
    openrc) cleanup_legacy_weechat_openrc "$name" ;;
    *) die "unsupported init system: $INIT_SYSTEM" ;;
  esac
}

restart_weechat_adapter() {
  local name="$1"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  ensure_init_system
  case "$INIT_SYSTEM" in
    systemd)
      _systemctl_user "$name" restart "lunarwing-weechat-adapter-${name}.service"
      ;;
    openrc)
      rc-service "lunarwing-weechat-adapter-${name}" restart
      ;;
    *) die "unsupported init system: $INIT_SYSTEM" ;;
  esac
}

darkirc_service_uses_release_selector() {
  local name="$1" service_path expected command_prefix fragment drop_ins
  ensure_init_system
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    service_path="$(tenant_home "$name")/.config/systemd/user/lunarwing-darkirc-${name}.service"
    expected="ExecStart=$DARKIRC_RELEASE_DAEMON --config $(tenant_state_dir "$name")/darkirc/darkirc_config.toml"
    command_prefix='ExecStart='
  else
    service_path="$OPENRC_INITD_DIR/lunarwing-darkirc-${name}"
    expected="darkirc_command=\"$DARKIRC_RELEASE_DAEMON\""
    command_prefix='darkirc_command='
  fi
  [[ -f "$service_path" && ! -L "$service_path" ]] || return 1
  [[ "$(grep -Fxc -- "$expected" "$service_path")" -eq 1 \
    && "$(grep -Fc -- "$command_prefix" "$service_path")" -eq 1 ]] || return 1
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    _systemctl_user "$name" daemon-reload >/dev/null || return 1
    fragment="$(_systemctl_user "$name" show --value -p FragmentPath \
      "lunarwing-darkirc-${name}.service" 2>/dev/null)" || return 1
    drop_ins="$(_systemctl_user "$name" show --value -p DropInPaths \
      "lunarwing-darkirc-${name}.service" 2>/dev/null)" || return 1
    [[ "$fragment" == "$service_path" && -z "$drop_ins" ]]
  fi
}

# ISSUE-006: enable_weechat is authoritative for lifecycle, not just health
# classification. disable-weechat stops the optional weechat units and removes
# their boot persistence (ending any systemd restart loop); enable-weechat
# re-enables and starts them. Both are idempotent and tolerate never-rendered
# or missing units on either init system.
weechat_lifecycle_stop_disable() {
  local name="$1"
  case "$INIT_SYSTEM" in
    systemd)
      if ! id -u "$name" >/dev/null 2>&1; then
        say "tenant user '$name' absent; cannot stop/disable WeeChat" >&2
        return 1
      fi
      local user_unit_dir
      user_unit_dir="$(tenant_home "$name")/.config/systemd/user"
      _systemctl_user "$name" show-environment >/dev/null || return 1
      local units=() unit load_state
      for unit in "lunarwing-weechat-adapter-${name}.service" "lunarwing-weechat-${name}.service"; do
        if ! load_state="$(_systemctl_user "$name" show --value -p LoadState "$unit" 2>/dev/null)"; then
          return 1
        fi
        if [[ -f "$user_unit_dir/$unit" || ( -n "$load_state" && "$load_state" != "not-found" ) ]]; then
          units+=("$unit")
        fi
      done
      if ((${#units[@]} > 0)); then
        _systemctl_user "$name" stop "${units[@]}" || return 1
        _systemctl_user "$name" disable "${units[@]}" || return 1
      fi
      say "stopped + disabled lunarwing-weechat{,-adapter}-${name}.service (systemd)"
      ;;
    openrc)
      local svc link runlevel
      for svc in "lunarwing-weechat-adapter-${name}" "lunarwing-weechat-${name}"; do
        if [[ -x "$OPENRC_INITD_DIR/$svc" ]]; then
          rc-service "$svc" stop || return 1
        fi
        for link in "$OPENRC_RUNLEVELS_DIR"/*/"$svc"; do
          [[ -e "$link" || -L "$link" ]] || continue
          runlevel="$(basename "$(dirname "$link")")"
          rc-update del "$svc" "$runlevel" || return 1
        done
      done
      say "stopped + removed from runlevels: lunarwing-weechat{,-adapter}-${name} (OpenRC)"
      ;;
    *)
      say "weechat lifecycle: unsupported INIT_SYSTEM=$INIT_SYSTEM" >&2
      return 1
      ;;
  esac
}

weechat_lifecycle_enable_start() {
  local name="$1"
  case "$INIT_SYSTEM" in
    systemd)
      if ! id -u "$name" >/dev/null 2>&1; then
        say "tenant user '$name' absent; cannot enable/start WeeChat" >&2
        return 1
      fi
      local user_unit_dir
      user_unit_dir="$(tenant_home "$name")/.config/systemd/user"
      if [[ ! -f "$user_unit_dir/lunarwing-weechat-${name}.service" \
        || ! -f "$user_unit_dir/lunarwing-weechat-adapter-${name}.service" ]]; then
        say "weechat units not rendered for '$name'" >&2
        return 1
      fi
      _systemctl_user "$name" daemon-reload || return 1
      _systemctl_user "$name" enable "lunarwing-weechat-${name}.service" "lunarwing-weechat-adapter-${name}.service" || return 1
      _systemctl_user "$name" start "lunarwing-weechat-${name}.service" || return 1
      _systemctl_user "$name" start "lunarwing-weechat-adapter-${name}.service" || return 1
      say "enabled + started lunarwing-weechat{,-adapter}-${name}.service (systemd)"
      ;;
    openrc)
      if [[ ! -x "$OPENRC_INITD_DIR/lunarwing-weechat-${name}" \
        || ! -x "$OPENRC_INITD_DIR/lunarwing-weechat-adapter-${name}" ]]; then
        say "weechat units not rendered for '$name'" >&2
        return 1
      fi
      rc-update add "lunarwing-weechat-${name}" default || return 1
      rc-update add "lunarwing-weechat-adapter-${name}" default || return 1
      rc-service "lunarwing-weechat-${name}" start || return 1
      rc-service "lunarwing-weechat-adapter-${name}" start || return 1
      say "enabled + started lunarwing-weechat{,-adapter}-${name} (OpenRC)"
      ;;
    *)
      say "weechat lifecycle: unsupported INIT_SYSTEM=$INIT_SYSTEM" >&2
      return 1
      ;;
  esac
}

weechat_reconcile_units() {
  local name="$1"
  case "$INIT_SYSTEM" in
    systemd)
      (render_tenant_systemd_units "$name") || return 1
      _systemctl_user "$name" daemon-reload || return 1
      ;;
    openrc)
      (render_tenant_openrc_units "$name") || return 1
      ;;
    *)
      say "weechat lifecycle: unsupported INIT_SYSTEM=$INIT_SYSTEM" >&2
      return 1
      ;;
  esac
}

set_tenant_weechat_enabled() {
  local name="$1" enabled="$2"
  with_tenant_lifecycle_lock "$name" _set_tenant_weechat_enabled_locked "$name" "$enabled"
}

_set_tenant_weechat_enabled_locked() {
  local name="$1" enabled="$2"
  case "$enabled" in true|false) ;; *) return 2 ;; esac

  ensure_init_system || return 1
  if [[ "$enabled" == "false" ]]; then
    # Stop first: health must not mask a unit until init confirms that its
    # process and boot persistence are gone.
    weechat_lifecycle_stop_disable "$name" || return 1
    ports_set_weechat "$name" false || return 1
    if ! weechat_reconcile_units "$name"; then
      # Reconciliation failed after the flag changed. Restore fail-loud health
      # semantics and make a best effort to restore the prior unit graph.
      say "disabled WeeChat unit reconciliation failed; restoring enabled flag" >&2
      ports_set_weechat "$name" true \
        || say "WARNING: failed to restore enable_weechat=true for '$name'" >&2
      weechat_reconcile_units "$name" \
        || say "WARNING: failed to restore enabled WeeChat units for '$name'" >&2
      return 1
    fi
    return 0
  fi

  # Enabled is fail-loud: health remains critical if rendering or startup fails.
  ports_set_weechat "$name" true || return 1
  weechat_reconcile_units "$name" || return 1
  weechat_lifecycle_enable_start "$name" || return 1
}

# Stop only services that can mutate a migration snapshot. PostgreSQL remains
# running so export can take a consistent pg_dump after quiescence.
tenant_writer_services() {
  local name="$1"
  printf '%s\n' "lunarwing-${name}" "xmpp-bridge-${name}"
  tenant_proxy_enabled "$name" && printf '%s\n' "lunarwing-proxy-${name}"
  if tenant_weechat_enabled "$name"; then
    printf '%s\n' "lunarwing-weechat-adapter-${name}" "lunarwing-weechat-${name}"
  fi
  if tenant_darkirc_enabled "$name"; then
    printf '%s\n' "lunarwing-darkirc-adapter-${name}" "lunarwing-darkirc-${name}"
  fi
  printf '%s\n' "lunarwing-vision-${name}"
  local worker
  for worker in pebble opencode kimi; do
    if tenant_worker_enabled "$name" "$worker"; then
      printf '%s\n' "lunarwing-${worker}-${name}"
      [[ "$INIT_SYSTEM" != openrc ]] || printf '%s\n' "lunarwing-${worker}-${name}-sup"
    fi
  done
}

tenant_writer_containers() {
  local name="$1" worker
  for worker in pebble opencode kimi vision; do
    printf 'lunarwing-%s-%s\n' "$worker" "$name"
  done
}

container_name_list_has() {
  local names="$1" expected="$2" current
  while IFS= read -r current; do
    [[ "$current" == "$expected" ]] && return 0
  done <<<"$names"
  return 1
}

stop_tenant_writers() {
  local name="$1" service state rc worker container names running writer_rc
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    while IFS= read -r service; do
      rc=0
      state="$(_systemctl_user "$name" is-active "${service}.service" 2>/dev/null)" || rc=$?
      case "$state" in
        active|activating|reloading|deactivating)
          _systemctl_user "$name" stop "${service}.service" >/dev/null \
            || die "failed to stop writer ${service}.service" ;;
        inactive|dead|failed|maintenance) ;;
        unknown) [[ "$rc" -eq 4 ]] || die "could not inspect writer ${service}.service" ;;
        *) [[ "$rc" -eq 4 ]] || die "could not classify writer ${service}.service state '$state'" ;;
      esac
    done < <(tenant_writer_services "$name")
  else
    while IFS= read -r service; do
      [[ -x "$OPENRC_INITD_DIR/$service" ]] || continue
      rc-service "$service" stop >/dev/null 2>&1 \
        || rc-service "$service" status 2>&1 | grep -Eqi 'stopped|inactive|dead' \
        || die "failed to stop writer $service"
    done < <(tenant_writer_services "$name")
  fi

  for worker in pebble opencode kimi; do
    stop_worker "$name" "$worker"
  done
  stop_tenant_vision "$name"

  ensure_container_runtime
  names="$(_ctr "$name" ps -a --format '{{.Names}}' 2>/dev/null)" \
    || die "could not inspect tenant container store"
  while IFS= read -r container; do
    container_name_list_has "$names" "$container" || continue
    running="$(_ctr "$name" inspect -f '{{.State.Running}}' "$container" 2>/dev/null)" \
      || die "could not inspect writer container $container"
    case "$running" in
      true) _ctr "$name" stop --time 0 "$container" >/dev/null \
        || die "failed to stop writer container $container" ;;
      false) ;;
      *) die "could not classify writer container $container" ;;
    esac
  done < <(tenant_writer_containers "$name")

  writer_rc=0
  tenant_writers_active "$name" || writer_rc=$?
  case "$writer_rc" in
    0) die "tenant '$name' still has active writers after stop" ;;
    1) ;;
    *) die "could not verify writer quiescence for tenant '$name'" ;;
  esac
  say "tenant writers stopped for $name (PostgreSQL left running)"
}

# Returns 0 when any writer is active, 1 when all are stopped/absent, and 2
# when state cannot be classified safely.
tenant_writers_active() {
  local name="$1" service state rc container running names
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || return 2
  ensure_init_system || return 2
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    while IFS= read -r service; do
      rc=0
      state="$(_systemctl_user "$name" is-active "${service}.service" 2>/dev/null)" || rc=$?
      case "$state" in
        active|activating|reloading|deactivating) return 0 ;;
        inactive|dead|failed|maintenance) ;;
        unknown) [[ "$rc" -eq 4 ]] || return 2 ;;
        *) [[ "$rc" -eq 4 ]] || return 2 ;;
      esac
    done < <(tenant_writer_services "$name")
  else
    while IFS= read -r service; do
      [[ -x "$OPENRC_INITD_DIR/$service" ]] || continue
      rc=0
      state="$(rc-service "$service" status 2>&1)" || rc=$?
      case "$state" in
        *stopped*|*inactive*|*dead*) ;;
        *started*|*running*|*active*) return 0 ;;
        *) [[ "$rc" -eq 3 ]] || return 2 ;;
      esac
    done < <(tenant_writer_services "$name")
  fi

  ensure_container_runtime || return 2
  names="$(_ctr "$name" ps -a --format '{{.Names}}' 2>/dev/null)" || return 2
  while IFS= read -r container; do
    container_name_list_has "$names" "$container" || continue
    running="$(_ctr "$name" inspect -f '{{.State.Running}}' "$container" 2>/dev/null)" \
      || return 2
    [[ "$running" != true ]] || return 0
    [[ "$running" == false ]] || return 2
  done < <(tenant_writer_containers "$name")
  return 1
}

tenant_writers_probe_status() {
  local rc=0
  tenant_writers_active "$1" || rc=$?
  case "$rc" in
    0) return 0 ;;
    1) return 10 ;;
    *) return 2 ;;
  esac
}

# Diagnostic-only in Stage A. Both init-managed services and authenticated
# adapter connectivity must be healthy; this does not activate an exchange.
darkirc_health_strict() {
  local name="$1" attempts="${2:-10}" quiet="${3:-false}"
  local adapter_port env_path secret response attempt
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || return 1
  tenant_darkirc_enabled "$name" || return 1
  ensure_init_system

  if [[ "$INIT_SYSTEM" == systemd ]]; then
    _systemctl_user "$name" is-active --quiet "lunarwing-darkirc-${name}.service" \
      || return 1
    _systemctl_user "$name" is-active --quiet "lunarwing-darkirc-adapter-${name}.service" \
      || return 1
  else
    rc-service "lunarwing-darkirc-${name}" status >/dev/null 2>&1 || return 1
    rc-service "lunarwing-darkirc-adapter-${name}" status >/dev/null 2>&1 || return 1
  fi

  adapter_port="$(ports_get "$name" darkirc_adapter 2>/dev/null || true)"
  [[ "$adapter_port" =~ ^[0-9]+$ ]] || return 1
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  secret="$(read_tenant_env_value \
    "$name" "$env_path" DARKIRC_ADAPTER_SECRET 2>/dev/null)" || return 1
  [[ -n "$secret" ]] || return 1
  require_cmd curl
  require_cmd jq

  for ((attempt = 1; attempt <= attempts; attempt++)); do
    response="$(printf 'Authorization: Bearer %s\n' "$secret" | \
      curl -fsS --max-time 2 --header @- \
        "http://127.0.0.1:${adapter_port}/health" 2>/dev/null || true)"
    if jq -e '(.status == "ok") and (.irc_connected == true)' <<<"$response" \
      >/dev/null 2>&1; then
      [[ "$quiet" == true ]] || say "strict DarkIRC health passed for $name"
      return 0
    fi
    [[ "$attempt" -lt "$attempts" ]] && sleep 1
  done
  say "strict DarkIRC health failed for $name" >&2
  return 1
}


# ── Compound commands ────────────────────────────────────────────────────────

# WeeChat's ws_adapter.py needs the `aiohttp` Python package importable by the
# TENANT user's python3 (system-wide, or in that user's ~/.local — an --user
# install for a different account, e.g. the admin, is NOT visible). Non-fatal:
# the adapter is optional, so we only warn with how to fix it.
add_tenants() {
  local names_csv="$1"
  shift

  local IFS=','
  local names_array
  read -ra names_array <<< "$names_csv"

  for raw_name in "${names_array[@]}"; do
    local name
    name="$(sanitize_name "$(echo "$raw_name" | xargs)")"
    [[ -n "$name" ]] || continue
    say ""
    add_tenant "$name" "$@"
  done
}

# Re-render a tenant's service units from the current generator WITHOUT touching
# secrets/env or restarting anything — for applying a generator change (e.g. an
# updated init-script status()) to an already-provisioned tenant. The systemd
# renderer daemon-reloads internally; OpenRC reads the script per invocation. A
# status()/health change takes effect immediately; a change to the run command
# needs a restart.
render_tenant_units() {
  local name="$1"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  with_tenant_lifecycle_lock "$name" _render_tenant_units_locked "$name" \
    || die "failed to re-render units for tenant '$name'"
}

_render_tenant_units_locked() {
  local name="$1"
  _retire_tenant_nanocode "$name" || return 1
  ensure_init_system
  say "--- Re-rendering $INIT_SYSTEM units for $name ---"
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    if tenant_voice_enabled "$name"; then
      render_voice_quadlet "$name"
    else
      rm -f "$(tenant_quadlet_dir "$name")/lunarwing-voice-${name}.container"
    fi
    render_tenant_systemd_units "$name"
  else
    if tenant_voice_enabled "$name"; then
      render_voice_openrc_unit "$name"
    else
      rc-update del "lunarwing-voice-${name}" default >/dev/null 2>&1 || true
      rm -f "$OPENRC_INITD_DIR/lunarwing-voice-${name}"
    fi
    render_tenant_openrc_units "$name"
  fi
  say "units re-rendered for '$name' (services NOT restarted)."
  say "run-command changes need a restart to apply: $0 restart-tenant $name"
}

# Print a post-start SSH readiness block sourced from the live API. Warn-only:
# a missing/failed API must never fail start-tenant.
_ssh_ready_summary() {
  local name="$1" http_port status keys hosts
  hosts="$(_ssh_hosts_from_config "$name" | paste -sd, -)"
  [[ -n "$hosts" ]] || return 0  # SSH not configured for this tenant

  http_port="$(ports_get "$name" http)"
  status="$(curl -sf --max-time 3 "http://127.0.0.1:${http_port}/agent/status" 2>/dev/null)" || status=""
  keys="$(jq -r '.data.keys_loaded // "?"' <<<"$status" 2>/dev/null)" || keys="?"

  say ""
  say "--- SSH readiness ---"
  say "  hosts:        $hosts"
  if [[ "$keys" =~ ^[0-9]+$ ]] && ((keys >= 1)); then
    say "  agent:        running, $keys key(s) loaded — ssh/ssh_git tools ready"
  else
    say "  agent:        keys_loaded=$keys — if a key upload just failed, re-run '$0 start-tenant $name'"
  fi
  if [[ -f "$(tenant_state_dir "$name")/tools/ssh-tool.wasm" ]]; then
    say "  wasm ssh:     installed (activate it in the web panel: Settings → Extensions → ssh)"
  fi
  say "  verify:       curl -s http://127.0.0.1:${http_port}/agent/status | jq"
}

# Poll the authenticated tenant gateway until reachable (up to ~30s).
_wait_tenant_gateway() {
  local name="$1" env_path gateway_port gateway_token auth_header i=0
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  gateway_port="$(ports_get "$name" gateway)"
  gateway_token="$(_read_env_value "$env_path" GATEWAY_AUTH_TOKEN)"
  [[ -n "$gateway_token" ]] || return 1
  auth_header="Authorization: Bearer ${gateway_token}"
  while ! curl -sf --max-time 2 \
    -K <(printf 'header = "%s"\n' "$auth_header") \
    "http://127.0.0.1:${gateway_port}/api/gateway/status" >/dev/null 2>&1; do
    i=$((i + 1))
    [[ $i -lt 15 ]] || return 1
    sleep 2
  done
  return 0
}

_start_tenant_init_services() {
  local name="$1"
  ensure_init_system || return 1
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    start_tenant_systemd "$name"
  else
    start_tenant_openrc "$name"
  fi
}

# Restart ONLY the lunarwing daemon unit for a tenant (not the full stack).
# Used by start_tenant to make a freshly-uploaded SSH key signable: the agent
# loads keys from the secrets store at startup only ("runtime key add is
# status-only" — see docs/architecture/SSH_AGENT_HARNESS.md §7).
stop_tenant_for_upgrade() {
  local name="$1" scope_id result=0 writer_lock_owned=false
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"

  if tenant_darkirc_enabled "$name"; then
    if [[ "$DARKIRC_WRITER_LOCK_HELD" != true ]]; then
      darkirc_writer_lock "$name"
      writer_lock_owned=true
    fi
    scope_id="$(darkirc_scope_id "$name")"
    validate_darkirc_scope_id "$scope_id"
    run_darkirc_key_helper "$name" migration-ready \
      --tenant "$name" --scope-id "$scope_id" --json >/dev/null \
      || die "DarkIRC migration state is not settled; refusing upgrade"
  fi

  stop_tenant "$name" || result=$?
  if [[ "$writer_lock_owned" == true ]]; then
    darkirc_writer_unlock
  fi
  return "$result"
}

# Machine export runs as a child so its existing passphrase and archive handling
# stay in export-tenant.sh. Both inherited lock descriptors remain open until that
# child has validated readiness, stopped/verified writers, pinned DarkIRC state,
# and sealed the final bundle.
migration_export_child() {
  local name="$1"
  shift
  local -a inherited_env=(
    "LUNARWING_MT_EXPORT_BOUNDARY_TENANT=$name"
    "LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_FD=$LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD"
    "LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_TENANT=$name"
  )
  if [[ "$DARKIRC_WRITER_LOCK_HELD" == true ]]; then
    inherited_env+=(
      "LUNARWING_MT_INHERITED_DARKIRC_LOCK_FD=$DARKIRC_WRITER_LOCK_FD"
      "LUNARWING_MT_INHERITED_DARKIRC_LOCK_SCOPE=$DARKIRC_WRITER_SCOPE"
    )
  fi
  env "${inherited_env[@]}" "$SCRIPT_DIR/export-tenant.sh" "$name" "$@"
}

migration_export_darkirc_locked() {
  local name="$1" scope_id
  shift
  scope_id="$(darkirc_scope_id "$name")"
  validate_darkirc_scope_id "$scope_id"
  run_darkirc_key_helper "$name" migration-ready \
    --tenant "$name" --scope-id "$scope_id" --json >/dev/null \
    || die "DarkIRC migration state is not settled; refusing export"
  migration_export_child "$name" "$@"
}

migration_export_boundary_locked() {
  local name="$1"
  shift
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  if tenant_darkirc_enabled "$name"; then
    with_darkirc_writer_lock "$name" migration_export_darkirc_locked "$name" "$@"
  else
    migration_export_child "$name" "$@"
  fi
}

migration_export_boundary() {
  local name="$1"
  shift
  name="$(sanitize_name "$name")"
  with_tenant_lifecycle_lock "$name" migration_export_boundary_locked "$name" "$@"
}

upgrade_wrapper_child() {
  local name="$1"
  shift
  local -a inherited_env=(
    "LUNARWING_MT_UPGRADE_BOUNDARY_TENANT=$name"
    "LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_FD=$LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD"
    "LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_TENANT=$name"
    "LUNARWING_MT_INHERITED_UPGRADE_LOCK_FD=$LUNARWING_MT_ACTIVE_UPGRADE_LOCK_FD"
  )
  if [[ "$DARKIRC_WRITER_LOCK_HELD" == true ]]; then
    inherited_env+=(
      "LUNARWING_MT_INHERITED_DARKIRC_LOCK_FD=$DARKIRC_WRITER_LOCK_FD"
      "LUNARWING_MT_INHERITED_DARKIRC_LOCK_SCOPE=$DARKIRC_WRITER_SCOPE"
    )
  fi
  env "${inherited_env[@]}" "$SCRIPT_DIR/upgrade-tenant.sh" "$@"
}

upgrade_wrapper_boundary_locked() {
  local name="$1" upgrade_lock_fd result=0
  shift
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  exec {upgrade_lock_fd}>"${BUILD_LOCK}.${name}.upgrade" \
    || die "failed to open tenant upgrade lock"
  flock -x "$upgrade_lock_fd" || die "failed to acquire tenant upgrade lock"
  LUNARWING_MT_ACTIVE_UPGRADE_LOCK_FD="$upgrade_lock_fd"
  if tenant_darkirc_enabled "$name"; then
    with_darkirc_writer_lock "$name" upgrade_wrapper_child "$name" "$@" || result=$?
  else
    upgrade_wrapper_child "$name" "$@" || result=$?
  fi
  unset LUNARWING_MT_ACTIVE_UPGRADE_LOCK_FD
  flock -u "$upgrade_lock_fd" || result=1
  exec {upgrade_lock_fd}>&-
  return "$result"
}

upgrade_wrapper_boundary() {
  local name="$1"
  shift
  name="$(sanitize_name "$name")"
  with_tenant_lifecycle_lock "$name" upgrade_wrapper_boundary_locked "$name" "$@"
}

validate_upgrade_wrapper_boundary_locked() {
  local name="$1" upgrade_fd="${LUNARWING_MT_INHERITED_UPGRADE_LOCK_FD:-}"
  [[ "${LUNARWING_MT_UPGRADE_BOUNDARY_TENANT:-}" == "$name" ]] \
    || die "upgrade wrapper boundary tenant marker mismatch"
  inherited_lock_fd_is_held "$upgrade_fd" "${BUILD_LOCK}.${name}.upgrade" \
    || die "invalid inherited tenant upgrade lock"
  if tenant_darkirc_enabled "$name"; then
    with_darkirc_writer_lock "$name" true
  elif [[ -n "${LUNARWING_MT_INHERITED_DARKIRC_LOCK_FD:-}${LUNARWING_MT_INHERITED_DARKIRC_LOCK_SCOPE:-}" ]]; then
    die "unexpected inherited DarkIRC writer lock for disabled tenant '$name'"
  fi
}

validate_upgrade_wrapper_boundary() {
  local name="$1"
  name="$(sanitize_name "$name")"
  with_tenant_lifecycle_lock "$name" validate_upgrade_wrapper_boundary_locked "$name"
}

validate_migration_export_boundary_locked() {
  local name="$1"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  if tenant_darkirc_enabled "$name"; then
    with_darkirc_writer_lock "$name" true
  elif [[ -n "${LUNARWING_MT_INHERITED_DARKIRC_LOCK_FD:-}${LUNARWING_MT_INHERITED_DARKIRC_LOCK_SCOPE:-}" ]]; then
    die "unexpected inherited DarkIRC writer lock for disabled tenant '$name'"
  fi
}

validate_migration_export_boundary() {
  local name="$1"
  name="$(sanitize_name "$name")"
  [[ "${LUNARWING_MT_EXPORT_BOUNDARY_TENANT:-}" == "$name" ]] \
    || die "migration export boundary tenant marker mismatch"
  with_tenant_lifecycle_lock "$name" validate_migration_export_boundary_locked "$name"
}

# (require_worker_name moved to mt-admin-lib/09-worker-config.sh)
# (prepare_worker_lifecycle moved to mt-admin-lib/09-worker-config.sh)

# Stop both worker units and remove every boot path. Init scripts stay rendered
# so an explicit start-worker can re-enable them without reprovisioning.
prepare_tenant_checkout() {
  local name="$1" lw_root="$2" target_ref="$3"
  local tgit=(sudo -u "$name" git -c safe.directory="$lw_root" -C "$lw_root")
  local dirty dirty_locks staged_changes other_unstaged checkout_error

  if ! dirty="$("${tgit[@]}" status --short --untracked-files=no)"; then
    die "failed to inspect tracked changes in $lw_root"
  fi
  if [[ -n "$dirty" ]]; then
    if ! dirty_locks="$("${tgit[@]}" diff --name-only -- ':(glob)**/Cargo.lock')" \
      || ! staged_changes="$("${tgit[@]}" diff --cached --name-only)" \
      || ! other_unstaged="$("${tgit[@]}" diff --name-only -- . ':(exclude,glob)**/Cargo.lock')"; then
      die "failed to classify tracked changes in $lw_root"
    fi

    if [[ -n "$dirty_locks" && -z "$staged_changes" && -z "$other_unstaged" ]]; then
      say "restoring build-regenerated Cargo.lock files before checkout:"
      while IFS= read -r lockfile; do
        say "  $lockfile"
      done <<<"$dirty_locks"
      "${tgit[@]}" restore --worktree -- ':(glob)**/Cargo.lock' \
        || die "failed to restore build-regenerated Cargo.lock files"
    fi

    if ! dirty="$("${tgit[@]}" status --short --untracked-files=no)"; then
      die "failed to inspect tracked changes in $lw_root"
    fi
  fi
  if [[ -n "$dirty" ]]; then
    say "tracked changes still present in tenant checkout:"
    while IFS= read -r changed; do
      say "  $changed"
    done <<<"$dirty"
    die "checkout $lw_root has tracked changes; commit or stash them, then re-run (tenant was not stopped)"
  fi

  if ! checkout_error="$("${tgit[@]}" read-tree --dry-run -m -u HEAD "$target_ref" 2>&1)"; then
    say "target checkout preflight failed:"
    while IFS= read -r detail; do
      say "  $detail"
    done <<<"$checkout_error"
    die "checkout $target_ref would not be safe (tenant was not stopped)"
  fi
}

recover_failed_tenant_checkout() {
  local name="$1" lw_root="$2" target="$3" previous_commit="$4"
  local tgit=(sudo -u "$name" git -c safe.directory="$lw_root" -C "$lw_root")
  local current_commit dirty

  if current_commit="$("${tgit[@]}" rev-parse --verify HEAD 2>/dev/null)" \
    && dirty="$("${tgit[@]}" status --short --untracked-files=no 2>/dev/null)" \
    && [[ "$current_commit" == "$previous_commit" && -z "$dirty" ]]; then
    say "checkout failed before changing tracked tree; restarting previous tenant version ..."
    if (start_tenant "$name"); then
      die "git checkout $target failed; previous tenant version restarted"
    fi
    die "git checkout $target failed; tracked tree is unchanged but automatic tenant restart failed"
  fi

  die "git checkout $target failed after repository state changed; tenant remains stopped for inspection"
}

darkirc_managed_state_exists() {
  local state_root
  state_root="$(tenant_state_dir "$1")/darkirc/key-exchange"
  [[ -e "$state_root" || -L "$state_root" ]]
}

# In-place tenant upgrade: composes the verified sequence (backup -> fetch ->
# clean-tree gate -> stop -> checkout as the tenant user -> rebuild with WASM ->
# re-render units -> patch env -> start). The verbs it calls dispatch
# OpenRC/systemd themselves, so this works on any supported init system. See
# docs/ops/TENANT-RENAME-MIGRATION-1.1.9.md for the 1.1.9 rename specifics.
_upgrade_tenant_lifecycle_locked() {
  local name="$1" upgrade_lock_fd
  say "acquiring tenant upgrade lock ..."
  exec {upgrade_lock_fd}>"${BUILD_LOCK}.${name}.upgrade" \
    || die "failed to open tenant upgrade lock"
  flock -x "$upgrade_lock_fd" || die "failed to acquire tenant upgrade lock"
  if tenant_darkirc_enabled "$name" && darkirc_managed_state_exists "$name"; then
    with_darkirc_writer_lock "$name" upgrade_tenant_darkirc_locked "$@"
  else
    upgrade_tenant_locked "$@"
  fi
  flock -u "$upgrade_lock_fd" || die "failed to release tenant upgrade lock"
  exec {upgrade_lock_fd}>&-
}

show_tokens() {
  local filter="${1:-}"

  say "=== Gateway auth tokens ==="
  say ""

  local names
  if [[ -n "$filter" ]]; then
    names="$(sanitize_name "$filter")"
  else
    names="$(all_tenant_names)"
  fi

  if [[ -z "$names" ]]; then
    say "no tenants found"
    return 0
  fi

  while IFS= read -r name; do
    local env_path gateway_port token
    env_path="$(tenant_env_dir "$name")/lunarwing.env"
    gateway_port="$(ports_get "$name" gateway)"
    token="$(_read_env_value "$env_path" GATEWAY_AUTH_TOKEN)"
    say "$name (port $gateway_port): ${token:-<not set>}"
  done <<< "$names"
}

# ── Main dispatcher ──────────────────────────────────────────────────────────

main() {
  local command_name="${1:-}"
  if [[ -z "$command_name" ]]; then
    usage
    exit 1
  fi
  shift || true

  # An explicitly-chosen runtime persists no matter which command runs.
  # Without this, commands that never touch containers (list-tenants, tokens,
  # ...) silently ignore LUNARWING_CONTAINER_RUNTIME and nothing is saved,
  # breaking the "set it once" promise. ensure_container_runtime validates,
  # persists idempotently, and memoizes; unprivileged runs warn-and-continue.
  [[ -z "${LUNARWING_CONTAINER_RUNTIME:-}" ]] || ensure_container_runtime

  case "$command_name" in
    add-tenant)
      require_root
      local name="" docker_group="false" xmpp_jid="" xmpp_password="" tz_url="$DEFAULT_TENSORZERO_URL" gotify_url="$DEFAULT_GOTIFY_URL" gotify_title="$DEFAULT_GOTIFY_TITLE" llm_api_key="" llm_base_url="$DEFAULT_LLM_BASE_URL" enable_darkirc="false" enable_proxy="false" llm_model="" gateway_host="" xmpp_allow_from="" opencode_model="" opencode_base_url="" kimi_model="" kimi_base_url="" kimi_api_key="" with_pebble="false" with_opencode="false" with_kimi="false" darkirc_scope_override=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --docker-group)    docker_group="true"; shift ;;
          --xmpp-jid)        xmpp_jid="$2"; shift 2 ;;
          --no-health)       HEALTH_OPT_OUT=true; shift ;;
          --no-ssh)          SSH_OPT_OUT=true; shift ;;
          --no-weechat-bootstrap) WEECHAT_BOOTSTRAP_OPT_OUT=true; shift ;;
          --enable-darkirc)  enable_darkirc="true"; shift ;;
          --darkirc-scope-id) [[ $# -ge 2 ]] || die "--darkirc-scope-id requires a value"; darkirc_scope_override="$2"; shift 2 ;;
          --enable-proxy)    enable_proxy="true"; shift ;;
          --with-nanocode|--nanocode-model|--nanocode-base-url) die "Nanocode worker is retired; remove $1" ;;
          --with-pebble)     with_pebble="true"; shift ;;
          --with-opencode)   with_opencode="true"; shift ;;
          --with-kimi)       with_kimi="true"; shift ;;
          --xmpp-password)   xmpp_password="$2"; shift 2 ;;
          --llm-api-key)     llm_api_key="$2"; shift 2 ;;
          --llm-base-url)    llm_base_url="$2"; shift 2 ;;
          --tensorzero-url)  tz_url="$2"; shift 2 ;;
          --gotify-url)      gotify_url="$2"; shift 2 ;;
          --gotify-title)    gotify_title="$2"; shift 2 ;;
          --opencode-model)    opencode_model="$2"; shift 2 ;;
          --opencode-base-url) opencode_base_url="$2"; shift 2 ;;
          --kimi-model)        kimi_model="$2"; shift 2 ;;
          --kimi-base-url)     kimi_base_url="$2"; shift 2 ;;
          --kimi-api-key-file) kimi_api_key="$(_read_secret_file "$2" "Kimi API key")"; shift 2 ;;
          --kimi-api-key)      die "--kimi-api-key was removed; use --kimi-api-key-file" ;;
          --llm-model)         llm_model="$2"; shift 2 ;;
          --gateway-host)      gateway_host="$2"; shift 2 ;;
          --xmpp-allow-from)   xmpp_allow_from="$2"; shift 2 ;;
          -*)                die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: add-tenant <name> [--docker-group] [--xmpp-jid <jid>]"
      # Leave empty when --xmpp-jid is absent: add_tenant preserves the tenant's
      # existing JID and only defaults to <name>@xmpp.localhost on a first write.
      name="$(sanitize_name "$name")"
      with_tenant_lifecycle_lock "$name" add_tenant "$name" "$docker_group" "$xmpp_jid" "$xmpp_password" "$tz_url" "$gotify_url" "$gotify_title" "$llm_api_key" "$llm_base_url" "$enable_darkirc" "$enable_proxy" "$llm_model" "$gateway_host" "$xmpp_allow_from" "$opencode_model" "$opencode_base_url" "$with_pebble" "$with_opencode" "$with_kimi" "$kimi_model" "$kimi_base_url" "$kimi_api_key" "$darkirc_scope_override"
      ;;

    add-tenants)
      require_root
      local names_csv="" docker_group="false" xmpp_domain="" tz_url="$DEFAULT_TENSORZERO_URL" gotify_url="$DEFAULT_GOTIFY_URL" gotify_title="$DEFAULT_GOTIFY_TITLE" llm_api_key="" llm_base_url="$DEFAULT_LLM_BASE_URL" enable_darkirc="false" enable_proxy="false" llm_model="" gateway_host="" xmpp_allow_from="" opencode_model="" opencode_base_url="" kimi_model="" kimi_base_url="" kimi_api_key="" with_pebble="false" with_opencode="false" with_kimi="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --docker-group)    docker_group="true"; shift ;;
          --xmpp-domain)     xmpp_domain="$2"; shift 2 ;;
          --no-health)       HEALTH_OPT_OUT=true; shift ;;
          --no-ssh)          SSH_OPT_OUT=true; shift ;;
          --no-weechat-bootstrap) WEECHAT_BOOTSTRAP_OPT_OUT=true; shift ;;
          --enable-darkirc)  enable_darkirc="true"; shift ;;
          --enable-proxy)    enable_proxy="true"; shift ;;
          --with-nanocode|--nanocode-model|--nanocode-base-url) die "Nanocode worker is retired; remove $1" ;;
          --with-pebble)     with_pebble="true"; shift ;;
          --with-opencode)   with_opencode="true"; shift ;;
          --with-kimi)       with_kimi="true"; shift ;;
          --llm-api-key)     llm_api_key="$2"; shift 2 ;;
          --llm-base-url)    llm_base_url="$2"; shift 2 ;;
          --tensorzero-url)  tz_url="$2"; shift 2 ;;
          --gotify-url)      gotify_url="$2"; shift 2 ;;
          --gotify-title)    gotify_title="$2"; shift 2 ;;
          --opencode-model)    opencode_model="$2"; shift 2 ;;
          --opencode-base-url) opencode_base_url="$2"; shift 2 ;;
          --kimi-model)        kimi_model="$2"; shift 2 ;;
          --kimi-base-url)     kimi_base_url="$2"; shift 2 ;;
          --kimi-api-key-file) kimi_api_key="$(_read_secret_file "$2" "Kimi API key")"; shift 2 ;;
          --kimi-api-key)      die "--kimi-api-key was removed; use --kimi-api-key-file" ;;
          --llm-model)         llm_model="$2"; shift 2 ;;
          --gateway-host)      gateway_host="$2"; shift 2 ;;
          --xmpp-allow-from)   xmpp_allow_from="$2"; shift 2 ;;
          -*)                die "unknown flag: $1" ;;
          *)
            if [[ -z "$names_csv" ]]; then names_csv="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$names_csv" ]] || die "usage: add-tenants <name1,name2,...> [--docker-group]"

      local IFS=','
      local names_array
      read -ra names_array <<< "$names_csv"
      for raw_name in "${names_array[@]}"; do
        local sname
        sname="$(sanitize_name "$(echo "$raw_name" | xargs)")"
        [[ -n "$sname" ]] || continue
        say ""
        # Empty --xmpp-domain -> empty JID, so add_tenant preserves an existing one.
        local sjid=""
        if [[ -n "$xmpp_domain" ]]; then sjid="${sname}@${xmpp_domain}"; fi
        with_tenant_lifecycle_lock "$sname" add_tenant "$sname" "$docker_group" "$sjid" "" "$tz_url" "$gotify_url" "$gotify_title" "$llm_api_key" "$llm_base_url" "$enable_darkirc" "$enable_proxy" "$llm_model" "$gateway_host" "$xmpp_allow_from" "$opencode_model" "$opencode_base_url" "$with_pebble" "$with_opencode" "$with_kimi" "$kimi_model" "$kimi_base_url" "$kimi_api_key"
      done
      ;;

    remove-tenant)
      require_root
      local name="" purge="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --purge) purge="true"; shift ;;
          -*)      die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: remove-tenant <name> [--purge]"
      name="$(sanitize_name "$name")"
      with_tenant_lifecycle_lock "$name" remove_tenant "$name" "$purge"
      ;;

    build-tenant)
      require_root
      local name="" with_wasm="false" require_wasm="false" with_pebble="false" with_opencode="false" with_kimi="false" with_toolchains="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --with-wasm)       with_wasm="true"; shift ;;
          --require-wasm)    require_wasm="true"; shift ;;
          --with-nanocode)   die "Nanocode worker is retired; remove --with-nanocode" ;;
          --with-pebble)     with_pebble="true"; shift ;;
          --with-opencode)   with_opencode="true"; shift ;;
          --with-kimi)       with_kimi="true"; shift ;;
          --with-toolchains) with_toolchains="true"; shift ;;
          -*)                die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: build-tenant <name> [--with-wasm] [--require-wasm] [--with-pebble] [--with-opencode] [--with-kimi] [--with-toolchains]"
      [[ "$require_wasm" != "true" || "$with_wasm" == "true" ]] || die "--require-wasm requires --with-wasm"
      build_tenant "$(sanitize_name "$name")" "$with_wasm" "$with_pebble" "$with_opencode" "$with_kimi" "$with_toolchains" "$require_wasm"
      ;;

    build-all)
      require_root
      local with_wasm="false" with_pebble="false" with_opencode="false" with_kimi="false" with_toolchains="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --with-wasm)       with_wasm="true"; shift ;;
          --with-nanocode)   die "Nanocode worker is retired; remove --with-nanocode" ;;
          --with-pebble)     with_pebble="true"; shift ;;
          --with-opencode)   with_opencode="true"; shift ;;
          --with-kimi)       with_kimi="true"; shift ;;
          --with-toolchains) with_toolchains="true"; shift ;;
          -*)                die "unknown flag: $1" ;;
          *)                 die "unexpected argument: $1" ;;
        esac
      done
      build_all "$with_wasm" "$with_pebble" "$with_opencode" "$with_kimi" "$with_toolchains"
      ;;

    build-darkirc)
      require_root
      local darkirc_tenant=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --tenant) darkirc_tenant="$2"; shift 2 ;;
          -*)       die "unknown flag: $1" ;;
          *)        die "unexpected argument: $1" ;;
        esac
      done
      build_darkirc "$darkirc_tenant"
      ;;

    ensure-darkirc-release)
      require_root
      [[ $# -eq 1 ]] || die "usage: ensure-darkirc-release <name>"
      _lifecycle_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_lifecycle_name" ensure_darkirc_release "$_lifecycle_name"
      ;;

    build-nanocode-worker)
      die "Nanocode worker is retired; build-nanocode-worker is unavailable"
      ;;

    build-pebble-worker)
      require_root
      local no_cache="false"
      local with_toolchains="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --no-cache) no_cache="true"; shift ;;
          --with-toolchains) with_toolchains="true"; shift ;;
          -*)         die "unknown flag: $1" ;;
          *)          die "unexpected argument: $1" ;;
        esac
      done
      build_pebble_worker "$no_cache" "$with_toolchains"
      ;;

    build-opencode-worker)
      require_root
      local no_cache="false"
      local with_toolchains="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --no-cache)         no_cache="true"; shift ;;
          --with-toolchains)  with_toolchains="true"; shift ;;
          -*)                 die "unknown flag: $1" ;;
          *)                  die "unexpected argument: $1" ;;
        esac
      done
      build_opencode_worker "$no_cache" "$with_toolchains"
      ;;

    build-kimi-worker)
      require_root
      local no_cache="false"
      local with_toolchains="false"
      local kimi_code_version=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --no-cache)          no_cache="true"; shift ;;
          --with-toolchains)   with_toolchains="true"; shift ;;
          --kimi-code-version) kimi_code_version="$2"; shift 2 ;;
          -*)                  die "unknown flag: $1" ;;
          *)                   die "unexpected argument: $1" ;;
        esac
      done
      build_kimi_worker "$no_cache" "$with_toolchains" "$kimi_code_version"
      ;;

    build-vision-sidecar)
      require_root
      build_vision_sidecar_image
      ;;

    build-voice-sidecar)
      require_root
      build_voice_sidecar_image
      ;;

    enable-voice|disable-voice)
      require_root
      [[ $# -eq 1 ]] || die "usage: $command_name <name>"
      _voice_name="$(sanitize_name "$1")"
      if [[ "$command_name" == "enable-voice" ]]; then
        configure_voice "$_voice_name" true
        start_tenant_voice "$_voice_name"
      else
        configure_voice "$_voice_name" false
        stop_tenant_voice "$_voice_name"
        remove_tenant_voice_unit "$_voice_name"
      fi
      ;;

    install-wasm)
      require_root
      [[ -n "${1:-}" ]] || die "usage: install-wasm <name>"
      install_wasm_tenant "$1"
      ;;

    install-wasm-all)
      require_root
      install_wasm_all
      ;;

    start-tenant)
      require_root
      [[ -n "${1:-}" ]] || die "usage: start-tenant <name>"
      _lifecycle_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_lifecycle_name" start_tenant "$_lifecycle_name"
      ;;

    stop-tenant)
      require_root
      [[ -n "${1:-}" ]] || die "usage: stop-tenant <name>"
      _lifecycle_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_lifecycle_name" stop_tenant "$_lifecycle_name"
      ;;

    cleanup-legacy-weechat)
      require_root
      [[ $# -eq 1 ]] || die "usage: cleanup-legacy-weechat <name>"
      _lifecycle_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_lifecycle_name" cleanup_legacy_weechat "$_lifecycle_name"
      ;;

    restart-weechat-adapter)
      require_root
      [[ $# -eq 1 ]] || die "usage: restart-weechat-adapter <name>"
      _lifecycle_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_lifecycle_name" restart_weechat_adapter "$_lifecycle_name"
      ;;

    stop-tenant-for-upgrade)
      require_root
      [[ $# -eq 1 ]] || die "usage: stop-tenant-for-upgrade <name>"
      ports_registry_init
      _lifecycle_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_lifecycle_name" stop_tenant_for_upgrade "$_lifecycle_name"
      ;;

    migration-export-boundary)
      require_root
      [[ $# -ge 1 ]] || die "usage: migration-export-boundary <name> [export options]"
      ports_registry_init
      _lifecycle_name="$(sanitize_name "$1")"
      shift
      migration_export_boundary "$_lifecycle_name" "$@"
      ;;

    upgrade-wrapper-boundary)
      require_root
      [[ $# -ge 1 ]] || die "usage: upgrade-wrapper-boundary <name> [upgrade options]"
      ports_registry_init
      _lifecycle_name="$(sanitize_name "$1")"
      shift
      upgrade_wrapper_boundary "$_lifecycle_name" "$@"
      ;;

    validate-upgrade-wrapper-boundary)
      require_root
      [[ $# -eq 1 ]] || die "usage: validate-upgrade-wrapper-boundary <name>"
      ports_registry_require_readonly
      validate_upgrade_wrapper_boundary "$1"
      ;;

    validate-migration-export-boundary)
      require_root
      [[ $# -eq 1 ]] || die "usage: validate-migration-export-boundary <name>"
      ports_registry_require_readonly
      validate_migration_export_boundary "$1"
      ;;

    restart-tenant)
      require_root
      [[ -n "${1:-}" ]] || die "usage: restart-tenant <name>"
      _lifecycle_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_lifecycle_name" restart_tenant "$_lifecycle_name"
      ;;

    stop-writers)
      require_root
      [[ $# -eq 1 ]] || die "usage: stop-writers <name>"
      ports_registry_init
      _writer_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_writer_name" stop_tenant_writers "$_writer_name"
      ;;

    writers-active)
      require_root
      [[ $# -eq 1 ]] || die "usage: writers-active <name>"
      ports_registry_require_readonly
      _writer_name="$(sanitize_name "$1")"
      _writer_rc=0
      with_tenant_lifecycle_lock "$_writer_name" tenant_writers_probe_status "$_writer_name" \
        || _writer_rc=$?
      case "$_writer_rc" in
        0) say "active"; return 0 ;;
        10) say "stopped"; return 1 ;;
        *) say "could not determine tenant writer state" >&2; return 2 ;;
      esac
      ;;

    start-pg)
      require_root
      [[ -n "${1:-}" ]] || die "usage: start-pg <name>"
      # PG-only window for offline population (e.g. hermes_kawarimi loader):
      # daemon stays down while rows are written with nothing racing.
      _pg_only_name="$(sanitize_name "$1")"
      tenant_exists_in_registry "$_pg_only_name" || die "tenant '$1' not found in registry"
      start_tenant_postgres "$_pg_only_name"
      ;;

    stop-pg)
      require_root
      [[ -n "${1:-}" ]] || die "usage: stop-pg <name>"
      _pg_only_name="$(sanitize_name "$1")"
      tenant_exists_in_registry "$_pg_only_name" || die "tenant '$1' not found in registry"
      stop_tenant_postgres "$_pg_only_name"
      ;;

    start-worker|stop-worker|restart-worker|kill-worker)
      require_root
      [[ $# -eq 2 ]] || die "usage: $command_name <name> <pebble|opencode|kimi>"
      case "$command_name" in
        start-worker) start_worker "$1" "$2" ;;
        stop-worker) stop_worker "$1" "$2" ;;
        restart-worker) restart_worker "$1" "$2" ;;
        kill-worker) kill_worker "$1" "$2" ;;
      esac
      ;;

    render-units)
      require_root
      [[ -n "${1:-}" ]] || die "usage: render-units <name>"
      ports_registry_init
      render_tenant_units "$1"
      ;;

    upgrade-tenant)
      require_root
      local name="" target="" source_repo="" do_backup="true" do_render="true"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --target)      target="$2"; shift 2 ;;
          --source-repo) source_repo="$2"; shift 2 ;;
          --no-backup)   do_backup="false"; shift ;;
          --skip-render) do_render="false"; shift ;;
          -*)            die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: upgrade-tenant <name> --target <ref> [--source-repo <path>] [--no-backup] [--skip-render]"
      [[ -n "$target" ]] || die "upgrade-tenant requires an explicit --target <ref> (no implicit default)"
      name="$(sanitize_name "$name")"
      ports_registry_init
      tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
      upgrade_tenant "$name" "$target" "$source_repo" "$do_backup" "$do_render"
      ;;

    rotate-pg-password)
      require_root
      [[ -n "${1:-}" ]] || die "usage: rotate-pg-password <name>"
      rotate_tenant_pg_password "$1"
      ;;

    list-tenants|list)
      ports_registry_init
      list_tenants
      ;;

    status)
      [[ -n "${1:-}" ]] || die "usage: status <name>"
      status_tenant "$1"
      ;;

    darkirc-contact)
      require_root
      local darkirc_operation="${1:-}"
      if [[ "$darkirc_operation" == validate-migration ]]; then
        shift
        local darkirc_input=false darkirc_unbound=false
        while [[ $# -gt 0 ]]; do
          case "$1" in
            --in) [[ $# -ge 2 && "$2" == - ]] || die "validate-migration accepts stdin only (--in -)"; darkirc_input=true; shift 2 ;;
            --json) shift ;;
            --unbound) darkirc_unbound=true; shift ;;
            *) die "unexpected validate-migration argument: $1" ;;
          esac
        done
        [[ "$darkirc_input" == true ]] || die "validate-migration requires --in -"
        run_darkirc_manifest_validator "$darkirc_unbound"
      else
        case "$darkirc_operation" in
          list|status|doctor|exchanges) ports_registry_require_readonly ;;
          *) ports_registry_init ;;
        esac
        darkirc_contact_command "$darkirc_operation" "${@:2}"
      fi
      ;;

    darkirc-health)
      require_root
      local health_tenant="" health_strict=false health_json=false
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --strict) health_strict=true; shift ;;
          --json) health_json=true; shift ;;
          -*) die "unknown flag: $1" ;;
          *) [[ -z "$health_tenant" ]] || die "unexpected argument: $1"; health_tenant="$1"; shift ;;
        esac
      done
      [[ "$health_strict" == true && -n "$health_tenant" ]] \
        || die "darkirc-health requires <tenant> --strict"
      health_tenant="$(sanitize_name "$health_tenant")"
      ports_registry_require_readonly
      if darkirc_health_strict "$health_tenant" 10 "$health_json"; then
        if [[ "$health_json" == true ]]; then
          printf '{"status":"ok","tenant":"%s","irc_connected":true}\n' "$health_tenant"
        fi
      else
        die "strict DarkIRC health failed for tenant '$health_tenant'"
      fi
      ;;

    tokens)
      show_tokens "${1:-}"
      ;;

    configure-gotify)
      require_root
      local name="${1:-}" gotify_url="${2:-}" gotify_title="${3:-}"
      [[ -n "$name" && -n "$gotify_url" ]] || die "usage: configure-gotify <name> <url> [title]"
      name="$(sanitize_name "$name")"
      tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
      write_tenant_gotify_config "$name" "$gotify_url" "$gotify_title"
      configure_gotify_capabilities "$name" "$gotify_url"
      say "Gotify configured for tenant '$name': $gotify_url"
      ;;

    configure-pebble)
      require_root
      local name="" nanogpt_key="" pebble_model=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --nanogpt-api-key) nanogpt_key="$2"; shift 2 ;;
          --model)           pebble_model="$2"; shift 2 ;;
          -*)                die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: configure-pebble <name> --nanogpt-api-key <key> [--model <model>]"
      [[ -n "$nanogpt_key" ]] || die "configure-pebble requires --nanogpt-api-key"
      ports_registry_init
      configure_pebble "$(sanitize_name "$name")" "$nanogpt_key" "$pebble_model"
      ;;

    configure-nanocode)
      die "Nanocode worker is retired; configure-nanocode is unavailable"
      ;;

    configure-opencode)
      require_root
      local name="" oc_model="" oc_base_url=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --model)    oc_model="$2"; shift 2 ;;
          --base-url) oc_base_url="$2"; shift 2 ;;
          -*)         die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: configure-opencode <name> [--model <model>] [--base-url <url>]"
      configure_opencode "$name" "$oc_model" "$oc_base_url"
      ;;

    configure-kimi)
      require_root
      local name="" kimi_model="" kimi_base_url="" kimi_api_key=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --model)    kimi_model="$2"; shift 2 ;;
          --base-url) kimi_base_url="$2"; shift 2 ;;
          --api-key-file) kimi_api_key="$(_read_secret_file "$2" "Kimi API key")"; shift 2 ;;
          --api-key)  die "--api-key was removed; use --api-key-file" ;;
          -*)         die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: configure-kimi <name> [--model <model>] [--base-url <url>] [--api-key-file <path>]"
      configure_kimi "$name" "$kimi_model" "$kimi_base_url" "$kimi_api_key"
      ;;

    configure-study-mcp)
      require_root
      local name="" study_home="" study_state_home="" study_allow_write="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --home) study_home="$2"; shift 2 ;;
          --state-home) study_state_home="$2"; shift 2 ;;
          --allow-write) study_allow_write="true"; shift ;;
          -*)     die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" && -n "$study_home" ]] \
        || die "usage: configure-study-mcp <name> --home <path> [--allow-write] [--state-home <path>]"
      configure_study_mcp "$name" "$study_home" "$study_allow_write" "$study_state_home"
      ;;

    configure-ssh)
      require_root
      local name="" ssh_host="" ssh_user=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --host) ssh_host="$2"; shift 2 ;;
          --user) ssh_user="$2"; shift 2 ;;
          -*)     die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: configure-ssh <name> [--host <host>] [--user <user>]"
      name="$(sanitize_name "$name")"
      ports_registry_init
      tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
      [[ -n "$ssh_host" ]] || ssh_host="127.0.0.1"
      [[ -n "$ssh_user" ]] || ssh_user="$name"
      ensure_ssh_config "$name" "$ssh_host" "$ssh_user"
      provision_tenant_ssh_key "$name" "$ssh_host" "$ssh_user"
      patch_ssh_tool_allowlist "$name"
      say ""
      say "SSH harness configured for tenant '$name' (host=$ssh_host user=$ssh_user)"
      say "Run '$0 restart-tenant $name' to start the daemon and upload the key to the secrets store"
      ;;

    configure-weechat-relay)
      [[ -n "${1:-}" ]] || die "usage: configure-weechat-relay <name>"
      [[ $# -eq 1 ]]   || die "usage: configure-weechat-relay <name> (unexpected extra arguments)"
      require_root
      local name
      name="$(sanitize_name "$1")"
      ports_registry_init
      tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
      configure_weechat_relay "$name"
      ;;

    disable-weechat|enable-weechat)
      [[ -n "${1:-}" ]] || die "usage: $command_name <name>"
      [[ $# -eq 1 ]]   || die "usage: $command_name <name> (unexpected extra arguments)"
      require_root
      local name
      name="$(sanitize_name "$1")"
      ports_registry_init
      tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
      if [[ "$command_name" == "disable-weechat" ]]; then
        set_tenant_weechat_enabled "$name" false \
          || die "failed to stop/disable WeeChat for tenant '$name'"
        say "WeeChat disabled for tenant '$name': units stopped + boot persistence removed; health reports them skipped (never critical)"
      else
        set_tenant_weechat_enabled "$name" true \
          || die "failed to enable/start WeeChat for tenant '$name'"
        say "WeeChat enabled for tenant '$name': units enabled + started; health reports critical when down"
      fi
      ;;

    patch-env)
      require_root
      local name="${1:-}"
      [[ -n "$name" ]] || die "usage: patch-env <name>"
      ports_registry_init
      patch_tenant_env "$name"
      ;;

    patch-env-all)
      require_root
      ports_registry_init
      local names
      names="$(all_tenant_names)"
      [[ -n "$names" ]] || { say "no tenants registered"; exit 0; }
      while IFS= read -r name; do
        patch_tenant_env "$name"
      done <<< "$names"
      ;;

    migrate-owner-scope)
      require_root
      local name="" old_scope="default"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --from) old_scope="$2"; shift 2 ;;
          -*)     die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: migrate-owner-scope <name> [--from <old_scope>]"
      ports_registry_init
      migrate_owner_scope "$name" "$old_scope"
      ;;

    owner-scopes)
      require_root
      local name="${1:-}"
      [[ -n "$name" ]] || die "usage: owner-scopes <name>"
      ports_registry_init
      owner_scope_rows "$name"
      ;;

    backup-tenant)
      require_root
      local name="${1:-}"
      [[ -n "$name" ]] || die "usage: backup-tenant <name>"
      ports_registry_init
      backup_tenant_postgres "$name"
      ;;

    backup-all)
      require_root
      ports_registry_init
      local names
      names="$(all_tenant_names)"
      [[ -n "$names" ]] || { say "no tenants registered"; exit 0; }
      while IFS= read -r name; do
        [[ -n "$name" ]] || continue
        # Subshell so a per-tenant die() doesn't abort the whole fleet backup.
        ( backup_tenant_postgres "$name" ) || say "WARNING: backup failed for $name (continuing)"
      done <<< "$names"
      ;;

    list-backups)
      ports_registry_init
      list_tenant_backups "${1:-}"
      ;;

    restore-tenant)
      require_root
      local name="" file="" confirmed=false
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --yes) confirmed=true; shift ;;
          -*)    die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            elif [[ -z "$file" ]]; then file="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" && -n "$file" ]] || die "usage: restore-tenant <name> <file> --yes"
      ports_registry_init
      restore_tenant_postgres "$name" "$file" "$confirmed"
      ;;

    doctor)
      doctor
      ;;

    help|--help|-h)
      usage
      ;;

    *)
      die "unknown command: $command_name (try: $0 help)"
      ;;
  esac
}

# Only dispatch when executed directly; allows sourcing for tests/inspection.
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  main "$@"
fi

