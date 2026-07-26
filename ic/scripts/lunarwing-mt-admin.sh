#!/usr/bin/env bash
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
PORTS_LOCK_HELD="false"
PROFILE="${LUNARWING_MT_PROFILE:-release}"
SOURCE_REPO="${LUNARWING_MT_SOURCE_REPO:-$LUNARWING_ROOT}"
DARKIRC_SOURCE="${LUNARWING_MT_DARKIRC_SOURCE:-}"
DARKIRC_BIN="${LUNARWING_MT_DARKIRC_BIN:-/usr/local/bin/darkirc}"
# darkfi source for build-darkirc auto-clone. The source is cloned + built as the
# tenant (or invoking) user, never root, so there are no root-owned artifacts and
# the user's own rust toolchain is used. Override for a pinned rev or local mirror.
DARKIRC_REPO="${LUNARWING_MT_DARKIRC_REPO:-https://github.com/darkrenaissance/darkfi}"
DARKIRC_REV="${LUNARWING_MT_DARKIRC_REV:-a05956d412a091e8b54c1cd4f4264c33b941203d}"
OPENRC_ENV_EXEC_SRC="$SCRIPT_DIR/lunarwing-openrc-env-exec.sh"
OPENRC_ENV_EXEC="/usr/local/libexec/lunarwing-openrc-env-exec"
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

# ── Health-check / self-heal pipeline (host-global) ──────────────────────────
# The infra health-check + self-heal pipeline auto-discovers every tenant from
# /etc/init.d and the port registry, so ONE host-global scheduled run covers all
# current and future tenants. Enabled by default for new tenants on OpenRC; opt
# out per add-tenant with --no-health, or fleet-wide with
# LUNARWING_MT_HEALTH_ENABLED=false.
DEFAULT_HEALTH_ENABLED="${LUNARWING_MT_HEALTH_ENABLED:-true}"
HEALTH_INTERVAL_MIN="${LUNARWING_MT_HEALTH_INTERVAL_MIN:-15}"
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


# ── Library sourcing ─────────────────────────────────────────────────────────
#
# Every function below the constants above lives in mt-admin-lib/. Libraries
# define functions and (in detect.sh / containers.sh) their own module-level
# state, but never call anything at source time, so load order is not
# load-bearing here; the order below is for readability. `main` (bottom of this
# file) is the sole entrypoint and the LUNARWING_MT_ADMIN override resolves to
# this file, with libraries found relative to it via SCRIPT_DIR.
#
# `selfcheck` (below) asserts the expected symbols survived the load; `doctor`
# runs it too.

MT_ADMIN_LIB_DIR="${SCRIPT_DIR}/mt-admin-lib"
[[ -d "$MT_ADMIN_LIB_DIR" ]] || {
  printf 'error: mt-admin library directory not found: %s\n' "$MT_ADMIN_LIB_DIR" >&2
  exit 1
}

# shellcheck source=mt-admin-lib/00-common.sh
source "$MT_ADMIN_LIB_DIR/00-common.sh"
# shellcheck source=mt-admin-lib/detect.sh
source "$MT_ADMIN_LIB_DIR/detect.sh"
# shellcheck source=mt-admin-lib/ports-registry.sh
source "$MT_ADMIN_LIB_DIR/ports-registry.sh"
# shellcheck source=mt-admin-lib/tenant-build.sh
source "$MT_ADMIN_LIB_DIR/tenant-build.sh"
# shellcheck source=mt-admin-lib/env-generation.sh
source "$MT_ADMIN_LIB_DIR/env-generation.sh"
# shellcheck source=mt-admin-lib/worker-config.sh
source "$MT_ADMIN_LIB_DIR/worker-config.sh"
# shellcheck source=mt-admin-lib/owner-scope.sh
source "$MT_ADMIN_LIB_DIR/owner-scope.sh"
# shellcheck source=mt-admin-lib/containers.sh
source "$MT_ADMIN_LIB_DIR/containers.sh"
# shellcheck source=mt-admin-lib/postgres.sh
source "$MT_ADMIN_LIB_DIR/postgres.sh"
# shellcheck source=mt-admin-lib/units-systemd.sh
source "$MT_ADMIN_LIB_DIR/units-systemd.sh"
# shellcheck source=mt-admin-lib/units-openrc.sh
source "$MT_ADMIN_LIB_DIR/units-openrc.sh"
# shellcheck source=mt-admin-lib/health-pipeline.sh
source "$MT_ADMIN_LIB_DIR/health-pipeline.sh"
# shellcheck source=mt-admin-lib/tenant-lifecycle.sh
source "$MT_ADMIN_LIB_DIR/tenant-lifecycle.sh"

# ── Library selfcheck ────────────────────────────────────────────────────────
#
# One representative public symbol per library. A dropped `source` line, a
# truncated library, or a function that got moved to the wrong file fails here
# instead of halfway through an add-tenant run. Needs no root and touches
# nothing on disk, so it is safe to run anywhere (CI, pre-commit, doctor).

selfcheck() {
  local missing_count=0 entry fn lib
  local -a expected=(
    "00-common:say" "00-common:die" "00-common:require_cmd"
    "00-common:tenant_home" "00-common:tenant_pg_password"
    "00-common:render_template" "00-common:require_root"
    "detect:detect_init_system" "detect:ensure_init_system"
    "detect:ensure_container_runtime" "detect:_ctr"
    "ports-registry:ports_registry_init" "ports-registry:ports_migrate"
    "ports-registry:ports_migrate_v11" "ports-registry:ports_allocate"
    "ports-registry:ports_get" "ports-registry:ports_deallocate"
    "ports-registry:all_tenant_names"
    "tenant-build:create_tenant_user" "tenant-build:clone_tenant_repo"
    "tenant-build:build_tenant" "tenant-build:install_wasm_tenant"
    "env-generation:write_tenant_lunarwing_env"
    "env-generation:write_tenant_bridge_env"
    "env-generation:configure_weechat_relay" "env-generation:patch_tenant_env"
    "worker-config:ensure_external_worker_config"
    "worker-config:provision_tenant_ssh_key" "worker-config:configure_pebble"
    "owner-scope:migrate_owner_scope" "owner-scope:owner_scope_rows"
    "containers:start_tenant_nanocode" "containers:start_tenant_opencode"
    "containers:start_tenant_pebble" "containers:start_tenant_vision"
    "postgres:start_tenant_postgres" "postgres:backup_tenant_postgres"
    "postgres:restore_tenant_postgres" "postgres:rotate_tenant_pg_password"
    "units-systemd:render_tenant_systemd_units" "units-systemd:_systemctl_user"
    "units-systemd:start_tenant_systemd" "units-systemd:uninstall_tenant_systemd"
    "units-openrc:render_tenant_openrc_units" "units-openrc:start_tenant_openrc"
    "units-openrc:install_openrc_env_exec" "units-openrc:render_worker_openrc_unit"
    "health-pipeline:ensure_health_pipeline" "health-pipeline:remove_health_pipeline"
    "tenant-lifecycle:add_tenant" "tenant-lifecycle:remove_tenant"
    "tenant-lifecycle:start_tenant" "tenant-lifecycle:stop_tenant"
    "tenant-lifecycle:upgrade_tenant" "tenant-lifecycle:status_tenant"
    "tenant-lifecycle:list_tenants" "tenant-lifecycle:doctor"
  )

  for entry in "${expected[@]}"; do
    lib="${entry%%:*}"
    fn="${entry#*:}"
    if ! declare -f "$fn" >/dev/null 2>&1; then
      say "MISSING: $fn (expected from mt-admin-lib/${lib}.sh)"
      missing_count=$((missing_count + 1))
    fi
  done

  if [[ "$missing_count" -ne 0 ]]; then
    die "selfcheck: $missing_count of ${#expected[@]} expected symbols undefined after sourcing"
  fi
  say "selfcheck: ${#expected[@]} expected symbols present across 13 libraries"
}

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
    --with-nanocode                Select the nanocode worker for this tenant (recorded
                                   in the port registry; start-tenant only starts workers
                                   the tenant selected — see --with-* on build-tenant to
                                   also build the image). Off by default.
    --with-pebble                  Select the pebble worker for this tenant (as above)
    --with-opencode                Select the opencode worker for this tenant (as above)
    --nanocode-model <model>       Override the nanocode worker's LLM model
                                   (written to lunarwing.env as NANOCODE_MODEL)
    --nanocode-base-url <url>      Override the nanocode worker's TensorZero baseURL
                                   (written to lunarwing.env as NANOCODE_BASE_URL)
    --opencode-model <model>       Override the opencode worker's LLM model
                                   (written to lunarwing.env as OPENCODE_MODEL)
    --opencode-base-url <url>      Override the opencode worker's TensorZero baseURL
                                   (written to lunarwing.env as OPENCODE_BASE_URL)
    --llm-model <model>            Override LLM_MODEL (default:
                                   tensorzero::function_name::lunarwing)
    --gateway-host <host>          Override GATEWAY_HOST bind address (default:
                                   127.0.0.1; use 0.0.0.0 for LAN access)
    --xmpp-allow-from <jids>       Comma-separated extra XMPP JIDs allowed to DM
                                   the agent (added to the tenant's own JID;
                                   written to both lunarwing.env and xmpp-bridge.env)

   add-tenants <names> [options]    Comma-separated list (e.g. "Ruffles,Miyuki")
     (same options as add-tenant apply to all, including --enable-darkirc,
      --nanocode-model/--nanocode-base-url, --opencode-model/--opencode-base-url,
      --llm-model, --gateway-host, --xmpp-allow-from, and --no-weechat-bootstrap)

  remove-tenant <name>             Stop services, deallocate ports
    --purge                        Also delete OS user and home directory

  build-tenant <name>             Build binaries for one tenant (OOM-safe flock)
    --with-wasm                    Also build WASM extensions
    --require-wasm                 Fail instead of preserving stale WASM (requires --with-wasm)
    --with-nanocode                Also build the nanocode worker Docker image
    --with-pebble                  Also build the pebble worker Docker image
    --with-opencode                Also build the opencode worker Docker image

  build-all                        Build each tenant sequentially
    --with-wasm                    Also build WASM extensions
    --with-nanocode                Also build the nanocode worker Docker image
    --with-pebble                  Also build the pebble worker Docker image
    --with-opencode                Also build the opencode worker Docker image

  build-nanocode-worker            Build the nanocode worker Docker image
    --no-cache                     Force a full rebuild without Docker cache

  build-pebble-worker             Build the pebble worker Docker image
    --no-cache                     Force a full rebuild without Docker cache

  build-opencode-worker            Build the opencode worker Docker image
    --no-cache                     Force a full rebuild without Docker cache
    --with-toolchains              Include Rust/Go/C++ toolchains (default: slim)

  build-vision-sidecar             Build the LunarVision OCR sidecar Docker image

  build-darkirc                   Build darkirc daemon from external source
                                   (shared binary, not per-tenant)

  install-wasm <name>             Install built WASM tools/channels into tenant state dir
  install-wasm-all                Install WASM for all tenants

  start-tenant <name>             Start all services for a tenant
  stop-tenant <name>              Stop all services for a tenant
  restart-tenant <name>           Stop then start
  render-units <name>             Re-render a tenant's service units from the current
                                  generator (no restart; applies init-script changes)
  upgrade-tenant <name> --target <ref>
                                  In-place upgrade: backup, fetch, clean-tree gate,
                                  stop, checkout <ref> (as tenant user), rebuild
                                  with WASM, re-render units, patch env, start.
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

  configure-nanocode <name>        Set nanocode worker LLM overrides for a tenant
    --model <model>                TensorZero model (NANOCODE_MODEL; any string)
    --base-url <url>               TensorZero baseURL (NANOCODE_BASE_URL; full URL)
                                   (restart the worker after: stop-tenant && start-tenant)

  configure-opencode <name>        Set opencode worker LLM overrides for a tenant
    --model <model>                TensorZero model (OPENCODE_MODEL; any string)
    --base-url <url>               TensorZero baseURL (OPENCODE_BASE_URL; full URL)
                                   (restart the worker after: stop-tenant && start-tenant)

   configure-ssh <name>             Provision SSH harness for an existing tenant
     --host <host>                  SSH host (default: 127.0.0.1)
     --user <user>                  SSH user (default: tenant name)
                                   (restart the tenant after to upload the key: restart-tenant)

  configure-weechat-relay <name>   Generate WeeChat relay config for a tenant.
                                   Preserves existing config: fails on any
                                   non-empty ~/.config/weechat without modifying it.

  patch-env <name>                 Add missing env vars (e.g. ORCHESTRATOR_PORT)
  patch-env-all                    Patch env for all registered tenants

  migrate-owner-scope <name>       Rekey DB data from 'default' to tenant scope
    --from <old_scope>             Old owner_id (default: 'default')
                                   (run after patch-env adds LUNARWING_OWNER_ID)
  owner-scopes <name>              Print restored owner scopes and row counts

  list-tenants                     Show all tenants with ports and status
  status <name>                    Detailed status for one tenant
  tokens [name]                    Print gateway auth tokens (all or one)
  doctor                           System dependency and health checks
  selfcheck                        Verify every mt-admin-lib/ library loaded and
                                   its expected symbols are defined (no root
                                   needed; also run as part of doctor)

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
      local name="" docker_group="false" xmpp_jid="" xmpp_password="" tz_url="$DEFAULT_TENSORZERO_URL" gotify_url="$DEFAULT_GOTIFY_URL" gotify_title="$DEFAULT_GOTIFY_TITLE" llm_api_key="" llm_base_url="$DEFAULT_LLM_BASE_URL" enable_darkirc="false" enable_proxy="false" nanocode_model="" nanocode_base_url="" llm_model="" gateway_host="" xmpp_allow_from="" opencode_model="" opencode_base_url="" with_nanocode="false" with_pebble="false" with_opencode="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --docker-group)    docker_group="true"; shift ;;
          --xmpp-jid)        xmpp_jid="$2"; shift 2 ;;
          --no-health)       HEALTH_OPT_OUT=true; shift ;;
          --no-ssh)          SSH_OPT_OUT=true; shift ;;
          --no-weechat-bootstrap) WEECHAT_BOOTSTRAP_OPT_OUT=true; shift ;;
          --enable-darkirc)  enable_darkirc="true"; shift ;;
          --enable-proxy)    enable_proxy="true"; shift ;;
          --with-nanocode)   with_nanocode="true"; shift ;;
          --with-pebble)     with_pebble="true"; shift ;;
          --with-opencode)   with_opencode="true"; shift ;;
          --xmpp-password)   xmpp_password="$2"; shift 2 ;;
          --llm-api-key)     llm_api_key="$2"; shift 2 ;;
          --llm-base-url)    llm_base_url="$2"; shift 2 ;;
          --tensorzero-url)  tz_url="$2"; shift 2 ;;
          --gotify-url)      gotify_url="$2"; shift 2 ;;
          --gotify-title)    gotify_title="$2"; shift 2 ;;
          --nanocode-model)    nanocode_model="$2"; shift 2 ;;
          --nanocode-base-url) nanocode_base_url="$2"; shift 2 ;;
          --opencode-model)    opencode_model="$2"; shift 2 ;;
          --opencode-base-url) opencode_base_url="$2"; shift 2 ;;
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
      add_tenant "$name" "$docker_group" "$xmpp_jid" "$xmpp_password" "$tz_url" "$gotify_url" "$gotify_title" "$llm_api_key" "$llm_base_url" "$enable_darkirc" "$enable_proxy" "$nanocode_model" "$nanocode_base_url" "$llm_model" "$gateway_host" "$xmpp_allow_from" "$opencode_model" "$opencode_base_url" "$with_nanocode" "$with_pebble" "$with_opencode"
      ;;

    add-tenants)
      require_root
      local names_csv="" docker_group="false" xmpp_domain="" tz_url="$DEFAULT_TENSORZERO_URL" gotify_url="$DEFAULT_GOTIFY_URL" gotify_title="$DEFAULT_GOTIFY_TITLE" llm_api_key="" llm_base_url="$DEFAULT_LLM_BASE_URL" enable_darkirc="false" enable_proxy="false" nanocode_model="" nanocode_base_url="" llm_model="" gateway_host="" xmpp_allow_from="" opencode_model="" opencode_base_url="" with_nanocode="false" with_pebble="false" with_opencode="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --docker-group)    docker_group="true"; shift ;;
          --xmpp-domain)     xmpp_domain="$2"; shift 2 ;;
          --no-health)       HEALTH_OPT_OUT=true; shift ;;
          --no-ssh)          SSH_OPT_OUT=true; shift ;;
          --no-weechat-bootstrap) WEECHAT_BOOTSTRAP_OPT_OUT=true; shift ;;
          --enable-darkirc)  enable_darkirc="true"; shift ;;
          --enable-proxy)    enable_proxy="true"; shift ;;
          --with-nanocode)   with_nanocode="true"; shift ;;
          --with-pebble)     with_pebble="true"; shift ;;
          --with-opencode)   with_opencode="true"; shift ;;
          --llm-api-key)     llm_api_key="$2"; shift 2 ;;
          --llm-base-url)    llm_base_url="$2"; shift 2 ;;
          --tensorzero-url)  tz_url="$2"; shift 2 ;;
          --gotify-url)      gotify_url="$2"; shift 2 ;;
          --gotify-title)    gotify_title="$2"; shift 2 ;;
          --nanocode-model)    nanocode_model="$2"; shift 2 ;;
          --nanocode-base-url) nanocode_base_url="$2"; shift 2 ;;
          --opencode-model)    opencode_model="$2"; shift 2 ;;
          --opencode-base-url) opencode_base_url="$2"; shift 2 ;;
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
        add_tenant "$sname" "$docker_group" "$sjid" "" "$tz_url" "$gotify_url" "$gotify_title" "$llm_api_key" "$llm_base_url" "$enable_darkirc" "$enable_proxy" "$nanocode_model" "$nanocode_base_url" "$llm_model" "$gateway_host" "$xmpp_allow_from" "$opencode_model" "$opencode_base_url" "$with_nanocode" "$with_pebble" "$with_opencode"
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
      remove_tenant "$name" "$purge"
      ;;

    build-tenant)
      require_root
      local name="" with_wasm="false" require_wasm="false" with_nanocode="false" with_pebble="false" with_opencode="false" with_toolchains="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --with-wasm)       with_wasm="true"; shift ;;
          --require-wasm)    require_wasm="true"; shift ;;
          --with-nanocode)   with_nanocode="true"; shift ;;
          --with-pebble)     with_pebble="true"; shift ;;
          --with-opencode)   with_opencode="true"; shift ;;
          --with-toolchains) with_toolchains="true"; shift ;;
          -*)                die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: build-tenant <name> [--with-wasm] [--require-wasm] [--with-nanocode] [--with-pebble] [--with-opencode] [--with-toolchains]"
      [[ "$require_wasm" != "true" || "$with_wasm" == "true" ]] || die "--require-wasm requires --with-wasm"
      build_tenant "$(sanitize_name "$name")" "$with_wasm" "$with_nanocode" "$with_pebble" "$with_opencode" "$with_toolchains" "$require_wasm"
      ;;

    build-all)
      require_root
      local with_wasm="false" with_nanocode="false" with_pebble="false" with_opencode="false" with_toolchains="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --with-wasm)       with_wasm="true"; shift ;;
          --with-nanocode)   with_nanocode="true"; shift ;;
          --with-pebble)     with_pebble="true"; shift ;;
          --with-opencode)   with_opencode="true"; shift ;;
          --with-toolchains) with_toolchains="true"; shift ;;
          -*)                die "unknown flag: $1" ;;
          *)                 die "unexpected argument: $1" ;;
        esac
      done
      build_all "$with_wasm" "$with_nanocode" "$with_pebble" "$with_opencode" "$with_toolchains"
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

    build-nanocode-worker)
      require_root
      local no_cache="false"
      local with_toolchains="false"
      local nanocode_ref="v1.2.28"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --no-cache)        no_cache="true"; shift ;;
          --with-toolchains) with_toolchains="true"; shift ;;
          --nanocode-ref)    nanocode_ref="$2"; shift 2 ;;
          -*)                die "unknown flag: $1" ;;
          *)                 die "unexpected argument: $1" ;;
        esac
      done
      build_nanocode_worker "$no_cache" "$with_toolchains" "$nanocode_ref"
      ;;

    build-pebble-worker)
      require_root
      local no_cache="false"
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --no-cache) no_cache="true"; shift ;;
          -*)         die "unknown flag: $1" ;;
          *)          die "unexpected argument: $1" ;;
        esac
      done
      build_pebble_worker "$no_cache"
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

    build-vision-sidecar)
      require_root
      build_vision_sidecar_image
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
      start_tenant "$1"
      ;;

    stop-tenant)
      require_root
      [[ -n "${1:-}" ]] || die "usage: stop-tenant <name>"
      stop_tenant "$1"
      ;;

    restart-tenant)
      require_root
      [[ -n "${1:-}" ]] || die "usage: restart-tenant <name>"
      restart_tenant "$1"
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
      require_root
      local name="" nc_model="" nc_base_url=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --model)    nc_model="$2"; shift 2 ;;
          --base-url) nc_base_url="$2"; shift 2 ;;
          -*)         die "unknown flag: $1" ;;
          *)
            if [[ -z "$name" ]]; then name="$1"; shift
            else die "unexpected argument: $1"
            fi
            ;;
        esac
      done
      [[ -n "$name" ]] || die "usage: configure-nanocode <name> [--model <model>] [--base-url <url>]"
      configure_nanocode "$name" "$nc_model" "$nc_base_url"
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

    selfcheck)
      selfcheck
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
