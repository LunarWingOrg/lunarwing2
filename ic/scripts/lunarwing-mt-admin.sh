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
DARKIRC_BIN="${LUNARWING_MT_DARKIRC_BIN:-/usr/local/bin/darkirc}"
# Managed exchange accepts only this pinned, root-attested DarkFi profile. Build
# recipes execute in a disposable tree as a dedicated unprivileged account.
DARKIRC_REPO="${LUNARWING_MT_DARKIRC_REPO:-https://github.com/darkrenaissance/darkfi}"
DARKIRC_APPROVED_REV="a05956d412a091e8b54c1cd4f4264c33b941203d"
DARKIRC_REV="${LUNARWING_MT_DARKIRC_REV:-$DARKIRC_APPROVED_REV}"
DARKIRC_BUILD_USER="${LUNARWING_MT_DARKIRC_BUILD_USER:-lunarwing-build}"
DARKIRC_BUILD_HOME="${LUNARWING_MT_DARKIRC_BUILD_HOME:-/var/cache/lunarwing/build-home}"
DARKIRC_BUILD_ROOT="${LUNARWING_MT_DARKIRC_BUILD_ROOT:-/var/cache/lunarwing/darkirc-builds}"
DARKIRC_COMPAT_FILE="${LUNARWING_MT_DARKIRC_COMPAT_FILE:-/etc/lunarwing/darkirc-compat.json}"
DARKIRC_KEY_HELPER="${LUNARWING_MT_DARKIRC_KEY_HELPER:-/usr/local/libexec/lunarwing-darkirc-key-helper}"
DARKIRC_KEY_HELPER_BUILD_ROOT="${LUNARWING_MT_DARKIRC_KEY_HELPER_BUILD_ROOT:-/var/cache/lunarwing/darkirc-key-helper-target}"
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

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

acquire_ports_lock() {
  local registry_dir
  [[ "$PORTS_LOCK_HELD" == "true" ]] && return 0
  require_cmd flock
  registry_dir="$(dirname "$PORTS_REGISTRY")"
  mkdir -p "$registry_dir"
  exec 199<"$registry_dir"
  flock -x 199
  PORTS_LOCK_HELD="true"
}

release_ports_lock() {
  [[ "$PORTS_LOCK_HELD" == "true" ]] || return 0
  flock -u 199
  exec 199>&-
  PORTS_LOCK_HELD="false"
}

with_tenant_lifecycle_lock() {
  local name="$1"
  shift
  [[ "$name" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]] \
    || die "invalid tenant name for lifecycle lock"
  require_cmd flock
  [[ ! -L "$TENANT_LIFECYCLE_LOCK_DIR" ]]
  install -d -o root -g root -m 0700 "$TENANT_LIFECYCLE_LOCK_DIR"
  [[ "$(stat -c '%u:%a' "$TENANT_LIFECYCLE_LOCK_DIR" 2>/dev/null)" == "0:700" ]] \
    || die "unsafe tenant lifecycle lock directory"
  local lock_fd result
  if [[ -n "${LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD:-}" ]]; then
    lock_fd="$LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD"
    [[ "$lock_fd" =~ ^[0-9]+$ \
      && "${LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_TENANT:-}" == "$name" \
      && -e "/proc/self/fd/$lock_fd" \
      && "$(stat -c '%d:%i' "/proc/self/fd/$lock_fd" 2>/dev/null)" \
        == "$(stat -c '%d:%i' "$TENANT_LIFECYCLE_LOCK_DIR/${name}.lock" 2>/dev/null)" ]] \
      || die "invalid active tenant lifecycle lock"
    flock -x -n "$lock_fd" || die "active tenant lifecycle lock is not held"
    "$@"
    return
  fi
  if [[ -n "${LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_FD:-}" ]]; then
    lock_fd="$LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_FD"
    [[ "$lock_fd" =~ ^[0-9]+$ \
      && "${LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_TENANT:-}" == "$name" \
      && -e "/proc/self/fd/$lock_fd" \
      && "$(stat -c '%d:%i' "/proc/self/fd/$lock_fd" 2>/dev/null)" \
        == "$(stat -c '%d:%i' "$TENANT_LIFECYCLE_LOCK_DIR/${name}.lock" 2>/dev/null)" ]] \
      || die "invalid inherited tenant lifecycle lock"
    flock -x -n "$lock_fd" || die "inherited tenant lifecycle lock is not held"
    LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD="$lock_fd"
    LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_TENANT="$name"
    "$@" || result=$?
    result="${result:-0}"
    unset LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_TENANT
    return "$result"
  fi
  exec {lock_fd}>"$TENANT_LIFECYCLE_LOCK_DIR/${name}.lock" || return 1
  if ! flock -x "$lock_fd"; then
    exec {lock_fd}>&-
    return 1
  fi
  LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD="$lock_fd"
  LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_TENANT="$name"
  "$@" || result=$?
  result="${result:-0}"
  unset LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_TENANT
  flock -u "$lock_fd" || result=1
  exec {lock_fd}>&-
  return "$result"
}

generate_token() {
  if command -v od >/dev/null 2>&1; then
    dd if=/dev/urandom bs=32 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n'
  else
    printf 'replace-with-random-token-%s' "$(date +%s)"
  fi
}

# Generate a non-secret registry identity. Callers hold the ports lock while
# checking uniqueness and persisting the value.
generate_darkirc_scope_id() {
  require_cmd od
  local scope_id
  scope_id="$(dd if=/dev/urandom bs=16 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n')"
  [[ "$scope_id" =~ ^[0-9a-f]{32}$ && "$scope_id" != "00000000000000000000000000000000" ]] \
    || die "failed to generate DarkIRC scope ID"
  printf '%s' "$scope_id"
}

generate_unique_darkirc_scope_id() {
  local scope_id attempts=0 jq_status
  while (( attempts < 16 )); do
    scope_id="$(generate_darkirc_scope_id)"
    if jq -e --arg scope "$scope_id" \
        '[.tenants // {} | to_entries[] | select(.value.darkirc_scope_id == $scope)] | length == 0' \
        "$PORTS_REGISTRY" >/dev/null 2>&1; then
      printf '%s' "$scope_id"
      return 0
    fi
    jq_status=$?
    [[ "$jq_status" == "1" ]] || die "failed to validate DarkIRC scope ID uniqueness"
    attempts=$((attempts + 1))
  done
  die "failed to allocate unique DarkIRC scope ID"
}

ensure_darkirc_scope_id() {
  local name="$1" current matches scope_id tmp
  local had_lock="$PORTS_LOCK_HELD"
  acquire_ports_lock
  jq -e --arg name "$name" \
    '(.tenants // {}) | has($name) and (.[$name].enable_darkirc == true)' \
    "$PORTS_REGISTRY" >/dev/null 2>&1 || die "tenant '$name' is missing or DarkIRC is disabled"
  current="$(jq -r --arg name "$name" '.tenants[$name].darkirc_scope_id // empty' \
    "$PORTS_REGISTRY" 2>/dev/null)" || die "failed to read DarkIRC scope ID for tenant '$name'"
  if [[ -n "$current" ]]; then
    validate_darkirc_scope_id "$current"
    matches="$(jq -r --arg scope "$current" \
      '[.tenants // {} | to_entries[] | select(.value.darkirc_scope_id == $scope)] | length' \
      "$PORTS_REGISTRY" 2>/dev/null)" || die "failed to validate DarkIRC scope ID for tenant '$name'"
    [[ "$matches" == "1" ]] || die "duplicate DarkIRC scope ID for tenant '$name'"
    [[ "$had_lock" == "true" ]] || release_ports_lock
    printf '%s' "$current"
    return 0
  fi

  scope_id="$(generate_unique_darkirc_scope_id)"
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  if ! jq --arg name "$name" --arg scope "$scope_id" \
      '.tenants[$name].darkirc_scope_id = $scope' "$PORTS_REGISTRY" >"$tmp" 2>/dev/null; then
    rm -f "$tmp"
    die "failed to store DarkIRC scope ID for tenant '$name'"
  fi
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  [[ "$had_lock" == "true" ]] || release_ports_lock
  printf '%s' "$scope_id"
}

sanitize_name() {
  local raw="$1"
  printf '%s' "$raw" | tr '[:upper:]' '[:lower:]' | tr -cs 'a-z0-9-' '-' | sed 's/^-//;s/-$//'
}

tenant_home() { printf '/home/%s' "$1"; }
tenant_lw_root() { printf '%s/lunarwing' "$(tenant_home "$1")"; }
tenant_repo() { printf '%s/ic' "$(tenant_lw_root "$1")"; }
tenant_env_dir() { printf '%s/env' "$(tenant_lw_root "$1")"; }
tenant_quadlet_dir() { printf '%s/.config/containers/systemd' "$(tenant_home "$1")"; }
tenant_state_dir() { printf '%s/state' "$(tenant_lw_root "$1")"; }
tenant_log_dir() { printf '%s/logs' "$(tenant_lw_root "$1")"; }
tenant_run_dir() { printf '%s/run' "$(tenant_lw_root "$1")"; }
STUDY_NOTES_CONTAINER_HOME="/opt/lw2studymcp"
STUDY_NOTES_CONTAINER_STATE_HOME="/var/lib/lw2studymcp"
OPENCODE_CONTAINER_UID="10001"
OPENCODE_CONTAINER_GID="10001"
OPENCODE_USERNS="keep-id:uid=${OPENCODE_CONTAINER_UID},gid=${OPENCODE_CONTAINER_GID}"
OPENCODE_ROOTLESS_LAUNCH_CONTRACT="userns=${OPENCODE_USERNS}"

KIMI_CONTAINER_UID="10002"
KIMI_CONTAINER_GID="10002"
KIMI_USERNS="keep-id:uid=${KIMI_CONTAINER_UID},gid=${KIMI_CONTAINER_GID}"
KIMI_ROOTLESS_LAUNCH_CONTRACT="userns=${KIMI_USERNS}"

# ── Container config-hash tracking ────────────────────────────────────────────
#
# Quadlet and imperative `podman run` both create a container with config
# (env vars, volumes, ports) baked in at creation time. `systemctl restart`
# and `podman start` only restart the EXISTING container — they do NOT pick
# up changes to the .container file or a re-rendered `podman run` command.
# This causes stale-env bugs on upgrade (e.g. AGENT_AUTH_TOKEN missing,
# SSH_AUTH_SOCK not mounted) because the old container survives the re-render.
#
# Fix: after rendering a quadlet or before an imperative `podman run`, compute
# a hash of the config source and compare it to the hash stored when the
# container was last created. If they differ, force-recreate the container.
# The hash is stored in a sidecar file next to the container's quadlet/state.

# Directory for config-hash sidecars (created on first use).
_hash_dir() { printf '%s/.config/lunarwing/container-hashes' "$(tenant_home "$1")"; }

# Hash a config file with an optional launch contract for arguments that are not
# represented in that file. Empty contracts retain the historical file hash.
_container_config_hash() {  # <config-file> [launch-contract]
  local config_file="$1" contract="${2:-}" file_hash
  file_hash="$(sha256sum "$config_file" 2>/dev/null | cut -d' ' -f1 || true)"
  [[ -n "$file_hash" ]] || return 1
  if [[ -z "$contract" ]]; then
    printf '%s\n' "$file_hash"
  else
    printf '%s\n%s\n' "$file_hash" "$contract" | sha256sum | cut -d' ' -f1
  fi
}

# Compute and store the hash of a config file (e.g. a .container quadlet).
# Usage: _store_container_hash <tenant> <container-name> <config-file> [launch-contract]
_store_container_hash() {
  local name="$1" container="$2" config_file="$3" contract="${4:-}" hdir hash
  hdir="$(_hash_dir "$name")"
  mkdir -p "$hdir" 2>/dev/null || true
  hash="$(_container_config_hash "$config_file" "$contract" || true)"
  [[ -n "$hash" ]] && printf '%s\n' "$hash" >"$hdir/${container}.hash"
}

# Check whether the config file's hash matches the stored hash.
# Returns 0 when missing/mismatched and recreation is needed, 1 when unchanged.
# Usage: _container_config_changed <tenant> <container-name> <config-file> [launch-contract]
_container_config_changed() {
  local name="$1" container="$2" config_file="$3" contract="${4:-}" hdir stored current
  hdir="$(_hash_dir "$name")"
  [[ -f "$config_file" ]] || return 1  # no config file = no opinion
  current="$(_container_config_hash "$config_file" "$contract" || true)"
  [[ -n "$current" ]] || return 1      # can't hash = don't force recreate
  stored=""
  [[ -f "$hdir/${container}.hash" ]] && stored="$(cat "$hdir/${container}.hash" 2>/dev/null || true)"
  [[ -z "$stored" || "$stored" != "$current" ]]
}

# Force-recreate a Quadlet-managed container: stop the service, remove the
# stale container, then start the service (Quadlet re-runs `podman run`).
# Usage: _recreate_quadlet_container <tenant> <service-name> <container-name>
_recreate_quadlet_container() {
  local name="$1" svc="$2" container="$3"
  say "config changed for $container; force-recreating"
  _systemctl_user "$name" stop "$svc" >/dev/null 2>&1 || true
  _ctr "$name" rm -f "$container" >/dev/null 2>&1 || true
}

tenant_darkirc_enabled() {
  local name="$1"
  local val
  val="$(jq -r ".tenants[\"$name\"].enable_darkirc // false" "$PORTS_REGISTRY" 2>/dev/null)"
  [[ "$val" == "true" ]]
}

tenant_proxy_enabled() {
  local name="$1"
  local val
  val="$(jq -r ".tenants[\"$name\"].enable_proxy // false" "$PORTS_REGISTRY" 2>/dev/null)"
  [[ "$val" == "true" ]]
}

# Whether WeeChat is enabled for this tenant. Default TRUE: the flag is
# absent on tenants created before the health-gate existed, and a missing or
# unreadable registry must fail safe to the old behavior (weechat managed).
# Used to make disable-weechat authoritative for lifecycle, not just health
# classification (ISSUE-006). Explicit ==false test: jq `//` misreads false.
tenant_weechat_enabled() {
  local name="$1"
  local val
  val="$(jq -r --arg name "$name" 'if .tenants[$name].enable_weechat == false then "false" else "true" end' "$PORTS_REGISTRY" 2>/dev/null)"
  [[ "$val" != "false" ]]
}

# Whether a supported external worker (pebble|opencode|kimi) is selected for this
# tenant. The selection is persisted at add-tenant into
# .tenants[<name>].workers.<worker> and gates start_tenant_<worker> — otherwise a
# tenant would start every worker whose SHARED host image happens to exist,
# regardless of what was chosen (see docs/proposals/PER_TENANT_WORKER_GATING.md).
# Absent key = false (OFF): a tenant provisioned before this flag existed does not
# auto-start workers until re-selected via `add-tenant <name> --with-<worker>`.
tenant_worker_enabled() {
  local name="$1" worker="$2"
  local val
  val="$(jq -r ".tenants[\"$name\"].workers[\"$worker\"] // false" "$PORTS_REGISTRY" 2>/dev/null)"
  [[ "$val" == "true" ]]
}

# Per-tenant PostgreSQL password. The source of truth is a 0600, tenant-owned
# secret file, generated once (hex → URL-safe inside DATABASE_URL) and reused so
# it stays STABLE across restarts/reconfigures — POSTGRES_PASSWORD only
# initialises an EMPTY datadir, so the value must not drift after first init.
# Migration-safe: if a tenant was already provisioned (its lunarwing.env carries a
# DATABASE_URL password), that value is preserved so an already-initialised DB
# keeps working; only brand-new tenants get a fresh random password. Use
# `rotate-pg-password` to deliberately move an existing tenant onto a random one.
# Never logged (repo rule).
tenant_pg_password() {
  local name="$1" f envf existing pw
  f="$(tenant_env_dir "$name")/pg.secret"
  if [[ -s "$f" ]]; then
    cat "$f"   # callers use $(...), which strips the trailing newline
    return 0
  fi
  envf="$(tenant_env_dir "$name")/lunarwing.env"
  if [[ -f "$envf" ]]; then
    existing="$(sed -n 's#^DATABASE_URL=postgres://lunarwing:\([^@]*\)@.*#\1#p' "$envf" | head -1)"
  fi
  if [[ -n "${existing:-}" ]]; then
    pw="$existing"                       # preserve an already-initialised DB's password
  else
    pw="$(generate_token | cut -c1-32)"  # fresh tenant → random 128-bit hex
  fi
  mkdir -p "$(dirname "$f")"
  ( umask 077; printf '%s\n' "$pw" > "$f" )
  chown "$name:$name" "$f" 2>/dev/null || true
  printf '%s\n' "$pw"
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
    --with-toolchains              Include toolchains in selected opencode and kimi
                                   worker images (default: slim)

  build-all                        Build each tenant sequentially
    --with-wasm                    Also build WASM extensions
    --with-pebble                  Also build the pebble worker Docker image
    --with-opencode                Also build the opencode worker Docker image
    --with-kimi                    Also build the kimi worker Docker image
    --with-toolchains              Include toolchains in selected opencode and kimi
                                   worker images (default: slim)

  build-pebble-worker             Build the pebble worker Docker image
    --no-cache                     Force a full rebuild without Docker cache

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

  build-darkirc                   Build darkirc daemon from external source
                                   (shared binary, not per-tenant)

  install-wasm <name>             Install built WASM tools/channels into tenant state dir
  install-wasm-all                Install WASM for all tenants

  start-tenant <name>             Start all services for a tenant
  stop-tenant <name>              Stop all services for a tenant
  stop-tenant-for-upgrade <name>  Verify settled DarkIRC state and stop atomically
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
render_template() {
  local template="$1" out="$2"
  shift 2
  render_template_content "$template" "$@" >"$out"
}

render_template_content() {
  local template="$1"
  shift
  [[ "$template" = /* ]] || template="$TEMPLATES_DIR/$template"
  [[ -f "$template" ]] || die "template not found: $template"
  local content pair var val
  content="$(<"$template")"
  for pair; do
    var="${pair%%=*}"
    val="${pair#*=}"
    content="${content//__${var}__/${val}}"
  done
  printf '%s\n' "$content"
}
# ── Root check ────────────────────────────────────────────────────────────────

require_root() {
  [[ "${EUID}" -eq 0 ]] || die "must run as root (use sudo)"
}

# ── Init system detection ────────────────────────────────────────────────────

detect_init_system() {
  local override="${LUNARWING_SERVICE_MANAGER:-}"
  if [[ -n "$override" ]]; then
    case "${override,,}" in
      systemd|systemd-user) printf 'systemd'; return 0 ;;
      openrc)               printf 'openrc';  return 0 ;;
      *) die "unsupported service manager override '$override'; use systemd or openrc" ;;
    esac
  fi

  if [[ -e /run/openrc/softlevel ]]; then printf 'openrc'; return 0; fi
  if [[ -e /run/systemd/system ]];   then printf 'systemd'; return 0; fi

  if command -v rc-service >/dev/null 2>&1 && ! command -v systemctl >/dev/null 2>&1; then
    printf 'openrc'; return 0
  fi
  if command -v systemctl >/dev/null 2>&1; then printf 'systemd'; return 0; fi
  if command -v rc-service >/dev/null 2>&1; then printf 'openrc'; return 0; fi

  die "could not detect a supported service manager; set LUNARWING_SERVICE_MANAGER=systemd or openrc"
}

INIT_SYSTEM=""
ensure_init_system() {
  [[ -n "$INIT_SYSTEM" ]] || INIT_SYSTEM="$(detect_init_system)"
  if [[ -z "$HEALTH_INTERVAL_MIN" ]]; then
    if [[ "$INIT_SYSTEM" == "openrc" ]]; then
      HEALTH_INTERVAL_MIN=5
    else
      HEALTH_INTERVAL_MIN=15
    fi
  fi
}

# Machine-wide persisted runtime choice (see _save_container_runtime). One
# line: "podman" or "docker". World-readable so unprivileged doctor runs can
# still resolve the saved choice.
RUNTIME_STATE_FILE="/etc/lunarwing/container-runtime"

# Print the persisted runtime choice, or nothing. Invalid/unreadable content
# warns (stderr) and prints nothing so callers fall through to auto-detect.
_load_saved_container_runtime() {
  [[ -f "$RUNTIME_STATE_FILE" ]] || return 0
  local saved=""
  saved="$(tr -d '[:space:]' <"$RUNTIME_STATE_FILE" 2>/dev/null)" || return 0
  saved="${saved,,}"
  case "$saved" in
    docker|podman) printf '%s' "$saved" ;;
    *) say "WARNING: ignoring invalid $RUNTIME_STATE_FILE: '$saved' (expected docker or podman)" >&2 ;;
  esac
  return 0
}

# Persist an explicitly-chosen runtime machine-wide. Warn-and-continue: a
# read-only /etc or non-root caller must never break the invoking command.
_save_container_runtime() {
  local rt="$1" tmp
  mkdir -p /etc/lunarwing 2>/dev/null || { say "WARNING: cannot create /etc/lunarwing; runtime choice not persisted" >&2; return 0; }
  tmp="$(mktemp "${RUNTIME_STATE_FILE}.tmp.XXXXXX" 2>/dev/null)" \
    || { say "WARNING: cannot write $RUNTIME_STATE_FILE; runtime choice not persisted" >&2; return 0; }
  if printf '%s\n' "$rt" >"$tmp" && chmod 0644 "$tmp" && mv "$tmp" "$RUNTIME_STATE_FILE"; then
    say "container runtime '$rt' saved to $RUNTIME_STATE_FILE (env var no longer needed)" >&2
  else
    rm -f "$tmp"
    say "WARNING: cannot write $RUNTIME_STATE_FILE; runtime choice not persisted" >&2
  fi
  return 0
}

# ── Container runtime detection ──────────────────────────────────────────────

detect_container_runtime() {
  local override="${LUNARWING_CONTAINER_RUNTIME:-}" saved=""
  if [[ -n "$override" ]]; then
    case "${override,,}" in
      docker|podman)
        override="${override,,}"
        # Persist the explicit choice (idempotent: skip when unchanged).
        saved="$(_load_saved_container_runtime)"
        [[ "$saved" == "$override" ]] || _save_container_runtime "$override"
        printf '%s' "$override"; return 0 ;;
      *) die "unsupported container runtime '$override'; use docker or podman" ;;
    esac
  fi

  saved="$(_load_saved_container_runtime)"
  if [[ -n "$saved" ]]; then
    printf '%s' "$saved"; return 0
  fi

  if command -v podman >/dev/null 2>&1 && ! command -v docker >/dev/null 2>&1; then
    printf 'podman'; return 0
  fi
  if command -v docker >/dev/null 2>&1; then printf 'docker'; return 0; fi
  if command -v podman >/dev/null 2>&1; then printf 'podman'; return 0; fi

  die "neither docker nor podman found; install one or set LUNARWING_CONTAINER_RUNTIME"
}

CONTAINER_RT=""
MT_ROOTLESS=""
ensure_container_runtime() {
  [[ -n "$CONTAINER_RT" ]] || CONTAINER_RT="$(detect_container_runtime)"
  if [[ -z "$MT_ROOTLESS" ]]; then
    # Rootless-per-tenant is the default for podman (no daemon; each tenant owns
    # its containers under ~/.local/share/containers). Docker keeps the legacy
    # rootful-as-root model (it has a daemon). Override via LUNARWING_MT_ROOTLESS.
    if [[ -n "${LUNARWING_MT_ROOTLESS:-}" ]]; then
      MT_ROOTLESS="${LUNARWING_MT_ROOTLESS}"
    elif [[ "$CONTAINER_RT" == "podman" ]]; then
      MT_ROOTLESS="true"
    else
      MT_ROOTLESS="false"
    fi
  fi
}

# True if the active runtime is podman new enough for the Quadlet .container
# features we emit. Floor is >= 4.6: Quadlet itself shipped in 4.4, but the
# Health* keys render_pg_quadlet uses first exist in 4.5 (Quadlet hard-errors
# and skips the whole unit on an unknown key), and 4.6 is the conservative
# stable baseline. Gates the systemd rootless container-supervision path against
# the imperative `podman run` fallback. Result is memoised in QUADLET_OK.
QUADLET_OK=""
podman_supports_quadlet() {
  ensure_container_runtime
  [[ "$CONTAINER_RT" == "podman" ]] || return 1
  if [[ -z "$QUADLET_OK" ]]; then
    local ver major minor
    ver="$(podman version --format '{{.Client.Version}}' 2>/dev/null || true)"
    [[ -n "$ver" ]] || ver="$(podman --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -n1 || true)"
    major="${ver%%.*}"
    minor="${ver#*.}"; minor="${minor%%.*}"
    if [[ "$major" =~ ^[0-9]+$ && "$minor" =~ ^[0-9]+$ ]] \
       && { [[ "$major" -gt 4 ]] || { [[ "$major" -eq 4 ]] && [[ "$minor" -ge 6 ]]; }; }; then
      QUADLET_OK="yes"
    else
      QUADLET_OK="no"
    fi
  fi
  [[ "$QUADLET_OK" == "yes" ]]
}

# Run the container runtime for a TENANT's containers. When rootless (podman),
# execute as the tenant user against their rootless store + runtime dir; when
# rootful (docker), run as root unchanged. Every per-tenant pg/worker container
# operation MUST go through this so inspect/start/stop/exec/rm hit the SAME store
# that owns the container — root and rootless podman are separate universes.
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
render_worker_openrc_unit() {
  local name="$1" worker="$2" health_port="${3:-8443}"
  ensure_container_runtime
  local runtime_bin="" container="lunarwing-${worker}-${name}" uid home
  [[ -n "${CONTAINER_RT:-}" ]] && runtime_bin="$(command -v "$CONTAINER_RT" 2>/dev/null || true)"
  uid="$(id -u "$name" 2>/dev/null || echo "")"
  home="$(tenant_home "$name")"

  cat >"/etc/init.d/${container}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing ${worker} worker ($name)"

: "\${wk_runtime:=$runtime_bin}"
: "\${wk_container:=$container}"
: "\${wk_rootless:=$MT_ROOTLESS}"
: "\${wk_user:=$name}"
: "\${wk_home:=$home}"
: "\${wk_uid:=$uid}"
: "\${wk_health_port:=$health_port}"
: "\${wk_wait:=60}"

depend() {
    need net localmount
    after firewall lunarwing-${name}
}

# Run the container runtime as the owning user (rootless) or root (rootful).
_wk() {
    if [ "\${wk_rootless}" = "true" ]; then
        sudo -u "\${wk_user}" env HOME="\${wk_home}" XDG_RUNTIME_DIR="/run/user/\${wk_uid}" "\${wk_runtime}" "\$@"
    else
        "\${wk_runtime}" "\$@"
    fi
}

# Healthy = container running AND its internal /health endpoint answers.
_wk_healthy() {
    [ "\$(_wk inspect -f '{{.State.Running}}' "\${wk_container}" 2>/dev/null)" = "true" ] || return 1
    _wk exec "\${wk_container}" curl -sf -o /dev/null --max-time 3 "http://127.0.0.1:\${wk_health_port}/health" 2>/dev/null
}

start() {
    [ -n "\${wk_runtime}" ] && [ -x "\${wk_runtime}" ] || { ewarn "no container runtime; skipping ${worker} for $name"; return 0; }
    ebegin "Starting ${worker} worker (\${wk_container})"
    if [ "\${wk_rootless}" = "true" ]; then
        checkpath -d -m 0700 -o "\${wk_user}:\${wk_user}" "/run/user/\${wk_uid}"
    fi
    _wk start "\${wk_container}" >/dev/null 2>&1 || { eend 1 "container start failed"; return 1; }
    _w=0
    while ! _wk_healthy; do
        _w=\$((_w + 1))
        [ "\$_w" -lt "\${wk_wait}" ] || { eend 1 "${worker} worker not healthy after \${wk_wait}s"; return 1; }
        sleep 1
    done
    eend 0
}

stop() {
    [ -n "\${wk_runtime}" ] && [ -x "\${wk_runtime}" ] || return 0
    ebegin "Stopping ${worker} worker (\${wk_container})"
    _wk stop --time 30 "\${wk_container}" >/dev/null 2>&1
    eend 0
}

status() {
    # Standard OpenRC started/stopped wording so health-openrc.sh classifies it.
    if _wk_healthy; then
        einfo "\${wk_container}: started"; return 0
    fi
    einfo "\${wk_container}: stopped"; return 3
}
INITEOF
  chmod 0755 "/etc/init.d/${container}"
}

# Promote a (rootless) worker container to a dedicated OpenRC unit so it is
# health-monitored, self-healed, and boot-persistent. No-op unless OpenRC.
_register_worker_unit() {
  local name="$1" worker="$2"
  ensure_init_system
  [[ "$INIT_SYSTEM" == "openrc" ]] || return 0
  render_worker_openrc_unit "$name" "$worker"
  rc-update add "lunarwing-${worker}-${name}" default >/dev/null 2>&1 || true
  rc-service "lunarwing-${worker}-${name}" start >/dev/null 2>&1 || true
  say "registered OpenRC unit lunarwing-${worker}-${name} (health-monitored, boot-persistent)"
  local container="lunarwing-${worker}-${name}"
  local uid home
  uid="$(id -u "$name" 2>/dev/null || echo "")"
  home="$(tenant_home "$name")"
  _register_babysitter "$name" "$worker" "$container" "$uid" "$home"
}

# Tear down a worker's OpenRC unit (boot-disable + remove the init script).
_deregister_worker_unit() {
  local name="$1" worker="$2"
  ensure_init_system
  [[ "$INIT_SYSTEM" == "openrc" ]] || return 0
  [[ -f "/etc/init.d/lunarwing-${worker}-${name}" ]] || return 0
  _deregister_babysitter "lunarwing-${worker}-${name}"
  rc-service "lunarwing-${worker}-${name}" stop >/dev/null 2>&1 || true
  rc-update del "lunarwing-${worker}-${name}" default >/dev/null 2>&1 || true
  rm -f "/etc/init.d/lunarwing-${worker}-${name}" "/etc/conf.d/lunarwing-${worker}-${name}"
}

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

# ── Port registry ────────────────────────────────────────────────────────────

ports_registry_init() {
  local registry_version="" registry_needs_repair="false"
  if [[ -f "$PORTS_REGISTRY" ]]; then
    registry_version="$(jq -r '.version // 0' "$PORTS_REGISTRY" 2>/dev/null || true)"
    if jq -e '.tenants | to_entries[] | select(.value.extended_ports | has("darkirc_adapter") | not)' \
        "$PORTS_REGISTRY" >/dev/null 2>&1; then
      registry_needs_repair="true"
    fi
  fi
  if [[ ! -f "$PORTS_REGISTRY" || "$registry_version" != "14" || "$registry_needs_repair" == "true" ]]; then
    acquire_ports_lock
  fi

  if [[ ! -d /etc/lunarwing ]]; then
    mkdir -p /etc/lunarwing
    chmod 0755 /etc/lunarwing
  fi

  if [[ ! -f "$PORTS_REGISTRY" ]]; then
    local tmp
    tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
    cat >"$tmp" <<'ENDJSON'
{
  "version": 3,
  "range": { "start": 10000, "end": 19999 },
  "block_size": 10,
  "tenants": {}
}
ENDJSON
    chmod 0644 "$tmp"
    mv "$tmp" "$PORTS_REGISTRY"
    say "initialized port registry: $PORTS_REGISTRY"
  fi

  ports_migrate
  # ports_migrate may have bumped to the current version; verify and release.
  release_ports_lock
}

# Inspection commands must not create or migrate host state.
ports_registry_require_readonly() {
  require_cmd jq
  [[ -f "$PORTS_REGISTRY" && ! -L "$PORTS_REGISTRY" ]] \
    || die "port registry not found: $PORTS_REGISTRY"
  jq -e '.tenants | type == "object"' "$PORTS_REGISTRY" >/dev/null 2>&1 \
    || die "invalid port registry: $PORTS_REGISTRY"
}

ports_migrate_v2() {
  say "migrating port registry v1 -> v2 (reserved_0 -> orchestrator) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 2 |
    .tenants |= with_entries(
      .value.ports |= (
        if .reserved_0 then
          .orchestrator = .reserved_0 | del(.reserved_0)
        else
          .
        end
      )
    )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v2"
}

ports_migrate_v3() {
  say "migrating port registry v2 -> v3 (reserved_1 -> nanocode_wss) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 3 |
    .tenants |= with_entries(
      .value.ports |= (
        if .reserved_1 then
          .nanocode_wss = .reserved_1 | del(.reserved_1)
        else
          . + { nanocode_wss: (.orchestrator + 1) }
        end
      )
    )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v3"
}

ports_migrate_v4() {
  say "migrating port registry v3 -> v4 (reserved_2 -> pebble_wss) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 4 |
    .tenants |= with_entries(
      .value.ports |= (
        if .reserved_2 then
          .pebble_wss = .reserved_2 | del(.reserved_2)
        else
          . + { pebble_wss: (.orchestrator + 2) }
        end
      )
    )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v4"
}

ports_migrate_v5() {
  say "migrating port registry v4 -> v5 (reserved_3 -> weechat_adapter) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 5 |
    .tenants |= with_entries(
      .value.ports |= (
        if .reserved_3 then
          .weechat_adapter = .reserved_3 | del(.reserved_3)
        else
          . + { weechat_adapter: (.orchestrator + 3) }
        end
      )
    )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v5"
}

ports_migrate_v6() {
  say "migrating port registry v${current_version} -> v6 (add extended port range for overflow services) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 6
    | .extended_range = { "start": 20000, "end": 29999 }
    | .extended_block_size = (.block_size // 10)
    | ( .range.start // 10000 ) as $rstart
    | ( .extended_range.start ) as $estart
    | ( .extended_block_size ) as $bs
    | .tenants |= with_entries(
        .value |= (
          if .base_port then
            ( .base_port - $rstart + $estart ) as $eb
            | .extended_base = $eb
            | .extended_ports = (
                reduce range(0; $bs) as $i ({}; . + { ("reserved_\($i)"): ($eb + $i) })
              )
          else . end
        )
      )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v6"
}

ports_migrate_v6_1() {
  # v6.1: assign darkirc_adapter to existing tenants from extended_ports[0].
  # Idempotent: detected by the absence of darkirc_adapter in extended_ports.
  if ! jq -e '.tenants | to_entries[] | select(.value.extended_ports | has("darkirc_adapter") | not)' "$PORTS_REGISTRY" >/dev/null 2>&1; then
    return 0
  fi

  say "migrating port registry -> v6.1 (assign darkirc_adapter from extended slot 0) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .tenants |= with_entries(
      .value |= (
        if (.extended_ports | has("darkirc_adapter") | not) and (.extended_ports | type == "object") then
          .extended_ports.darkirc_adapter = .extended_ports.reserved_0
          | if (.extended_ports.reserved_0) then .extended_ports |= del(.reserved_0) else . end
        else . end
      )
    )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v6.1 (darkirc_adapter added)"
}

ports_migrate_v7() {
  say "migrating port registry v${current_version} -> v7 (assign darkirc_irc + darkirc_rpc from extended slots) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 7
    | .tenants |= with_entries(
        .value.extended_ports |= (
          if type == "object" then
            .darkirc_irc = ((.darkirc_irc) // (.reserved_1) // (.darkirc_adapter + 1))
            | .darkirc_rpc = ((.darkirc_rpc) // (.reserved_2) // (.darkirc_adapter + 2))
            | del(.reserved_1, .reserved_2)
          else . end
        )
      )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  current_version=7
  say "port registry migrated to v7 (darkirc_irc + darkirc_rpc added)"
}

ports_migrate_v8() {
  say "migrating port registry v${current_version} -> v8 (dedicate nanocode_health + pebble_health) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 8
    | .tenants |= with_entries(
        .value |= (
          .extended_base as $eb
          | .extended_ports |= (
              if type == "object" then
                .nanocode_health = ((.nanocode_health) // (.reserved_3) // ($eb + 3))
                | .pebble_health = ((.pebble_health) // (.reserved_4) // ($eb + 4))
                | del(.reserved_3, .reserved_4)
              else . end
            )
        )
      )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  current_version=8
}

ports_migrate() {
  [[ -f "$PORTS_REGISTRY" ]] || return 0
  require_cmd jq

  local current_version
  current_version="$(jq -r '.version // 0' "$PORTS_REGISTRY")"

  if [[ "$current_version" -lt 2 ]]; then ports_migrate_v2; current_version=2; fi
  if [[ "$current_version" -lt 3 ]]; then ports_migrate_v3; current_version=3; fi
  if [[ "$current_version" -lt 4 ]]; then ports_migrate_v4; current_version=4; fi
  if [[ "$current_version" -lt 5 ]]; then ports_migrate_v5; current_version=5; fi
  if [[ "$current_version" -lt 6 ]]; then ports_migrate_v6; current_version=6; fi
  ports_migrate_v6_1
  if [[ "$current_version" -lt 7 ]]; then ports_migrate_v7; fi
  if [[ "$current_version" -lt 8 ]]; then ports_migrate_v8; fi
  if [[ "$current_version" -lt 9 ]]; then ports_migrate_v9; fi
  if [[ "$current_version" -lt 10 ]]; then ports_migrate_v10; fi
  if [[ "$current_version" -lt 11 ]]; then ports_migrate_v11; fi
  if [[ "$current_version" -lt 12 ]]; then ports_migrate_v12; fi
  if [[ "$current_version" -lt 13 ]]; then ports_migrate_v13; fi
  if [[ "$current_version" -lt 14 ]]; then ports_migrate_v14; fi
}

# v8 -> v9: rename extended_ports.reserved_5 -> vision_service.
ports_migrate_v9() {
  if jq -e '.tenants | to_entries[] | select(.value.extended_ports | has("vision_service") | not) | select(.value.extended_ports | has("reserved_5"))' "$PORTS_REGISTRY" >/dev/null 2>&1; then
    say "migrating port registry -> v9 (assign vision_service from reserved_5 slot) ..."
    local tmp
    tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
    jq '
      .tenants |= with_entries(
          .value |= (
            if (.extended_ports | type == "object") then
              .extended_ports |= (
                .vision_service = ((.vision_service) // (.reserved_5) // ((.extended_base // 0) + 5))
                | del(.reserved_5)
              )
            else . end
          )
        )
    ' "$PORTS_REGISTRY" >"$tmp"
    chmod 0644 "$tmp"
    mv "$tmp" "$PORTS_REGISTRY"
    say "port registry migrated to v9 (vision_service dedicated at extended_base+5)"
  fi

  # Always bump the version when the dispatcher calls v9, even if there were
  # no tenants to migrate (e.g. empty registry) — otherwise .version stays
  # stale and ports_allocate writes v9-shaped tenants into a v8-labeled file.
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '.version = 9' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
}

# v9 -> v10: dedicate extended_ports.reserved_6 -> vision_health.
# The vision sidecar now listens on two internal ports: 8088 (OCR_PORT) for
# API traffic and 8089 (OCR_HEALTH_PORT) for /health only. This dedicates a
# host-side port (extended_base+6) mapped to 8089 so the host self-heal
# pipeline can probe /health independently of OCR traffic — mirroring the
# nanocode_health / pebble_health pattern (v8).
ports_migrate_v10() {
  if jq -e '.tenants | to_entries[] | select(.value.extended_ports | has("vision_health") | not) | select(.value.extended_ports | has("reserved_6"))' "$PORTS_REGISTRY" >/dev/null 2>&1; then
    say "migrating port registry -> v10 (assign vision_health from reserved_6 slot) ..."
    local tmp
    tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
    jq '
      .tenants |= with_entries(
          .value |= (
            if (.extended_ports | type == "object") then
              .extended_ports |= (
                .vision_health = ((.vision_health) // (.reserved_6) // ((.extended_base // 0) + 6))
                | del(.reserved_6)
              )
            else . end
          )
        )
    ' "$PORTS_REGISTRY" >"$tmp"
    chmod 0644 "$tmp"
    mv "$tmp" "$PORTS_REGISTRY"
    say "port registry migrated to v10 (vision_health dedicated at extended_base+6)"
  fi

  # Always bump the version when the dispatcher calls v10, even if there were
  # no tenants to migrate (e.g. empty registry) — otherwise .version stays
  # stale and ports_allocate writes v10-shaped tenants into a v9-labeled file.
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '.version = 10' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
}

# v10 -> v11: dedicate extended_ports.reserved_7 -> opencode_wss,
# extended_ports.reserved_8 -> opencode_health.
ports_migrate_v11() {
  if jq -e '.tenants | to_entries[] | select(.value.extended_ports | has("opencode_wss") | not) | select(.value.extended_ports | has("reserved_7"))' "$PORTS_REGISTRY" >/dev/null 2>&1; then
    say "migrating port registry -> v11 (assign opencode_wss from reserved_7 slot) ..."
    local tmp
    tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
    jq '
      .tenants |= with_entries(
          .value |= (
            if (.extended_ports | type == "object") then
              .extended_ports |= (
                .opencode_wss = ((.opencode_wss) // (.reserved_7) // ((.extended_base // 0) + 7))
                | .opencode_health = ((.opencode_health) // (.reserved_8) // ((.extended_base // 0) + 8))
                | del(.reserved_7, .reserved_8)
              )
            else . end
          )
        )
    ' "$PORTS_REGISTRY" >"$tmp"
    chmod 0644 "$tmp"
    mv "$tmp" "$PORTS_REGISTRY"
    say "port registry migrated to v11 (opencode_wss + opencode_health dedicated at extended_base+7/+8)"
  fi

  # Always bump the version when the dispatcher calls v11, even if there were
  # no tenants to migrate (e.g. empty registry) — otherwise .version stays
  # stale and ports_allocate writes v11-shaped tenants into a v10-labeled file.
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '.version = 11' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
}

# v11 -> v12: add a dedicated kimi port range (30000-39999, block_size 2)
# with per-tenant kimi_ports {kimi_wss, kimi_health}. The existing extended
# block (20000-29999, block_size 10) has only one free slot (reserved_9), which
# is insufficient for two ports. Using a separate range avoids re-spacing
# existing tenants' extended blocks (which would require re-render + restart
# of every running tenant) and is fully non-disruptive.
ports_migrate_v12() {
  if ! jq -e '.tenants | to_entries[] | select(.value.kimi_ports | has("kimi_wss") | not)' "$PORTS_REGISTRY" >/dev/null 2>&1; then
    # No tenants need kimi_ports; just bump the version.
    local tmp
    tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
    jq '.version = 12
      | .kimi_range = ((.kimi_range) // {"start": 30000, "end": 39999})
      | .kimi_block_size = ((.kimi_block_size) // 2)' "$PORTS_REGISTRY" >"$tmp"
    chmod 0644 "$tmp"
    mv "$tmp" "$PORTS_REGISTRY"
    return 0
  fi

  say "migrating port registry -> v12 (add kimi_wss + kimi_health from dedicated range) ..."
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 12
    | .kimi_range = ((.kimi_range) // {"start": 30000, "end": 39999})
    | .kimi_block_size = ((.kimi_block_size) // 2)
    | ( .range.start // 10000 ) as $rstart
    | ( .kimi_range.start ) as $kstart
    | .tenants |= with_entries(
        .value |= (
          if .base_port then
            ( .base_port - $rstart + $kstart ) as $kbase
            | .kimi_base = $kbase
            | .kimi_ports = {
                kimi_wss: ($kbase + 0),
                kimi_health: ($kbase + 1)
              }
          else . end
        )
      )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v12 (kimi_wss + kimi_health dedicated in kimi_range 30000-39999)"
}

# v12 -> v13: dedicate the final extended slot to the per-tenant coordination
# admin server. Existing units are unaffected until coordination is enabled.
ports_migrate_v13() {
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '
    .version = 13
    | .tenants |= with_entries(
        .value |= (
          if (.extended_base | type) == "number" and ((.extended_ports | type) != "object" or (.extended_ports | has("coord_admin") | not)) then
            .extended_ports = (if (.extended_ports | type) == "object" then .extended_ports else {} end)
            | .extended_ports.coord_admin = (.extended_ports.reserved_9 // (.extended_base + 9))
            | .extended_ports |= del(.reserved_9)
          else . end
        )
      )
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v13 (coord_admin dedicated at extended_base+9)"
}

# v13 -> v14: stable non-secret identity for every DarkIRC-enabled tenant.
ports_migrate_v14() {
  local name tmp next scope
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  cp "$PORTS_REGISTRY" "$tmp"
  if ! jq -e '
      [(.tenants // {}) | to_entries[]
        | select(.value.enable_darkirc == true)
        | (.value.darkirc_scope_id // "")
        | select(length > 0)] as $ids
      | all($ids[]; test("^[0-9a-f]{32}$") and . != "00000000000000000000000000000000")
      and (($ids | unique | length) == ($ids | length))
    ' "$tmp" >/dev/null; then
    rm -f "$tmp"
    die "failed to migrate port registry to v14: invalid or duplicate scope ID"
  fi
  while IFS= read -r name; do
    [[ -n "$name" ]] || continue
    scope="$(jq -r --arg name "$name" '.tenants[$name].darkirc_scope_id // empty' "$tmp")"
    [[ -z "$scope" ]] || continue
    while :; do
      scope="$(generate_darkirc_scope_id)"
      if ! jq -e --arg scope "$scope" \
        '.tenants // {} | to_entries | any(.value.darkirc_scope_id == $scope)' \
        "$tmp" >/dev/null; then
        break
      fi
    done
    next="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
    jq --arg name "$name" --arg scope "$scope" \
      '.tenants[$name].darkirc_scope_id = $scope' "$tmp" >"$next" \
      || { rm -f "$tmp" "$next"; die "failed to assign DarkIRC scope ID"; }
    mv "$next" "$tmp"
  done < <(jq -r '.tenants // {} | to_entries[] | select(.value.enable_darkirc == true) | .key' "$tmp")
  next="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq '.version = 14' "$tmp" >"$next" \
    || { rm -f "$tmp" "$next"; die "failed to migrate port registry to v14"; }
  mv "$next" "$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  say "port registry migrated to v14 (stable DarkIRC scope IDs)"
}

ports_allocate() {
  local name="$1"
  local enable_darkirc="${2:-false}"
  local enable_proxy="${3:-false}"
  # Selected external workers, as a JSON object literal. Persisted so
  # start_tenant_<worker> can gate on the tenant's choice; see
  # tenant_worker_enabled. NOTE: assign in two steps — `${4:-{}}` mis-parses
  # (bash closes the ${...} at the first `}`, appending a stray `}` and
  # corrupting the JSON), so default explicitly instead.
  local workers_json="${4:-}"
  local requested_darkirc_scope="${5:-}"
  [[ -n "$workers_json" ]] || workers_json='{}'
  require_cmd jq
  acquire_ports_lock
  if [[ -n "$requested_darkirc_scope" ]]; then
    [[ "$enable_darkirc" == "true" ]] \
      || die "DarkIRC import scope requires --enable-darkirc"
    validate_darkirc_scope_id "$requested_darkirc_scope"
    jq -e --arg scope "$requested_darkirc_scope" --arg name "$name" \
      '.tenants // {} | to_entries | any(.key != $name and .value.darkirc_scope_id == $scope)' \
      "$PORTS_REGISTRY" >/dev/null 2>&1 \
      && die "DarkIRC import scope is already assigned"
  fi

  # Resumable (F4): if this tenant already has a block, reuse it (echo its
  # base_port) instead of dying — so re-running add-tenant after a mid-flow failure
  # resumes cleanly (clone/env/pg/render/pipeline are idempotent, and
  # tenant_pg_password is stable). Use `remove-tenant` to truly start over.
  local existing
  existing="$(jq -r ".tenants[\"$name\"].base_port // empty" "$PORTS_REGISTRY" 2>/dev/null || true)"
  if [[ -n "$existing" ]]; then
    if [[ -n "$requested_darkirc_scope" ]]; then
      local existing_scope
      existing_scope="$(jq -r --arg name "$name" '.tenants[$name].darkirc_scope_id // empty' "$PORTS_REGISTRY")"
      [[ "$existing_scope" == "$requested_darkirc_scope" ]] \
        || die "existing tenant has a different DarkIRC scope"
    fi
    say "tenant '$name' already has ports allocated (base $existing); reusing for resume" >&2
    # Reconcile the darkirc flag on resume (H2): add_tenant()'s env/unit writers
    # (write_tenant_lunarwing_env, render_tenant_*_units, start_tenant_*) gate on
    # tenant_darkirc_enabled (a registry read), but the adapter env/TOML writers
    # at add_tenant() use the in-memory flag. Without this flip, re-running
    # `add-tenant <existing> --enable-darkirc` leaves the tenant half-configured.
    # One-directional (false -> true): disabling darkirc post-provision is a
    # manual teardown (see docs/ops/DARKIRC-MULTITENANT.md).
    [[ "$enable_darkirc" == "true" ]] && ports_enable_darkirc "$name"
    [[ "$enable_proxy" == "true" ]] && ports_enable_proxy "$name"
    # Reconcile worker selection on resume, same one-directional false->true rule
    # as darkirc/proxy: re-running `add-tenant <existing> --with-<worker>` turns a
    # worker on; omitting the flag never turns one off (disabling is manual).
    local _w
    for _w in pebble opencode kimi; do
      if [[ "$(jq -r --arg w "$_w" '.[$w] // false' <<<"$workers_json" 2>/dev/null)" == "true" ]]; then
        ports_enable_worker "$name" "$_w"
      fi
    done
    printf '%s' "$existing"
    release_ports_lock
    return 0
  fi

  local base=-1
  local p
  for ((p = PORT_RANGE_START; p <= PORT_RANGE_END - PORT_BLOCK_SIZE + 1; p += PORT_BLOCK_SIZE)); do
    if ! jq -e ".tenants | to_entries[] | select(.value.base_port == $p)" "$PORTS_REGISTRY" >/dev/null 2>&1; then
      base=$p
      break
    fi
  done

  [[ $base -ge 0 ]] || die "no free port blocks in range $PORT_RANGE_START-$PORT_RANGE_END"

  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  local darkirc_json="false"
  [[ "$enable_darkirc" == "true" ]] && darkirc_json="true"
  local proxy_json="false"
  [[ "$enable_proxy" == "true" ]] && proxy_json="true"
  # Keep the retired nanocode field false so existing registry consumers retain
  # a stable shape and no historical port or worker slot gets reused.
  local workers_norm
  workers_norm="$(jq -c '{
      nanocode: false,
      pebble:   (.pebble   // false),
      opencode: (.opencode // false),
      kimi:     (.kimi     // false)
    }' <<<"$workers_json" 2>/dev/null || echo '{"nanocode":false,"pebble":false,"opencode":false,"kimi":false}')"
  local darkirc_scope=""
  if [[ "$enable_darkirc" == true ]]; then
    darkirc_scope="${requested_darkirc_scope:-$(generate_unique_darkirc_scope_id)}"
  fi
  jq --arg name "$name" --argjson base "$base" --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --argjson darkirc "$darkirc_json" --arg scope "$darkirc_scope" --argjson proxy "$proxy_json" --argjson workers "$workers_norm" '
    ( .range.start // 10000 ) as $rstart
    | ( .extended_range.start // 20000 ) as $estart
    | ( .extended_block_size // 10 ) as $ebs
    | ( .kimi_range.start // 30000 ) as $kstart
    | ( $base - $rstart + $estart ) as $ebase
    | ( $base - $rstart + $kstart ) as $kbase
    | .tenants[$name] = {
        base_port: $base,
        user: $name,
        created_at: $ts,
        enable_darkirc: $darkirc,
        darkirc_scope_id: (if $darkirc then $scope else null end),
        enable_proxy: $proxy,
        enable_weechat: true,
        workers: $workers,
        ports: {
          gateway:          ($base + 0),
          http:             ($base + 1),
          bridge:           ($base + 2),
          postgres:         ($base + 3),
          proxy:            ($base + 4),
          weechat:          ($base + 5),
          orchestrator:     ($base + 6),
          nanocode_wss:     ($base + 7),
          pebble_wss:       ($base + 8),
          weechat_adapter:  ($base + 9)
        },
        extended_base: $ebase,
        extended_ports: (
          {
            darkirc_adapter: $ebase,
            darkirc_irc: ($ebase + 1),
            darkirc_rpc: ($ebase + 2),
            nanocode_health: ($ebase + 3),
            pebble_health: ($ebase + 4),
            vision_service: ($ebase + 5),
            vision_health: ($ebase + 6),
            opencode_wss: ($ebase + 7),
            opencode_health: ($ebase + 8),
            coord_admin: ($ebase + 9)
          }
          + (reduce range(10; $ebs) as $i ({}; . + { ("reserved_\($i)"): ($ebase + $i) }))
        ),
        kimi_base: $kbase,
        kimi_ports: {
          kimi_wss: ($kbase + 0),
          kimi_health: ($kbase + 1)
        }
      }
  ' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"

  say "allocated port block $base-$((base + PORT_BLOCK_SIZE - 1)) for tenant '$name'" >&2
  printf '%s' "$base"
  release_ports_lock
}

# Mark darkirc enabled for a tenant in the registry (one-directional: false -> true).
# Idempotent: no-op if already enabled. Used by ports_allocate()'s resume path so
# `add-tenant <existing> --enable-darkirc` flips the registry (H2). Does NOT tear
# down darkirc (disabling is a manual operation; see DARKIRC-MULTITENANT.md).
ports_enable_darkirc() {
  local name="$1"
  require_cmd jq
  # ISSUE-012 (follow-up): registry read-modify-rename must run under the shared
  # ports lock, same as ports_set_weechat. Reentrancy-safe: when called from
  # ports_allocate() (which already holds the lock) acquire is a no-op and we
  # must NOT release the caller's lock — only release if we acquired it here.
  local had_lock="$PORTS_LOCK_HELD"
  acquire_ports_lock
  if ! jq -e ".tenants[\"$name\"]" "$PORTS_REGISTRY" >/dev/null 2>&1; then
    [[ "$had_lock" == "true" ]] || release_ports_lock
    die "tenant '$name' not in port registry"
  fi
  local current
  current="$(jq -r ".tenants[\"$name\"].enable_darkirc // false" "$PORTS_REGISTRY" 2>/dev/null || true)"
  if [[ "$current" == "true" ]]; then
    ensure_darkirc_scope_id "$name" >/dev/null
    [[ "$had_lock" == "true" ]] || release_ports_lock
    return 0
  fi

  local tmp scope_id
  scope_id="$(generate_unique_darkirc_scope_id)"
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  if ! jq --arg name "$name" --arg scope "$scope_id" \
      '.tenants[$name].enable_darkirc = true | .tenants[$name].darkirc_scope_id = $scope' \
      "$PORTS_REGISTRY" >"$tmp" 2>/dev/null; then
    rm -f "$tmp"
    [[ "$had_lock" == "true" ]] || release_ports_lock
    die "failed to enable darkirc flag for tenant '$name'"
  fi
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  [[ "$had_lock" == "true" ]] || release_ports_lock
  say "darkirc for tenant '$name': disabled -> enabled" >&2
}

# Mark proxy enabled for a tenant in the registry (one-directional: false -> true).
# Idempotent. Used by ports_allocate()'s resume path and by the --enable-proxy flag.
ports_enable_proxy() {
  local name="$1"
  require_cmd jq
  # ISSUE-012 (follow-up): same locked read-modify-rename pattern as
  # ports_enable_darkirc — see comment there for the reentrancy contract.
  local had_lock="$PORTS_LOCK_HELD"
  acquire_ports_lock
  if ! jq -e ".tenants[\"$name\"]" "$PORTS_REGISTRY" >/dev/null 2>&1; then
    [[ "$had_lock" == "true" ]] || release_ports_lock
    die "tenant '$name' not in port registry"
  fi
  local current
  current="$(jq -r ".tenants[\"$name\"].enable_proxy // false" "$PORTS_REGISTRY" 2>/dev/null || true)"
  if [[ "$current" == "true" ]]; then
    [[ "$had_lock" == "true" ]] || release_ports_lock
    return 0
  fi

  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  if ! jq --arg name "$name" '.tenants[$name].enable_proxy = true' \
      "$PORTS_REGISTRY" >"$tmp" 2>/dev/null; then
    rm -f "$tmp"
    [[ "$had_lock" == "true" ]] || release_ports_lock
    die "failed to enable proxy flag for tenant '$name'"
  fi
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  [[ "$had_lock" == "true" ]] || release_ports_lock
  say "proxy for tenant '$name': disabled -> enabled" >&2
}

# Set the per-tenant WeeChat health-gate flag in the registry (bidirectional).
# enable_weechat=false tells the infrastructure health checks (health-openrc.sh /
# health-systemd.sh) to classify the tenant's OPTIONAL lunarwing-weechat-<t> and
# lunarwing-weechat-adapter-<t> units as `skipped` when down, so an intentionally
# unused WeeChat backend no longer reads critical and self-heal does not
# restart-storm (flap) a unit that cannot come up. Absent flag == enabled, so
# tenants created before this field existed keep the old behavior.
# The flag is authoritative for lifecycle too (ISSUE-006): the
# disable-weechat / enable-weechat verbs stop/disable resp. enable/start the
# units after flipping it, and start-tenant respects it on both init systems.
# This function only mutates the registry (under the shared ports lock).
# ISSUE-012: runs under the shared ports lock like every other registry
# mutation, and revalidates tenant existence while holding it — two unlocked
# read-modify-rename writers could drop each other's updates, and a concurrent
# remove-tenant could be followed by recreation of a partial tenant object.
ports_set_weechat() {
  local name="$1" enabled="$2"
  require_cmd jq
  case "$enabled" in true|false) ;; *) die "ports_set_weechat: enabled must be true or false" ;; esac
  acquire_ports_lock
  local current
  if ! jq -e ".tenants[\"$name\"]" "$PORTS_REGISTRY" >/dev/null 2>&1; then
    release_ports_lock
    die "tenant '$name' not in port registry"
  fi
  # Explicit ==false test: jq's `//` treats false as empty, so
  # `.enable_weechat // true` would misread a disabled tenant as enabled and
  # `enable-weechat` could never flip it back.
  current="$(jq -r --arg name "$name" 'if .tenants[$name].enable_weechat == false then "false" else "true" end' "$PORTS_REGISTRY" 2>/dev/null || true)"
  if [[ "$current" == "$enabled" ]]; then
    say "weechat for tenant '$name': already $enabled" >&2
    release_ports_lock
    return 0
  fi
  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  if ! jq --arg name "$name" --argjson enabled "$enabled" '.tenants[$name].enable_weechat = $enabled' \
      "$PORTS_REGISTRY" >"$tmp" 2>/dev/null; then
    rm -f "$tmp"
    release_ports_lock
    die "failed to set weechat flag for tenant '$name'"
  fi
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  release_ports_lock
  say "weechat for tenant '$name': $current -> $enabled" >&2
}

# Mark a supported external worker (pebble|opencode|kimi) enabled for a tenant
# (one-directional: false -> true). Idempotent. Used by ports_allocate()'s resume
# path so `add-tenant <existing> --with-<worker>` flips the registry, mirroring
# ports_enable_darkirc/proxy. Ensures a .workers object exists on older tenant
# entries that predate the workers map.
ports_enable_worker() {
  local name="$1" worker="$2"
  require_cmd jq
    case "$worker" in
    pebble|opencode|kimi) ;;
    *) die "ports_enable_worker: unknown worker '$worker'" ;;
  esac
  # ISSUE-012 (follow-up): same locked read-modify-rename pattern as
  # ports_enable_darkirc — see comment there for the reentrancy contract.
  local had_lock="$PORTS_LOCK_HELD"
  acquire_ports_lock
  if ! jq -e ".tenants[\"$name\"]" "$PORTS_REGISTRY" >/dev/null 2>&1; then
    [[ "$had_lock" == "true" ]] || release_ports_lock
    die "tenant '$name' not in port registry"
  fi
  local current
  current="$(jq -r ".tenants[\"$name\"].workers[\"$worker\"] // false" "$PORTS_REGISTRY" 2>/dev/null || true)"
  if [[ "$current" == "true" ]]; then
    [[ "$had_lock" == "true" ]] || release_ports_lock
    return 0
  fi

  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  if ! jq --arg name "$name" --arg w "$worker" '
      .tenants[$name].workers = ((.tenants[$name].workers // {}) + { ($w): true })
    ' "$PORTS_REGISTRY" >"$tmp" 2>/dev/null; then
    rm -f "$tmp"
    [[ "$had_lock" == "true" ]] || release_ports_lock
    die "failed to enable worker '$worker' for tenant '$name'"
  fi
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  [[ "$had_lock" == "true" ]] || release_ports_lock
  say "worker '$worker' for tenant '$name': disabled -> enabled" >&2
}

# Preserve the legacy registry field while making retirement explicit. This runs
# only after supervision and containers are confirmed stopped.
ports_retire_nanocode() {
  local name="$1" tmp
  require_cmd jq
  acquire_ports_lock
  if ! jq -e --arg name "$name" '.tenants[$name]' "$PORTS_REGISTRY" >/dev/null 2>&1; then
    release_ports_lock
    return 0
  fi
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  if ! jq --arg name "$name" '
      .tenants[$name].workers = ((.tenants[$name].workers // {}) + {nanocode: false})
    ' "$PORTS_REGISTRY" >"$tmp"; then
    rm -f "$tmp"
    release_ports_lock
    return 1
  fi
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"
  release_ports_lock
}

ports_deallocate() {
  local name="$1"
  require_cmd jq
  acquire_ports_lock

  if ! jq -e ".tenants[\"$name\"]" "$PORTS_REGISTRY" >/dev/null 2>&1; then
    say "tenant '$name' not in port registry (already removed?)"
    release_ports_lock
    return 0
  fi

  local tmp
  tmp="$(mktemp "$PORTS_REGISTRY.tmp.XXXXXX")"
  jq --arg name "$name" 'del(.tenants[$name])' "$PORTS_REGISTRY" >"$tmp"
  chmod 0644 "$tmp"
  mv "$tmp" "$PORTS_REGISTRY"

  say "deallocated ports for tenant '$name'"
  release_ports_lock
}

ports_get() {
  local name="$1" port_name="$2"
  require_cmd jq
  local port
  port="$(jq -r ".tenants[\"$name\"].ports.$port_name // empty" "$PORTS_REGISTRY")"
  if [[ -n "$port" ]]; then
    printf '%s' "$port"
    return 0
  fi
  port="$(jq -r ".tenants[\"$name\"].extended_ports.$port_name // empty" "$PORTS_REGISTRY")"
  if [[ -n "$port" ]]; then
    printf '%s' "$port"
    return 0
  fi
  port="$(jq -r ".tenants[\"$name\"].kimi_ports.$port_name // empty" "$PORTS_REGISTRY")"
  if [[ -n "$port" ]]; then
    printf '%s' "$port"
    return 0
  fi
  # Back-compat: pre-v9 registries had `reserved_5` where `vision_service` now
  # lives. Read either name; new tenants only ever carry `vision_service`.
  if [[ "$port_name" == "vision_service" ]]; then
    port="$(jq -r ".tenants[\"$name\"].extended_ports.reserved_5 // empty" "$PORTS_REGISTRY")"
    [[ -n "$port" ]] && { printf '%s' "$port"; return 0; }
  fi
  # Back-compat: pre-v11 registries had `reserved_7`/`reserved_8` where
  # `opencode_wss`/`opencode_health` now live (renamed by ports_migrate_v11).
  # Read either name so an unmigrated tenant's opencode worker still resolves
  # its port instead of being skipped — mirrors the vision_service case above.
  if [[ "$port_name" == "opencode_wss" ]]; then
    port="$(jq -r ".tenants[\"$name\"].extended_ports.reserved_7 // empty" "$PORTS_REGISTRY")"
    [[ -n "$port" ]] && { printf '%s' "$port"; return 0; }
  fi
  if [[ "$port_name" == "opencode_health" ]]; then
    port="$(jq -r ".tenants[\"$name\"].extended_ports.reserved_8 // empty" "$PORTS_REGISTRY")"
    [[ -n "$port" ]] && { printf '%s' "$port"; return 0; }
  fi
  if [[ "$port_name" == "coord_admin" ]]; then
    port="$(jq -r ".tenants[\"$name\"].extended_ports.reserved_9 // empty" "$PORTS_REGISTRY")"
    [[ -n "$port" ]] && { printf '%s' "$port"; return 0; }
    port="$(jq -r ".tenants[\"$name\"].extended_base // empty" "$PORTS_REGISTRY")"
    [[ "$port" =~ ^[0-9]+$ ]] && { printf '%s' "$((port + 9))"; return 0; }
  fi
  return 1
}

ports_list() {
  require_cmd jq
  if [[ ! -f "$PORTS_REGISTRY" ]]; then
    say "no port registry found; run add-tenant first"
    return 0
  fi
  jq -r '.tenants | to_entries[] | "\(.key)\t\(.value.ports.gateway)\t\(.value.ports.http)\t\(.value.ports.bridge)\t\(.value.ports.postgres)\t\(.value.ports.proxy)\t\(.value.ports.weechat)\t\(.value.ports.weechat_adapter // "-")\t\(.value.extended_ports.darkirc_adapter // "-")\t\(.value.extended_ports.darkirc_irc // "-")\t\(.value.extended_ports.darkirc_rpc // "-")\t\(.value.ports.orchestrator)\t\(.value.ports.nanocode_wss // "-")\t\(.value.ports.pebble_wss // "-")\t\(.value.extended_ports.vision_service // "-")\t\(.value.extended_ports.vision_health // "-")"' "$PORTS_REGISTRY" \
    | column -t -N "TENANT,GATEWAY,HTTP,BRIDGE,PG,PROXY,WEECHAT,WS_ADPT,DARKIRC_ADPT,DARKIRC_IRC,DARKIRC_RPC,ORCH,NC_RSVD,PEBBLE,VISION_SVC,VISION_HLTH"
}

tenant_exists_in_registry() {
  local name="$1"
  require_cmd jq
  jq -e ".tenants[\"$name\"]" "$PORTS_REGISTRY" >/dev/null 2>&1
}

all_tenant_names() {
  require_cmd jq
  jq -r '.tenants | keys[]' "$PORTS_REGISTRY" 2>/dev/null
}

# ── User management ──────────────────────────────────────────────────────────

# Provision rootless-podman prerequisites for a tenant user. Idempotent: skips
# anything already present, never overlaps existing subordinate-id ranges.
ensure_rootless_prereqs() {
  local name="$1" uid home start
  ensure_container_runtime
  uid="$(id -u "$name")" || die "cannot resolve uid for tenant '$name'"
  home="$(getent passwd "$name" | cut -d: -f6)"

  # Subordinate uid/gid ranges for the user namespace. useradd may pre-allocate
  # these (via /etc/login.defs); only add when absent, and append AFTER the
  # current max so a new tenant never overlaps an existing range (or eris).
  if ! grep -q "^${name}:" /etc/subuid 2>/dev/null; then
    start="$(awk -F: 'BEGIN{m=100000}{e=$2+$3; if(e>m)m=e}END{print m}' /etc/subuid 2>/dev/null)"
    usermod --add-subuids "${start}-$((start + 65535))" "$name" \
      || die "failed to allocate subuid range for $name (shadow with subid support required)"
    say "allocated subuid range ${start}-$((start + 65535)) for $name"
  fi
  if ! grep -q "^${name}:" /etc/subgid 2>/dev/null; then
    start="$(awk -F: 'BEGIN{m=100000}{e=$2+$3; if(e>m)m=e}END{print m}' /etc/subgid 2>/dev/null)"
    usermod --add-subgids "${start}-$((start + 65535))" "$name" \
      || die "failed to allocate subgid range for $name"
    say "allocated subgid range ${start}-$((start + 65535)) for $name"
  fi

  # Runtime dir (XDG_RUNTIME_DIR). linger (enabled in create_tenant_user)
  # recreates it at boot; create it now for immediate use. tmpfs, 0700, owned.
  install -d -m 0700 -o "$name" -g "$name" "/run/user/$uid"

  # A freshly (re)created tenant may REUSE a uid whose previous holder left
  # rootless-podman runtime state behind in /run/user/$uid: that dir is created by
  # `install -d` above (not by a login session), so logind/elogind never reaps it
  # when the prior tenant is removed (see remove_tenant_user). A stale libpod
  # pause.pid then makes EVERY podman call — including the `system migrate` just
  # below, and the first pg container start in add-tenant — fail with
  # "cannot re-exec process to join the existing user namespace". Clear it so podman
  # spawns a fresh pause process — but KEEP it when it points to a live process
  # actually OWNED BY THIS tenant (its own running pause process on the "user
  # already exists" resume path, where ensure_rootless_prereqs re-runs from
  # create_tenant_user against a live tenant). Everything else is stale: an
  # empty/corrupt file, a dead pid, OR a pid since REUSED by another user's process
  # (host-ns owner != tenant uid) — a bare `kill -0` (we run as root) would read
  # that reused pid as "alive" and wrongly keep the stale file, leaving the O1 fault
  # unfixed. The tenant's pause process (catatonit) runs as the tenant uid in the
  # host ns, so /proc/<pid> ownership distinguishes it reliably.
  local pause_pid pause_owner
  pause_pid="/run/user/$uid/libpod/tmp/pause.pid"
  if [[ -f "$pause_pid" ]]; then
    pause_owner="$(cat "$pause_pid" 2>/dev/null || true)"
    if [[ -z "$pause_owner" ]] \
       || ! kill -0 "$pause_owner" 2>/dev/null \
       || [[ "$(stat -c %u "/proc/$pause_owner" 2>/dev/null || echo -1)" != "$uid" ]]; then
      rm -f "$pause_pid" 2>/dev/null || true
    fi
  fi

  # One-time rootless storage init (safe to re-run after subid changes).
  sudo -u "$name" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" \
    "$CONTAINER_RT" system migrate >/dev/null 2>&1 || true
  say "rootless prerequisites ready for $name (subuid/subgid, /run/user/$uid, storage)"
}

# Idempotent: install/verify the tenant user's WASM build toolchain (rustup,
# stable default, and the wasm32-wasip2 target).
# Called from create_tenant_user (add-tenant) and as a build-tenant --with-wasm
# preflight, so a partial install (network hiccup, killed add-tenant) is
# repaired instead of silently disabling WASM builds forever.
# Returns 0 when the toolchain is usable, 1 (after a loud warning) when not.
ensure_tenant_wasm_toolchain() {
  local name="$1"
  local cargo_src='if [ -f "$HOME/.cargo/env" ]; then . "$HOME/.cargo/env"; else export PATH="$HOME/.cargo/bin:$PATH"; fi;'

  # Run rustup inside the tenant repo when it exists so rust-toolchain.toml
  # selects the pinned build toolchain: the wasm target must be added to (and
  # verified on) THAT toolchain, not the tenant's default, or the pinned build in
  # build-wasm-extensions.sh still can't find it.
  local repo repo_cd=""
  repo="$(tenant_repo "$name" 2>/dev/null || true)"
  [[ -n "$repo" && -d "$repo" ]] && repo_cd="cd '$repo' && "

  # Install rustup for tenant user if not already present
  if ! sudo -u "$name" bash -c "${cargo_src} command -v rustup" &>/dev/null; then
    say "installing rustup for $name ..."
    sudo -u "$name" bash -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y' \
      || { say "WARNING: rustup installation failed for $name" >&2; }
  fi

  # Ensure a default toolchain is set (rustup install may leave none configured)
  if ! sudo -u "$name" bash -c "${cargo_src} rustup show active-toolchain" &>/dev/null; then
    say "setting default toolchain to stable for $name ..."
    sudo -u "$name" bash -c "${cargo_src} rustup default stable" \
      || say "WARNING: failed to set default toolchain for $name" >&2
  fi

  # First-party extensions use wit-bindgen and compile directly to Preview 2.
  say "ensuring WASM toolchain for $name ..."
  sudo -u "$name" bash -c "${cargo_src} ${repo_cd}rustup target add wasm32-wasip2 2>&1" || true

  # Final verification — loud, actionable, never fatal.
  local missing=()
  sudo -u "$name" bash -c "${cargo_src} ${repo_cd}rustup target list --installed 2>/dev/null | grep -q wasm32-wasip2" \
    || missing+=("wasm32-wasip2 target")
  if ((${#missing[@]} > 0)); then
    say "" >&2
    say "WARNING: WASM toolchain incomplete for $name — missing: ${missing[*]}" >&2
    say "         WASM extensions will NOT build. To fix, run:" >&2
    say "           sudo -u $name bash -lc 'rustup target add wasm32-wasip2'" >&2
    say "         then re-run: $0 build-tenant $name --with-wasm" >&2
    say "" >&2
    return 1
  fi
  return 0
}

create_tenant_user() {
  local name="$1"
  local add_docker_group="${2:-false}"

  if id "$name" &>/dev/null; then
    say "user '$name' already exists"
  else
    useradd --create-home --shell /bin/bash --comment "LunarWing tenant $name" "$name"
    say "created user: $name"
  fi

  ensure_init_system

  # Rootless-podman prerequisites for the tenant (subuid/subgid, runtime dir,
  # storage). No-op when rootful (docker).
  ensure_container_runtime
  if [[ "$MT_ROOTLESS" == "true" ]]; then
    ensure_rootless_prereqs "$name"
  fi

  # Persist a per-user runtime manager so /run/user/<uid> survives reboot. Works
  # on both systemd-logind and elogind (OpenRC) — capability-gated, not
  # systemd-only, so rootless podman keeps a runtime dir across reboots.
  if command -v loginctl >/dev/null 2>&1; then
    if loginctl enable-linger "$name" 2>/dev/null; then
      say "enabled linger for $name"
    fi
  fi

  if [[ "$add_docker_group" == "true" ]]; then
    ensure_container_runtime
    local group_name="$CONTAINER_RT"
    if getent group "$group_name" >/dev/null 2>&1; then
      usermod -aG "$group_name" "$name"
      say "added $name to $group_name group"
    else
      say "WARNING: group '$group_name' does not exist; skipping"
    fi
  fi

  local lw_root
  lw_root="$(tenant_lw_root "$name")"
  sudo -u "$name" mkdir -p \
    "$lw_root/env" \
    "$lw_root/state/channels" \
    "$lw_root/state/tools" \
    "$lw_root/state/xmpp" \
    "$lw_root/logs" \
    "$lw_root/run"
  chmod 0700 "$lw_root/env"
  say "created directories under $lw_root"

  ensure_tenant_wasm_toolchain "$name" || true
}

remove_tenant_user() {
  local name="$1"
  local purge="${2:-false}"

  ensure_init_system

  # Capture the uid BEFORE userdel removes the passwd entry — needed below to
  # reap the rootless runtime dir (/run/user/<uid>) once the account is gone.
  local _uid; _uid="$(id -u "$name" 2>/dev/null || true)"

  # Stop the per-user runtime manager so it doesn't keep/recreate /run/user/<uid>.
  # loginctl is provided by systemd-logind AND elogind (OpenRC), so gate on the
  # binary, not the init system — otherwise linger is never disabled on OpenRC.
  if command -v loginctl >/dev/null 2>&1; then
    loginctl disable-linger "$name" 2>/dev/null || true
  fi

  if [[ "$purge" == "true" ]]; then
    # Tear the user session down BEFORE userdel. Running userdel immediately after
    # disable-linger races the still-stopping user@<uid>.service and fails with
    # "user busy" (exit 8); previously that error was swallowed (2>/dev/null||true)
    # and "removed user" printed anyway, leaving orphaned accounts/home dirs.
    if [[ "$INIT_SYSTEM" == "systemd" ]]; then
      loginctl terminate-user "$name" 2>/dev/null || true
      if [[ -n "$_uid" ]]; then
        local _w=0
        while [[ -d "/run/user/$_uid" ]] && (( _w < 20 )); do sleep 0.5; _w=$((_w + 1)); done
      fi
    fi
    pkill -KILL -u "$name" 2>/dev/null || true
    # `userdel -r` prints a benign "mail spool not found" warning but still exits 0;
    # a real failure (user busy) exits nonzero — surface it instead of hiding it.
    if userdel -r "$name" 2>/dev/null; then
      say "removed user and home directory: $name"
    elif ! getent passwd "$name" >/dev/null 2>&1; then
      # Account gone but userdel exited nonzero (e.g. exit 12: home removal failed
      # on a busy mount / immutable file). Don't overclaim the home dir was removed.
      say "user '$name' account removed (userdel -r exited nonzero — home dir may persist; verify $(tenant_home "$name"))"
    else
      say "WARNING: failed to remove user '$name' (still present); remove manually: userdel -r $name"
    fi

    # O1 teardown half: /run/user/<uid> is created by `install -d` in
    # ensure_rootless_prereqs (not by a login session), so logind/elogind never
    # reaps it on disable-linger/terminate-user. Left behind, its stale libpod
    # pause.pid breaks the NEXT tenant that REUSES this uid. Remove it explicitly
    # once the account is gone. Guard on a real tenant uid (>=1000) so we never
    # touch root's or a system user's runtime dir.
    if [[ -n "$_uid" ]] && (( _uid >= 1000 )) \
       && ! getent passwd "$name" >/dev/null 2>&1 \
       && ! getent passwd "$_uid" >/dev/null 2>&1 \
       && [[ -d "/run/user/$_uid" ]]; then
      rm -rf "/run/user/$_uid" 2>/dev/null || true
      if [[ -d "/run/user/$_uid" ]]; then
        say "WARNING: could not fully remove rootless runtime dir /run/user/$_uid (busy mounts?); remove manually once released" >&2
      else
        say "reaped stale rootless runtime dir /run/user/$_uid"
      fi
    fi
  else
    say "user $name preserved (use --purge to remove)"
  fi
}

# ── Repo cloning ─────────────────────────────────────────────────────────────

clone_tenant_repo() {
  local name="$1"
  local dest
  dest="$(tenant_lw_root "$name")"

  if [[ -d "$dest/.git" ]] && [[ -d "$dest/ic" ]]; then
    say "repo already cloned at $dest"
    return 0
  fi
  # Clean up incomplete clone (has .git but missing content)
  if [[ -d "$dest/.git" ]] && [[ ! -d "$dest/ic" ]]; then
    say "incomplete clone detected, removing .git and re-cloning ..."
    rm -rf "$dest/.git"
  fi

  say "cloning repo from $SOURCE_REPO to $dest ..."
  local -a safedir=(-c "safe.directory=$dest" -c "safe.directory=$SOURCE_REPO")
  if [[ -d "$dest" ]] && [[ -n "$(ls -A "$dest")" ]]; then
    # Directory already has content (env/, state/, etc.) — init in place
    git "${safedir[@]}" init "$dest" >/dev/null
    git "${safedir[@]}" -C "$dest" remote add origin "$SOURCE_REPO"
    git "${safedir[@]}" -C "$dest" fetch origin --quiet
    local branch
    branch="$(git "${safedir[@]}" -C "$SOURCE_REPO" symbolic-ref --short HEAD 2>/dev/null || echo staging)"
    git "${safedir[@]}" -C "$dest" checkout -b "$branch" "origin/$branch" 2>&1 | tail -1
  else
    git "${safedir[@]}" clone --single-branch "$SOURCE_REPO" "$dest" 2>&1 | tail -1
  fi
  chown -R "$name:$name" "$dest"
  say "repo cloned for $name"
}

# ── Build management ─────────────────────────────────────────────────────────

# Run one tenant build step with resource bounding (BUG-005). On systemd the
# step runs as a transient scope in BUILD_SLICE via the tenant's own user
# manager, so cargo/rustc memory is capped and accounted outside both the
# operator's session and the daemon service. Falls back to a plain nice'd
# invocation when no user manager is reachable (OpenRC, or not yet started).
_run_tenant_build() {
  local name="$1" build_cmd="$2"
  local uid
  uid="$(id -u "$name")"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == systemd* ]] && command -v systemd-run >/dev/null 2>&1 \
    && sudo -u "$name" XDG_RUNTIME_DIR="/run/user/$uid" systemctl --user show --property=Version >/dev/null 2>&1; then
    sudo -u "$name" XDG_RUNTIME_DIR="/run/user/$uid" \
      systemd-run --user --scope --quiet --collect \
      --slice="$BUILD_SLICE" \
      --property="MemoryHigh=$BUILD_MEMORY_HIGH" \
      --property="MemoryMax=$BUILD_MEMORY_MAX" \
      bash -c "$build_cmd"
  else
    sudo -u "$name" nice -n 10 bash -c "$build_cmd"
  fi
}

build_tenant() {
  local name="$1"
  local with_wasm="${2:-false}"
  local with_pebble="${3:-false}"
  local with_opencode="${4:-false}"
  local with_kimi="${5:-false}"
  local with_toolchains="${6:-false}"
  local require_wasm="${7:-false}"
  local repo
  repo="$(tenant_repo "$name")"

  if [[ "$require_wasm" == "true" && "$with_wasm" != "true" ]]; then
    die "--require-wasm requires --with-wasm"
  fi

  [[ -d "$repo" ]] || die "repo not found at $repo; run add-tenant first"

  say "acquiring build lock (only one tenant builds at a time) ..."
  (
    flock -x 200

    local cargo_env="if [ -f \"\$HOME/.cargo/env\" ]; then . \"\$HOME/.cargo/env\"; else export PATH=\"\$HOME/.cargo/bin:\$PATH\"; fi;"

    say "building lunarwing for $name ..."
    _run_tenant_build "$name" "$cargo_env cd '$repo' && cargo build --profile $PROFILE --bin lunarwing -j$BUILD_JOBS" \
      || die "lunarwing build failed for $name"

    say "building xmpp-bridge for $name ..."
    _run_tenant_build "$name" "$cargo_env cd '$repo/bridges/xmpp-bridge' && cargo build --profile $PROFILE -j$BUILD_JOBS" \
      || die "xmpp-bridge build failed for $name"

    if [[ "$with_wasm" == "true" ]]; then
      if ensure_tenant_wasm_toolchain "$name"; then
        say "building WASM extensions for $name ..."
        local wasm_build_ok="true"
        if ! _run_tenant_build "$name" "$cargo_env cd '$repo' && CARGO_BUILD_JOBS=$BUILD_JOBS bash scripts/build-wasm-extensions.sh"; then
          if [[ "$require_wasm" == "true" ]]; then
            die "WASM extension build failed for $name; refusing to install stale artifacts"
          fi
          say "WARNING: WASM extension build reported errors for $name (some extensions may be missing; re-run '$0 build-tenant $name --with-wasm' after fixing)" >&2
          wasm_build_ok="false"
        fi

        if [[ "$wasm_build_ok" == "true" ]]; then
          say "installing WASM extensions for $name ..."
          install_wasm_tenant "$name"
        else
          say "WARNING: skipping WASM install for $name; existing installed artifacts were preserved" >&2
        fi
      else
        if [[ "$require_wasm" == "true" ]]; then
          die "WASM toolchain incomplete for $name; refusing to continue upgrade"
        fi
        say "WARNING: skipping WASM build for $name (toolchain incomplete — see above)" >&2
      fi
    fi

    say "build complete for $name"
    say ""
    say "Reminder: set your LLM provider API key (if applicable to your backend) in $(tenant_env_dir "$name")/lunarwing.env"
    say "  e.g.  LLM_API_KEY=sk-..."
  ) 200>"$BUILD_LOCK" || return $?

  if [[ "$with_pebble" == "true" ]]; then
    say ""
    say "=== Building pebble worker image ==="
    build_pebble_worker "false"
  fi

  if [[ "$with_opencode" == "true" ]]; then
    say ""
    say "=== Building opencode worker image ==="
    build_opencode_worker "false" "$with_toolchains" "$(tenant_lw_root "$name")"
  fi

  if [[ "$with_kimi" == "true" ]]; then
    say ""
    say "=== Building kimi worker image ==="
    build_kimi_worker "false" "$with_toolchains" "" "$(tenant_lw_root "$name")"
  fi

  # After a rebuild, the tenant will be restarted with the new binary. If the
  # tenant has LUNARWING_OWNER_ID but still has orphaned 'default'-scoped DB
  # data (e.g. upgrading from a pre-owner-id version), auto-migrate so the
  # new binary doesn't lose access to existing conversations and memory.
  local env_path
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  if grep -q '^LUNARWING_OWNER_ID=' "$env_path" 2>/dev/null && _owner_scope_needs_migration "$name"; then
    say ""
    say "--- Auto-migrating owner scope ---"
    migrate_owner_scope "$name" || say "WARNING: owner-scope migration failed (run 'migrate-owner-scope $name' manually)"
  fi
}

# ── Darkirc daemon build (shared, not per-tenant) ────────────────────────────

darkirc_validate_root_owned_directory() {
  local path="$1" rest component current="" owner mode
  [[ "$path" == /* && -d "$path" ]] || return 1
  rest="${path#/}"
  while [[ -n "$rest" ]]; do
    component="${rest%%/*}"
    [[ -n "$component" && "$component" != . && "$component" != .. ]] || return 1
    current="$current/$component"
    [[ -d "$current" && ! -L "$current" ]] || return 1
    owner="$(stat -c '%u' "$current" 2>/dev/null || true)"
    mode="$(stat -c '%a' "$current" 2>/dev/null || true)"
    [[ "$owner" == 0 && "$mode" =~ ^[0-7]+$ ]] || return 1
    (( (8#$mode & 0022) == 0 )) || return 1
    if [[ "$rest" == */* ]]; then rest="${rest#*/}"; else rest=""; fi
  done
}

darkirc_validate_clean_root_checkout() {
  local path="$1" expected_revision="${2:-}" revision dirty git_dir common_dir unsafe
  darkirc_validate_root_owned_directory "$path" || return 1
  revision="$(git -C "$path" rev-parse HEAD 2>/dev/null)" || return 1
  [[ -z "$expected_revision" || "$revision" == "$expected_revision" ]] || return 1
  dirty="$(git -C "$path" status --porcelain --untracked-files=all 2>/dev/null)" || return 1
  [[ -z "$dirty" ]] || return 1
  git_dir="$(git -C "$path" rev-parse --absolute-git-dir 2>/dev/null)" || return 1
  common_dir="$(git -C "$path" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || return 1
  for git_path in "$git_dir" "$common_dir"; do
    darkirc_validate_root_owned_directory "$git_path" || return 1
    unsafe="$(find "$git_path" -xdev \( -type l -o ! -user root -o -perm /022 \) -print -quit 2>/dev/null)" \
      || return 1
    [[ -z "$unsafe" ]] || return 1
  done
  ! git -C "$path" ls-tree -r "$revision" | grep -Eq '^(120000|160000) '
}

ensure_darkirc_build_user() {
  require_cmd useradd
  require_cmd passwd
  require_cmd setpriv
  if ! id "$DARKIRC_BUILD_USER" >/dev/null 2>&1; then
    useradd --system --no-create-home --home-dir "$DARKIRC_BUILD_HOME" \
      --shell "$(command -v nologin || printf '/sbin/nologin')" \
      --comment "LunarWing unprivileged build worker" "$DARKIRC_BUILD_USER"
  fi
  local build_uid build_gid build_groups account_home account_shell account_state
  build_uid="$(id -u "$DARKIRC_BUILD_USER")"
  build_gid="$(id -g "$DARKIRC_BUILD_USER")"
  build_groups="$(id -G "$DARKIRC_BUILD_USER")"
  account_home="$(getent passwd "$DARKIRC_BUILD_USER" | cut -d: -f6)"
  account_shell="$(getent passwd "$DARKIRC_BUILD_USER" | cut -d: -f7)"
  account_state="$(passwd -S "$DARKIRC_BUILD_USER" 2>/dev/null | awk '{print $2}')"
  [[ "$build_uid" -ne 0 && "$build_groups" == "$build_gid" \
    && "$account_home" == "$DARKIRC_BUILD_HOME" \
    && "$account_shell" == */nologin \
    && ( "$account_state" == L || "$account_state" == LK ) ]] \
    || die "DarkIRC build account must be locked, dedicated, and have no supplementary groups"
  DARKIRC_BUILD_GROUP="$(id -gn "$DARKIRC_BUILD_USER")"
  [[ ! -L "$DARKIRC_BUILD_HOME" && ( ! -e "$DARKIRC_BUILD_HOME" || -d "$DARKIRC_BUILD_HOME" ) ]] \
    || die "unsafe DarkIRC build home"
  install -d -o root -g root -m 0711 "$DARKIRC_BUILD_HOME"
  [[ "$(stat -c '%u:%a' "$DARKIRC_BUILD_HOME" 2>/dev/null)" == "0:711" ]] \
    || die "DarkIRC build home must be root-owned mode 0711"
}

darkirc_validate_build_artifact() {
  local artifact="$1" owner links
  [[ -f "$artifact" && ! -L "$artifact" && -x "$artifact" ]] || return 1
  owner="$(stat -c '%U' "$artifact" 2>/dev/null || true)"
  links="$(stat -c '%h' "$artifact" 2>/dev/null || true)"
  [[ "$owner" == "$DARKIRC_BUILD_USER" && "$links" == 1 ]]
}

darkirc_path_components_safe() {
  local path="$1" rest component current=""
  [[ "$path" == /* ]] || return 1
  rest="${path#/}"
  while [[ -n "$rest" ]]; do
    component="${rest%%/*}"
    [[ -n "$component" && "$component" != . && "$component" != .. ]] || return 1
    current="$current/$component"
    [[ ! -L "$current" ]] || return 1
    if [[ "$rest" == */* ]]; then rest="${rest#*/}"; else rest=""; fi
  done
}

darkirc_validate_root_executable() {
  local path="$1" mode owner links
  darkirc_path_components_safe "$path" || return 1
  darkirc_validate_root_owned_directory "$(dirname "$path")" || return 1
  [[ -f "$path" && ! -L "$path" && -x "$path" ]] || return 1
  mode="$(stat -c '%a' "$path" 2>/dev/null || true)"
  owner="$(stat -c '%u' "$path" 2>/dev/null || true)"
  links="$(stat -c '%h' "$path" 2>/dev/null || true)"
  [[ "$owner" == 0 && "$links" == 1 && "$mode" =~ ^[0-7]+$ ]] || return 1
  (( (8#$mode & 07022) == 0 ))
}

install_darkirc_key_helper() {
  local archive_root cargo_home artifact
  require_cmd cargo
  require_cmd taskset
  require_cmd tar
  darkirc_validate_clean_root_checkout "$LUNARWING_ROOT" \
    || die "trusted LunarWing admin checkout must be root-owned, non-writable, and clean"
  ensure_darkirc_build_user
  [[ "$DARKIRC_KEY_HELPER_BUILD_ROOT" == /var/cache/lunarwing/* \
    && "$DARKIRC_KEY_HELPER_BUILD_ROOT" != *'/../'* \
    && ! -L "$DARKIRC_KEY_HELPER_BUILD_ROOT" ]] \
    || die "DarkIRC helper build root must be a safe /var/cache/lunarwing path"
  rm -rf -- "$DARKIRC_KEY_HELPER_BUILD_ROOT"
  install -d -o "$DARKIRC_BUILD_USER" -g "$DARKIRC_BUILD_GROUP" -m 0700 \
    "$DARKIRC_KEY_HELPER_BUILD_ROOT"
  archive_root="$DARKIRC_KEY_HELPER_BUILD_ROOT/source"
  install -d -o root -g root -m 0700 "$archive_root"
  git -C "$LUNARWING_ROOT" archive --format=tar HEAD | tar -x -C "$archive_root"
  chown -R "$DARKIRC_BUILD_USER:$DARKIRC_BUILD_GROUP" "$archive_root"
  cargo_home="$(mktemp -d "$DARKIRC_BUILD_HOME/cargo.XXXXXX")"
  install -d -o root -g root -m 0700 "$cargo_home/.cargo"
  chown -R "$DARKIRC_BUILD_USER:$DARKIRC_BUILD_GROUP" "$cargo_home"
  say "building trusted DarkIRC key helper as $DARKIRC_BUILD_USER ..."
  setpriv --reuid="$(id -u "$DARKIRC_BUILD_USER")" \
    --regid="$(id -g "$DARKIRC_BUILD_USER")" --clear-groups --no-new-privs -- \
    env -i HOME="$cargo_home" PATH="/usr/bin:/bin" CARGO_HOME="$cargo_home/.cargo" \
    CARGO_TARGET_DIR="$DARKIRC_KEY_HELPER_BUILD_ROOT" \
    taskset -c 0-1 cargo build --manifest-path "$archive_root/ic/Cargo.toml" \
      --release -j2 --bin lunarwing-darkirc-key-helper \
    || die "DarkIRC key helper build failed"
  darkirc_validate_clean_root_checkout "$LUNARWING_ROOT" \
    || die "DarkIRC helper build changed trusted LunarWing checkout"
  rm -rf -- "$cargo_home"
  artifact="$DARKIRC_KEY_HELPER_BUILD_ROOT/release/lunarwing-darkirc-key-helper"
  darkirc_validate_build_artifact "$artifact" \
    || die "DarkIRC key helper artifact failed ownership validation"
  install -d -o root -g root -m 0755 "$(dirname "$DARKIRC_KEY_HELPER")"
  install -o root -g root -m 0755 "$artifact" "$DARKIRC_KEY_HELPER"
  darkirc_validate_root_executable "$DARKIRC_KEY_HELPER" \
    || die "installed DarkIRC key helper failed ownership validation"
}

write_darkirc_compatibility() {
  local source_revision="$1" test_user="$2" digest tmp
  require_cmd jq
  require_cmd sha256sum
  [[ "$DARKIRC_BIN" == "/usr/local/bin/darkirc" ]] \
    || die "managed DarkIRC config requires binary path /usr/local/bin/darkirc"
  [[ "$source_revision" == "$DARKIRC_APPROVED_REV" ]] \
    || die "DarkIRC source revision is not approved for managed config"
  digest="$(sha256sum "$DARKIRC_BIN")"
  digest="sha256:${digest%% *}"
  darkirc_validate_root_executable "$DARKIRC_KEY_HELPER" \
    || die "DarkIRC key helper is missing or unsafe"
  id "$test_user" >/dev/null 2>&1 || die "DarkIRC self-test user '$test_user' does not exist"
  sudo -u "$test_user" env -i HOME="$(getent passwd "$test_user" | cut -d: -f6)" PATH="/usr/bin:/bin" \
    "$DARKIRC_KEY_HELPER" self-test-generator \
    --generator-profile darkfi-a05956d41-chacha-v1 \
    --binary-sha256 "$digest" --binary-path "$DARKIRC_BIN" \
    --key-format darkfi-chacha-base58-32 --source-revision "$source_revision" >/dev/null \
    || die "installed DarkIRC generator failed compatibility self-test"
  install -d -o root -g root -m 0755 "$(dirname "$DARKIRC_COMPAT_FILE")"
  tmp="$(mktemp "$DARKIRC_COMPAT_FILE.tmp.XXXXXX")"
  jq -n --arg digest "$digest" --arg revision "$source_revision" \
    '{schema:"lunarwing.darkirc-compat/v1",profile_id:"darkfi-a05956d41-chacha-v1",binary_sha256:$digest,key_format:"darkfi-chacha-base58-32",source_revision:$revision}' \
    >"$tmp" || { rm -f "$tmp"; die "failed to create DarkIRC compatibility attestation"; }
  chmod 0600 "$tmp"
  chown root:root "$tmp"
  mv "$tmp" "$DARKIRC_COMPAT_FILE"
  darkirc_load_compatibility
}

build_darkirc() {
  local tenant_name="${1:-}" darkirc_src test_user build_tree built_bin source_revision cargo_home
  local previous_binary="" previous_compatibility="" previous_helper=""
  local publish_pending=false helper_publish_pending=false binary_candidate=""
  darkirc_src="${DARKIRC_SOURCE:-/var/cache/lunarwing/darkfi-source}"
  test_user="${tenant_name:-${SUDO_USER:-nobody}}"
  [[ "$test_user" != root ]] || test_user=nobody

  say "acquiring build lock for darkirc ..."
  (
    flock -x 200
    [[ "$DARKIRC_REV" == "$DARKIRC_APPROVED_REV" ]] \
      || die "DarkIRC revision is not approved: $DARKIRC_REV"
    if [[ ! -d "$darkirc_src/.git" ]]; then
      local source_parent
      source_parent="$(dirname "$darkirc_src")"
      [[ -d "$source_parent" ]] || install -d -o root -g root -m 0700 "$source_parent"
      darkirc_validate_root_owned_directory "$source_parent" \
        || die "DarkIRC source parent must be root-owned and non-writable"
      say "darkirc source not present at $darkirc_src — cloning pinned source as root ..."
      git clone "$DARKIRC_REPO" "$darkirc_src" || die "darkirc clone failed"
      git -C "$darkirc_src" checkout --detach "$DARKIRC_REV" \
        || die "darkirc checkout '$DARKIRC_REV' failed"
    fi
    darkirc_validate_clean_root_checkout "$darkirc_src" "$DARKIRC_REV" \
      || die "DarkIRC source must be root-owned, clean, and at approved revision"
    [[ -f "$darkirc_src/Makefile" && -f "$darkirc_src/bin/darkirc/Cargo.toml" ]] \
      || die "approved DarkIRC source layout is incomplete"

    ensure_darkirc_build_user
    install -d -o root -g root -m 0711 "$DARKIRC_BUILD_ROOT"
    build_tree="$(mktemp -d "$DARKIRC_BUILD_ROOT/build.XXXXXX")"
    cleanup_darkirc_build() {
      if [[ "$publish_pending" == true ]]; then
        if [[ -n "$previous_binary" ]]; then mv -f -- "$previous_binary" "$DARKIRC_BIN"; previous_binary=""; else rm -f -- "$DARKIRC_BIN"; fi
        if [[ -n "$previous_compatibility" ]]; then mv -f -- "$previous_compatibility" "$DARKIRC_COMPAT_FILE"; previous_compatibility=""; else rm -f -- "$DARKIRC_COMPAT_FILE"; fi
      fi
      if [[ "$helper_publish_pending" == true ]]; then
        if [[ -n "$previous_helper" ]]; then mv -f -- "$previous_helper" "$DARKIRC_KEY_HELPER"; previous_helper=""; else rm -f -- "$DARKIRC_KEY_HELPER"; fi
      fi
      [[ -z "$previous_binary" ]] || rm -f -- "$previous_binary"
      [[ -z "$previous_compatibility" ]] || rm -f -- "$previous_compatibility"
      [[ -z "$previous_helper" ]] || rm -f -- "$previous_helper"
      [[ -z "$binary_candidate" ]] || rm -f -- "$binary_candidate"
      [[ -z "${build_tree:-}" ]] || rm -rf -- "$build_tree"
      [[ -z "${cargo_home:-}" ]] || rm -rf -- "$cargo_home"
    }
    trap cleanup_darkirc_build EXIT
    git -C "$darkirc_src" archive --format=tar "$DARKIRC_APPROVED_REV" | tar -x -C "$build_tree"
    chown -R "$DARKIRC_BUILD_USER:$DARKIRC_BUILD_GROUP" "$build_tree"
    cargo_home="$(mktemp -d "$DARKIRC_BUILD_HOME/cargo.XXXXXX")"
    install -d -o root -g root -m 0700 "$cargo_home/.cargo"
    chown -R "$DARKIRC_BUILD_USER:$DARKIRC_BUILD_GROUP" "$cargo_home"
    say "building darkirc as $DARKIRC_BUILD_USER from disposable source copy ..."
    setpriv --reuid="$(id -u "$DARKIRC_BUILD_USER")" \
      --regid="$(id -g "$DARKIRC_BUILD_USER")" --clear-groups --no-new-privs -- \
      env -i HOME="$cargo_home" PATH="/usr/bin:/bin" CARGO_HOME="$cargo_home/.cargo" \
      MAKEFLAGS=-j2 taskset -c 0-1 make -C "$build_tree" darkirc \
      || die "darkirc build failed"
    darkirc_validate_clean_root_checkout "$darkirc_src" "$DARKIRC_REV" \
      || die "DarkIRC build changed trusted source checkout"
    built_bin="$build_tree/darkirc"
    darkirc_validate_build_artifact "$built_bin" \
      || die "darkirc build artifact failed ownership validation"
    source_revision="$(git -C "$darkirc_src" rev-parse HEAD 2>/dev/null)" \
      || die "could not attest DarkIRC source revision"
    [[ "$source_revision" == "$DARKIRC_APPROVED_REV" ]] \
      || die "DarkIRC checkout is at unapproved revision $source_revision"

    if [[ -e "$DARKIRC_KEY_HELPER" ]]; then
      darkirc_validate_root_executable "$DARKIRC_KEY_HELPER" \
        || die "installed DarkIRC key helper is unsafe; refusing transactional replacement"
      previous_helper="$(mktemp "$(dirname "$DARKIRC_KEY_HELPER")/.helper.rollback.XXXXXX")"
      install -o root -g root -m 0755 "$DARKIRC_KEY_HELPER" "$previous_helper"
    fi
    helper_publish_pending=true
    install_darkirc_key_helper
    say "installing darkirc to $DARKIRC_BIN ..."
    install -d -o root -g root -m 0755 "$(dirname "$DARKIRC_BIN")"
    binary_candidate="$(mktemp "$(dirname "$DARKIRC_BIN")/.darkirc.XXXXXX")"
    install -o root -g root -m 0755 "$built_bin" "$binary_candidate"
    if [[ -e "$DARKIRC_BIN" ]]; then
      darkirc_validate_root_executable "$DARKIRC_BIN" \
        || die "installed DarkIRC binary is unsafe; refusing transactional replacement"
      previous_binary="$(mktemp "$(dirname "$DARKIRC_BIN")/.darkirc.rollback.XXXXXX")"
      install -o root -g root -m 0755 "$DARKIRC_BIN" "$previous_binary"
    fi
    install -d -o root -g root -m 0755 "$(dirname "$DARKIRC_COMPAT_FILE")"
    if [[ -e "$DARKIRC_COMPAT_FILE" ]]; then
      [[ -f "$DARKIRC_COMPAT_FILE" && ! -L "$DARKIRC_COMPAT_FILE" \
        && "$(stat -c '%u:%a' "$DARKIRC_COMPAT_FILE" 2>/dev/null)" == "0:600" ]] \
        || die "installed DarkIRC compatibility attestation is unsafe"
      previous_compatibility="$(mktemp "$(dirname "$DARKIRC_COMPAT_FILE")/.compat.rollback.XXXXXX")"
      install -o root -g root -m 0600 "$DARKIRC_COMPAT_FILE" "$previous_compatibility"
    fi
    publish_pending=true
    mv "$binary_candidate" "$DARKIRC_BIN"
    binary_candidate=""
    rm -rf -- "$build_tree" "$cargo_home"
    build_tree=""; cargo_home=""
    write_darkirc_compatibility "$source_revision" "$test_user"
    publish_pending=false
    helper_publish_pending=false
    rm -f -- "$previous_binary" "$previous_compatibility" "$previous_helper"
    previous_binary=""; previous_compatibility=""; previous_helper=""
    say "darkirc build complete: $DARKIRC_BIN"
    sudo -u "$test_user" "$DARKIRC_BIN" --version \
      || say "(darkirc --version not supported, skipping)"
  ) 200>"$BUILD_LOCK"
}

shared_worker_source_root() {  # <names> <with-opencode> <with-kimi>
  local names="$1" with_opencode="$2" with_kimi="$3"
  local name root commit dirty expected_commit="" selected_root=""
  local -a worker_paths=()
  [[ "$with_opencode" != "true" ]] || worker_paths+=(opencode4lunarwing)
  [[ "$with_kimi" != "true" ]] || worker_paths+=(kimicode4lunarwing)
  while IFS= read -r name; do
    [[ -n "$name" ]] || continue
    root="$(tenant_lw_root "$name")"
    commit="$(sudo -u "$name" git -C "$root" rev-parse HEAD 2>/dev/null)" \
      || die "cannot resolve tenant checkout for worker build: $name"
    dirty="$(sudo -u "$name" git -C "$root" status --porcelain --untracked-files=all -- "${worker_paths[@]}")" \
      || die "cannot inspect tenant worker source: $name"
    [[ -z "$dirty" ]] \
      || die "build-all refuses dirty worker source in tenant checkout: $name"
    if [[ -z "$expected_commit" ]]; then
      expected_commit="$commit"
      selected_root="$root"
    elif [[ "$commit" != "$expected_commit" ]]; then
      die "build-all cannot publish one shared worker image from mixed tenant revisions"
    fi
  done <<<"$names"
  [[ -n "$selected_root" ]] || die "no tenant checkout available for worker build"
  printf '%s\n' "$selected_root"
}

build_all() {
  local with_wasm="${1:-false}"
  local with_pebble="${2:-false}"
  local with_opencode="${3:-false}"
  local with_kimi="${4:-false}"
  local with_toolchains="${5:-false}"
  local names
  names="$(all_tenant_names)"

  if [[ -z "$names" ]]; then
    say "no tenants registered"
    return 0
  fi

  local worker_source_root=""
  if [[ "$with_opencode" == "true" || "$with_kimi" == "true" ]]; then
    worker_source_root="$(shared_worker_source_root "$names" "$with_opencode" "$with_kimi")"
  fi

  if [[ "$with_pebble" == "true" ]]; then
    say ""
    say "=== Building pebble worker image ==="
    build_pebble_worker "false"
  fi

  if [[ "$with_opencode" == "true" ]]; then
    say ""
    say "=== Building opencode worker image ==="
    build_opencode_worker "false" "$with_toolchains" "$worker_source_root"
  fi

  if [[ "$with_kimi" == "true" ]]; then
    say ""
    say "=== Building kimi worker image ==="
    build_kimi_worker "false" "$with_toolchains" "" "$worker_source_root"
  fi

  while IFS= read -r name; do
    say ""
    say "=== Building tenant: $name ==="
    build_tenant "$name" "$with_wasm" "false" "false" "false" "$with_toolchains" "false"
  done <<< "$names"
}

build_pebble_worker() {
  local no_cache="${1:-false}"
  local pebble_dir="${LUNARWING_ROOT}/pebble4lunarwing"

  [[ -d "$pebble_dir" ]] || die "pebble worker dir not found at $pebble_dir"

  ensure_container_runtime

  say "building pebble worker Docker image ..."
  local cache_flag=""
  [[ "$no_cache" == "true" ]] && cache_flag="--no-cache"

  if [[ "$CONTAINER_RT" == "podman" ]]; then
    # --network=host: podman's default build network
    # can't reach the internet for RUN steps on this host; the host netns has IPv4.
    # --format docker (O4): preserve the Dockerfile HEALTHCHECK (podman OCI drops it).
    podman build $cache_flag --network=host --format docker -t lunarwing-worker-pebble:latest -f "$pebble_dir/Dockerfile" "$LUNARWING_ROOT" \
      || die "pebble worker image build failed"
  else
    docker build $cache_flag -t lunarwing-worker-pebble:latest -f "$pebble_dir/Dockerfile" "$LUNARWING_ROOT" \
      || die "pebble worker image build failed"
  fi

  say "pebble worker image built: lunarwing-worker-pebble:latest"
}

build_opencode_worker() {
  local no_cache="${1:-false}"
  local with_toolchains="${2:-false}"
  local source_root="${3:-$LUNARWING_ROOT}"
  local opencode_dir="${source_root}/opencode4lunarwing"

  [[ -d "$opencode_dir" ]] || die "opencode worker dir not found at $opencode_dir"

  ensure_container_runtime

  local toolchain_desc="slim (no toolchains)"
  [[ "$with_toolchains" == "true" ]] && toolchain_desc="fat (with Rust/Go/C++ toolchains)"
  say "building opencode worker Docker image [$toolchain_desc] ..."

  local -a cache_args=()
  [[ "$no_cache" == "true" ]] && cache_args=(--no-cache)

  local -a toolchain_args=()
  [[ "$with_toolchains" == "true" ]] && toolchain_args=(--build-arg WITH_TOOLCHAINS=true)

  if [[ "$CONTAINER_RT" == "podman" ]]; then
    local -a layer_args=()
    if [[ "$no_cache" == "true" ]]; then
      # --no-cache avoids reading old layers but still creates new intermediate
      # layers by default. Drop safe dangling images first, then keep this large
      # Bun build in one layer per stage to reduce peak storage and inode use.
      say "pruning dangling Podman images before uncached OpenCode build ..."
      if ! podman image prune -f >/dev/null; then
        say "WARNING: failed to prune dangling Podman images; continuing build" >&2
      fi
      layer_args=(--layers=false)
    fi
    podman build "${cache_args[@]}" "${layer_args[@]}" "${toolchain_args[@]}" --network=host --format docker -t lunarwing-worker-opencode:latest "$opencode_dir" \
      || die "opencode worker image build failed"
  else
    docker build "${cache_args[@]}" "${toolchain_args[@]}" -t lunarwing-worker-opencode:latest "$opencode_dir" \
      || die "opencode worker image build failed"
  fi

  say "opencode worker image built: lunarwing-worker-opencode:latest [$toolchain_desc]"
}

build_kimi_worker() {
  local no_cache="${1:-false}"
  local with_toolchains="${2:-false}"
  local kimi_code_version="${3:-}"
  local source_root="${4:-$LUNARWING_ROOT}"
  local kimi_dir="${source_root}/kimicode4lunarwing"

  [[ -d "$kimi_dir" ]] || die "kimi worker dir not found at $kimi_dir"

  ensure_container_runtime

  local toolchain_desc="slim (no toolchains)"
  [[ "$with_toolchains" == "true" ]] && toolchain_desc="fat (with Rust/Go/C++ toolchains)"
  local version_desc="Dockerfile default"
  [[ -z "$kimi_code_version" ]] || version_desc="$kimi_code_version"
  say "building kimi worker Docker image [$toolchain_desc] (kimi-code version: ${version_desc}) ..."

  local -a cache_args=()
  [[ "$no_cache" == "true" ]] && cache_args=(--no-cache)

  local -a toolchain_args=()
  [[ "$with_toolchains" == "true" ]] && toolchain_args=(--build-arg WITH_TOOLCHAINS=true)

  local -a version_args=()
  [[ -z "$kimi_code_version" ]] || version_args=(--build-arg KIMI_CODE_VERSION="${kimi_code_version}")

  if [[ "$CONTAINER_RT" == "podman" ]]; then
    local -a layer_args=()
    if [[ "$no_cache" == "true" ]]; then
      say "pruning dangling Podman images before uncached Kimi build ..."
      if ! podman image prune -f >/dev/null; then
        say "WARNING: failed to prune dangling Podman images; continuing build" >&2
      fi
      layer_args=(--layers=false)
    fi
    podman build "${cache_args[@]}" "${layer_args[@]}" "${toolchain_args[@]}" "${version_args[@]}" --network=host --format docker -t lunarwing-worker-kimi:latest "$kimi_dir" \
      || die "kimi worker image build failed"
  else
    docker build "${cache_args[@]}" "${toolchain_args[@]}" "${version_args[@]}" -t lunarwing-worker-kimi:latest "$kimi_dir" \
      || die "kimi worker image build failed"
  fi

  say "kimi worker image built: lunarwing-worker-kimi:latest [$toolchain_desc]"
}

# ── WASM install ─────────────────────────────────────────────────────────────

channel_crate_name() {
  case "$1" in
    weechat) printf 'weechat_relay_channel' ;;
    *)       printf '%s_channel' "$1" ;;
  esac
}

tool_binary_name() {
  printf '%s_tool' "$(printf '%s' "$1" | tr '-' '_')"
}

install_wasm_tenant() {
  local name="$1"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"

  local repo state_dir
  repo="$(tenant_repo "$name")"
  state_dir="$(tenant_state_dir "$name")"

  local channels_dir="$state_dir/channels"
  local tools_dir="$state_dir/tools"

  mkdir -p "$channels_dir" "$tools_dir"

  # Resolve wasm-tools. add-tenant installs it for the *tenant* user under
  # ~/.cargo/bin; this function runs as root/admin, so prefer the tenant's copy
  # (otherwise we'd miss it and warn spuriously) before falling back to the
  # admin PATH. Output is chowned to the tenant at the end either way.
  local wasm_tools="" tenant_wasm_tools
  tenant_wasm_tools="$(tenant_home "$name")/.cargo/bin/wasm-tools"
  if [[ -x "$tenant_wasm_tools" ]]; then
    wasm_tools="$tenant_wasm_tools"
  elif command -v wasm-tools >/dev/null 2>&1; then
    wasm_tools="wasm-tools"
  fi

  local has_wasm_tools=true
  if [[ -z "$wasm_tools" ]]; then
    say "note: wasm-tools not installed — installing raw WASM components (works fine; skipping optional debug-info strip)"
    has_wasm_tools=false
  fi

  local installed=0 skipped=0

  say "installing WASM channels to $channels_dir..."
  for dir in "$repo/channels-src"/*/; do
    [[ -d "$dir" ]] || continue
    local ch_name crate_name src_wasm dest_wasm caps_src caps_dest
    ch_name="$(basename "$dir")"
    crate_name="$(channel_crate_name "$ch_name")"
    src_wasm="$dir/target/wasm32-wasip2/release/${crate_name}.wasm"
    dest_wasm="$channels_dir/${ch_name}.wasm"
    caps_src="$dir/${ch_name}.capabilities.json"
    caps_dest="$channels_dir/${ch_name}.capabilities.json"

    if [[ ! -f "$src_wasm" ]]; then
      skipped=$((skipped + 1))
      continue
    fi

    if [[ "$has_wasm_tools" == "true" ]]; then
      "$wasm_tools" component new "$src_wasm" -o "$dest_wasm" 2>/dev/null \
        || cp "$src_wasm" "$dest_wasm"
      "$wasm_tools" strip "$dest_wasm" -o "$dest_wasm" 2>/dev/null || true
    else
      cp "$src_wasm" "$dest_wasm"
    fi

    if [[ -f "$caps_src" ]]; then
      cp "$caps_src" "$caps_dest"
    fi
    say "  installed channel: $ch_name"
    installed=$((installed + 1))
  done

  say "installing WASM tools to $tools_dir..."
  for dir in "$repo/tools-src"/*/; do
    [[ -d "$dir" ]] || continue
    local t_name bin_name install_name src_wasm dest_wasm caps_src caps_dest
    t_name="$(basename "$dir")"
    bin_name="$(tool_binary_name "$t_name")"
    install_name="${t_name}-tool"
    src_wasm="$dir/target/wasm32-wasip2/release/${bin_name}.wasm"
    dest_wasm="$tools_dir/${install_name}.wasm"
    caps_dest="$tools_dir/${install_name}.capabilities.json"

    if [[ ! -f "$src_wasm" ]]; then
      skipped=$((skipped + 1))
      continue
    fi

    if [[ "$has_wasm_tools" == "true" ]]; then
      "$wasm_tools" component new "$src_wasm" -o "$dest_wasm" 2>/dev/null \
        || cp "$src_wasm" "$dest_wasm"
      "$wasm_tools" strip "$dest_wasm" -o "$dest_wasm" 2>/dev/null || true
    else
      cp "$src_wasm" "$dest_wasm"
    fi

    caps_src="$dir/${install_name}.capabilities.json"
    if [[ ! -f "$caps_src" ]]; then
      caps_src="$dir/${t_name}.capabilities.json"
    fi
    if [[ -f "$caps_src" ]]; then
      cp "$caps_src" "$caps_dest"
    fi
    say "  installed tool: $install_name"
    installed=$((installed + 1))
  done

  chown -R "$name:$name" "$channels_dir" "$tools_dir"
  patch_ssh_tool_allowlist "$name"

  local gotify_config="$state_dir/workspace/config/gotify.json"
  if [[ -f "$gotify_config" ]] && [[ -f "$tools_dir/gotify-tool.capabilities.json" ]]; then
    local gotify_url
    gotify_url="$(jq -r '.url // empty' "$gotify_config" 2>/dev/null)"
    if [[ -n "$gotify_url" ]]; then
      configure_gotify_capabilities "$name" "$gotify_url"
    fi
  fi

  say "WASM install for $name: $installed installed, $skipped skipped (not built)"
}

install_wasm_all() {
  local names
  names="$(all_tenant_names)"

  if [[ -z "$names" ]]; then
    say "no tenants registered"
    return 0
  fi

  while IFS= read -r name; do
    say ""
    say "=== Installing WASM for tenant: $name ==="
    install_wasm_tenant "$name"
  done <<< "$names"
}

# ── Environment file generation ──────────────────────────────────────────────

# Echo the existing VALUE of KEY from a tenant env file (empty if file/key absent).
# Used to PRESERVE secrets across re-runs so re-provisioning never rotates them.
_env_existing() {  # <env_file> <KEY>
  _read_env_value "$1" "$2"
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
    [[ "$inherited_fd" =~ ^[0-9]+$ && "$inherited_scope" == "$scope_id" \
      && -e "/proc/self/fd/$inherited_fd" ]] \
      || die "invalid inherited DarkIRC writer lock"
    [[ "$(stat -c '%d:%i' "/proc/self/fd/$inherited_fd" 2>/dev/null)" \
      == "$(stat -c '%d:%i' "$lock_file" 2>/dev/null)" ]] \
      || die "inherited DarkIRC writer lock does not match tenant scope"
    flock -x -n "$inherited_fd" || die "inherited DarkIRC writer lock is not held"
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
  darkirc_path_components_safe "$DARKIRC_COMPAT_FILE" \
    || die "unsafe DarkIRC compatibility attestation path"
  darkirc_validate_root_owned_directory "$(dirname "$DARKIRC_COMPAT_FILE")" \
    || die "unsafe DarkIRC compatibility attestation parent"
  [[ -f "$DARKIRC_COMPAT_FILE" && ! -L "$DARKIRC_COMPAT_FILE" ]] \
    || die "DarkIRC compatibility attestation is missing: $DARKIRC_COMPAT_FILE"
  local mode owner links schema profile digest key_format source_revision actual
  mode="$(stat -c '%a' "$DARKIRC_COMPAT_FILE" 2>/dev/null || true)"
  owner="$(stat -c '%u' "$DARKIRC_COMPAT_FILE" 2>/dev/null || true)"
  links="$(stat -c '%h' "$DARKIRC_COMPAT_FILE" 2>/dev/null || true)"
  [[ "$mode" == 600 && "$owner" == 0 && "$links" == 1 ]] \
    || die "DarkIRC compatibility attestation must be root-owned, single-link mode 0600"
  schema="$(jq -r '.schema // empty' "$DARKIRC_COMPAT_FILE" 2>/dev/null || true)"
  profile="$(jq -r '.profile_id // empty' "$DARKIRC_COMPAT_FILE" 2>/dev/null || true)"
  digest="$(jq -r '.binary_sha256 // empty' "$DARKIRC_COMPAT_FILE" 2>/dev/null || true)"
  key_format="$(jq -r '.key_format // empty' "$DARKIRC_COMPAT_FILE" 2>/dev/null || true)"
  source_revision="$(jq -r '.source_revision // empty' "$DARKIRC_COMPAT_FILE" 2>/dev/null || true)"
  [[ "$schema" == "lunarwing.darkirc-compat/v1" \
    && "$profile" == "darkfi-a05956d41-chacha-v1" \
    && "$key_format" == "darkfi-chacha-base58-32" \
    && "$source_revision" == "$DARKIRC_APPROVED_REV" \
    && "$digest" =~ ^sha256:[0-9a-f]{64}$ ]] \
    || die "unsupported DarkIRC compatibility attestation"
  darkirc_validate_root_executable "$DARKIRC_BIN" \
    || die "attested DarkIRC binary is missing or unsafe: $DARKIRC_BIN"
  actual="$(sha256sum "$DARKIRC_BIN")"
  actual="sha256:${actual%% *}"
  [[ "$actual" == "$digest" ]] || die "DarkIRC binary digest does not match attestation"
  DARKIRC_COMPAT_ARGS=(
    --generator-profile "$profile"
    --binary-sha256 "$digest"
    --binary-path "$DARKIRC_BIN"
    --key-format "$key_format"
    --source-revision "$source_revision"
  )
}

run_darkirc_manifest_validator() {
  local unbound="${1:-false}"
  darkirc_validate_root_executable "$DARKIRC_KEY_HELPER" \
    || die "trusted DarkIRC key helper is missing; run build-darkirc before machine import"
  if [[ "$unbound" == "true" ]]; then
    "$DARKIRC_KEY_HELPER" validate-migration --json --unbound
  else
    darkirc_load_compatibility
    "$DARKIRC_KEY_HELPER" validate-migration --json "${DARKIRC_COMPAT_ARGS[@]}"
  fi
}

run_darkirc_key_helper() {
  local name="$1"
  shift
  darkirc_validate_root_executable "$DARKIRC_KEY_HELPER" \
    || die "DarkIRC key helper is missing or unsafe: $DARKIRC_KEY_HELPER"
  darkirc_load_compatibility
  local uid gid home
  uid="$(id -u "$name")" || die "cannot resolve uid for tenant '$name'"
  gid="$(id -g "$name")" || die "cannot resolve gid for tenant '$name'"
  home="$(tenant_home "$name")"
  (
    cd "$home"
    exec sudo -u "$name" env -i HOME="$home" PATH="/usr/bin:/bin" \
      "$DARKIRC_KEY_HELPER" "$@" "${DARKIRC_COMPAT_ARGS[@]}" \
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
upgrade_external_worker_timeout() {
  local name="$1" worker="$2" config_path="$3"
  local awk_status temp_path

  if [[ "$MIGRATE_LEGACY_EXTERNAL_WORKER_TIMEOUTS" != "true" ]] ||
     [[ "$DEFAULT_EXTERNAL_WORKER_TIMEOUT_MS" == "$LEGACY_EXTERNAL_WORKER_TIMEOUT_MS" ]]; then
    return 1
  fi

  if ! temp_path="$(mktemp "${config_path}.tmp.XXXXXX")"; then
    return 2
  fi

  if awk \
    -v worker="$worker" \
    -v old_timeout="$LEGACY_EXTERNAL_WORKER_TIMEOUT_MS" \
    -v new_timeout="$DEFAULT_EXTERNAL_WORKER_TIMEOUT_MS" '
      BEGIN {
        in_external_worker = 0
        target_worker = 0
        changed = 0
      }
      /^\[\[sandbox\.external_workers\]\][[:space:]]*$/ {
        in_external_worker = 1
        target_worker = 0
        print
        next
      }
      /^\[/ {
        in_external_worker = 0
        target_worker = 0
      }
      in_external_worker && $0 == "name = \"" worker "\"" {
        target_worker = 1
      }
      in_external_worker && target_worker && $0 == "timeout_ms = " old_timeout {
        print "timeout_ms = " new_timeout
        changed = 1
        next
      }
      { print }
      END { if (!changed) exit 42 }
    ' "$config_path" >"$temp_path"; then
    if ! chown "$name:$name" "$temp_path" ||
       ! chmod 600 "$temp_path" ||
       ! mv -f -- "$temp_path" "$config_path"; then
      rm -f -- "$temp_path"
      return 2
    fi
    say "upgraded external worker '$worker' timeout to ${DEFAULT_EXTERNAL_WORKER_TIMEOUT_MS}ms"
    return 0
  else
    awk_status=$?
    rm -f -- "$temp_path"
    if [[ "$awk_status" -eq 42 ]]; then
      return 1
    fi
    return 2
  fi
}

ensure_external_worker_config() {
  local name="$1"      # tenant name
  local worker="$2"    # logical worker name, matches create_job mode
  local port_key="$3"  # ports-registry key for its WSS port

  local state_dir env_path config_path wss_port auth_token

  if [[ "$worker" == "kimi" ]] && ! tenant_worker_enabled "$name" kimi; then
    say "kimi worker not selected for $name (skipping external-worker config)"
    return 0
  fi
  state_dir="$(tenant_state_dir "$name")"
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  config_path="$state_dir/config.toml"

  wss_port="$(ports_get "$name" "$port_key")"
  if [[ -z "$wss_port" ]]; then
    say "no $port_key port allocated for $name (skipping $worker external-worker config)"
    return 0
  fi

  if [[ ! "$DEFAULT_EXTERNAL_WORKER_TIMEOUT_MS" =~ ^[1-9][0-9]*$ ]]; then
    die "LUNARWING_MT_EXTERNAL_WORKER_TIMEOUT_MS must be a positive integer"
  fi

  # Already configured? Upgrade only the exact legacy timeout, then skip so
  # re-runs / patch-env are idempotent. The daemon's
  # TOML writer emits `name = "<worker>"` identically, so this also matches a
  # file that has been load-modify-saved by `/model`.
  if [[ -f "$config_path" ]] && grep -q "name = \"$worker\"" "$config_path" 2>/dev/null; then
    local upgrade_status=0
    upgrade_external_worker_timeout "$name" "$worker" "$config_path" || upgrade_status=$?
    case "$upgrade_status" in
      0) ;;
      1) say "external worker '$worker' already configured in $config_path (skipping)" ;;
      *) die "failed to upgrade external worker '$worker' timeout in $config_path" ;;
    esac
    return 0
  fi

  auth_token="$(_read_env_value "$env_path" GATEWAY_AUTH_TOKEN)"

  # add-tenant runs before the daemon ever starts, so config.toml usually does
  # not exist yet — create it with a header. Appending a fresh
  # `[[sandbox.external_workers]]` array-of-tables to an existing file is valid
  # TOML because we only ever append when no entry for this array exists yet.
  if [[ ! -f "$config_path" ]]; then
    sudo -u "$name" mkdir -p "$state_dir"
    (
      umask 077
      cat >"$config_path" <<'HDR'
# LunarWing tenant configuration (auto-generated by lunarwing-mt-admin.sh).
#
# Priority: env var > this file > database settings > defaults.
# The external-worker blocks below wire create_job(mode: "<name>") to the
# per-tenant worker containers. Hand edits outside these blocks are preserved.
HDR
    )
  fi

  {
    printf '\n# External worker: %s — create_job(mode: "%s")\n' "$worker" "$worker"
    printf '[[sandbox.external_workers]]\n'
    printf 'name = "%s"\n' "$worker"
    printf 'url = "ws://127.0.0.1:%s/ws/agent"\n' "$wss_port"
    if [[ -n "$auth_token" ]]; then
      printf 'auth_token = "%s"\n' "$auth_token"
    fi
    printf 'timeout_ms = %s\n' "$DEFAULT_EXTERNAL_WORKER_TIMEOUT_MS"
  } >>"$config_path"

  if [[ -z "$auth_token" ]]; then
    say "WARNING: GATEWAY_AUTH_TOKEN not found in $env_path;" \
        "'$worker' worker config written WITHOUT auth_token (connections will fail until set)"
  fi

  chown "$name:$name" "$config_path"
  chmod 600 "$config_path"
  say "wrote $worker external-worker config to $config_path (ws://127.0.0.1:$wss_port/ws/agent)"
}

# Remove one external-worker array entry plus any nested child tables. Preserve
# unrelated TOML text/comments, validate before and after, and retain one secure
# rollback copy. Returns success when no Nanocode entry exists.
remove_external_worker_config() {
  local name="$1" worker="$2" config_path temp_path backup_path rc=0
  config_path="$(tenant_state_dir "$name")/config.toml"
  [[ -f "$config_path" ]] || return 0
  [[ ! -L "$config_path" ]] || { say "refusing symlinked tenant config: $config_path" >&2; return 1; }
  require_cmd python3
  temp_path="$(mktemp "${config_path}.tmp.XXXXXX")"
  backup_path="${config_path}.pre-${worker}-retirement"

  python3 - "$config_path" "$temp_path" "$backup_path" "$worker" <<'PY' || rc=$?
import os
import re
import stat
import sys
import tomllib
from pathlib import Path

source, target, backup = map(Path, sys.argv[1:4])
worker = sys.argv[4]
source_fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW)
source_stat = os.fstat(source_fd)
if not stat.S_ISREG(source_stat.st_mode):
    os.close(source_fd)
    raise RuntimeError("tenant config is not a regular file")
with os.fdopen(source_fd, encoding="utf-8") as handle:
    text = handle.read()
tomllib.loads(text)
lines = text.splitlines(keepends=True)
root = "sandbox.external_workers"
header_re = re.compile(r"^\s*(\[\[?)([^\]]+)(\]\]?)\s*(?:#.*)?$")
name_re = re.compile(rf"^\s*name\s*=\s*(['\"]){re.escape(worker)}\1\s*(?:#.*)?$")

def header(line: str):
    match = header_re.match(line.rstrip("\r\n"))
    if not match or len(match.group(1)) != len(match.group(3)):
        return None
    return len(match.group(1)), "".join(match.group(2).split())

ranges = []
index = 0
while index < len(lines):
    info = header(lines[index])
    if info != (2, root):
        index += 1
        continue
    end = len(lines)
    scalar_end = len(lines)
    for cursor in range(index + 1, len(lines)):
        child = header(lines[cursor])
        if child is None:
            continue
        if scalar_end == len(lines):
            scalar_end = cursor
        if child[1] == root or not child[1].startswith(root + "."):
            end = cursor
            break
    if any(name_re.match(line.rstrip("\r\n")) for line in lines[index + 1:scalar_end]):
        start = index
        cursor = index - 1
        if cursor >= 0 and re.match(
            rf"^\s*#\s*External worker:\s*{re.escape(worker)}\b",
            lines[cursor],
            re.IGNORECASE,
        ):
            start = cursor
            if start > 0 and not lines[start - 1].strip():
                start -= 1
        ranges.append((start, end))
    index = end

if not ranges:
    raise SystemExit(3)

kept = []
cursor = 0
for start, end in ranges:
    kept.extend(lines[cursor:start])
    cursor = end
kept.extend(lines[cursor:])
result = "".join(kept)
parsed = tomllib.loads(result)
workers = parsed.get("sandbox", {}).get("external_workers", [])
if any(isinstance(item, dict) and item.get("name") == worker for item in workers):
    raise RuntimeError(f"failed to remove external worker {worker!r}")

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
    backup_stat = os.lstat(backup)
    if not stat.S_ISREG(backup_stat.st_mode):
        raise RuntimeError(f"unsafe retirement backup: {backup}")
else:
    write_secure(backup, text, os.O_WRONLY | os.O_CREAT | os.O_EXCL)
write_secure(target, result, os.O_WRONLY | os.O_TRUNC)
os.replace(target, source)
PY
  if [[ "$rc" -eq 3 ]]; then
    rm -f "$temp_path"
    return 0
  fi
  if [[ "$rc" -ne 0 ]]; then
    rm -f "$temp_path"
    say "failed to remove external worker '$worker' from $config_path" >&2
    return 1
  fi
  say "removed retired '$worker' route from $config_path"
}

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
      || { say "failed to reload tenant user manager after Nanocode retirement" >&2; return 1; }
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
owner_scope_tables() {
  printf '%s\n' \
    settings conversations memory_documents routines agent_jobs api_tokens \
    heartbeat_state reflex_patterns user_identities secrets wasm_tools \
    tool_rate_limit_state secret_usage_log leak_detection_events wasm_channels
}

# Print aggregate owner-scope counts as: <user_id><tab><row_count>
owner_scope_rows() {
  local name="$1"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  ensure_container_runtime

  local container_name="lunarwing-pg-$name"
  _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true \
    || die "PostgreSQL not running for '$name' — start the tenant's pg container first"

  local psql_cmd="psql -U lunarwing -d lunarwing"
  local selects="" sep="" tbl exists
  while IFS= read -r tbl; do
    exists="$(cd / && _ctr "$name" exec "$container_name" $psql_cmd -tAc \
      "SELECT 1 FROM information_schema.columns WHERE table_schema='public' AND table_name='$tbl' AND column_name='user_id'" 2>/dev/null || true)"
    [[ "$exists" == "1" ]] || continue
    selects+="${sep}SELECT user_id, count(*)::bigint AS row_count FROM $tbl GROUP BY user_id"
    sep=" UNION ALL "
  done < <(owner_scope_tables)

  [[ -n "$selects" ]] || return 0
  local sql
  sql="SELECT user_id, sum(row_count)::bigint FROM ($selects) s GROUP BY user_id ORDER BY user_id;"
  cd / && _ctr "$name" exec "$container_name" \
    psql -U lunarwing -d lunarwing -tA -F $'\t' -c "$sql"
}

# Check whether a tenant's DB has orphaned old-scope rows that need migration.
# Returns 0 (needs migration) or 1 (already clean / DB unreachable).
# Usage: _owner_scope_needs_migration <name> [old_scope]
_owner_scope_needs_migration() {
  local name="$1"
  local old_scope="${2:-default}"
  # Confirm a postgres port is allocated (tenant is provisioned); we do NOT connect
  # over it — psql runs INSIDE the pg container via the local socket (see the note in
  # migrate_owner_scope). A host-published port isn't reachable from inside the
  # container under rootless podman.
  ports_get "$name" postgres >/dev/null 2>&1 || return 1

  # If the PG container isn't running, can't check — assume clean.
  _ctr "$name" inspect -f '{{.State.Running}}' "lunarwing-pg-$name" 2>/dev/null | grep -q true || return 1

  local count
  count="$(cd / && _ctr "$name" exec lunarwing-pg-$name \
    psql -U lunarwing -d lunarwing -tAc "
    SELECT count(*) FROM (
      SELECT user_id FROM settings WHERE user_id='$old_scope'
      UNION ALL SELECT user_id FROM conversations WHERE user_id='$old_scope'
      UNION ALL SELECT user_id FROM memory_documents WHERE user_id='$old_scope'
      UNION ALL SELECT user_id FROM secrets WHERE user_id='$old_scope'
      UNION ALL SELECT user_id FROM agent_jobs WHERE user_id='$old_scope'
    ) AS t;" 2>/dev/null || echo 0)"

  [[ "$count" -gt 0 ]] 2>/dev/null
}

# Usage: migrate_owner_scope <name> [--from <old_scope>]
migrate_owner_scope() {
  local name="$1"
  local old_scope="${2:-default}"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"

  # Self-wipe guard: if the source scope already equals the target tenant name there
  # is nothing to rekey — and proceeding would be CATASTROPHIC. The collision dedup
  # DELETEs self-join the table (DELETE ... d USING ... t WHERE d.user_id=old_scope
  # AND t.user_id=name AND <key match>); when old_scope==name every row matches
  # itself and is deleted before the no-op UPDATE, wiping the table. The default-only
  # auto-callers never reach this (old_scope='default'), but an explicit
  # `--from <name>` (or a future scope-detection caller) could.
  if [[ "$old_scope" == "$name" ]]; then
    say "owner scope '$name' already matches the tenant; nothing to rekey"
    return 0
  fi

  local pg_port
  pg_port="$(ports_get "$name" postgres)" || die "no postgres port for $name"

  # Quick check: are there any old-scope rows at all?
  if ! _owner_scope_needs_migration "$name" "$old_scope"; then
    say "no '$old_scope' rows found for $name; owner scope already clean"
    return 0
  fi

  say "migrating owner scope: '$old_scope' -> '$name' (pg port $pg_port)"

  # Tables with a user_id column (base tables only, not views).
  # Discovered via information_schema — kept as a static list so the migration
  # is deterministic and doesn't break if a view is added/renamed.
  local tables
  tables="$(owner_scope_tables)"

  # Stop the daemon first so it doesn't re-create 'default' rows mid-migration.
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    _systemctl_user "$name" stop "lunarwing-${name}.service" 2>/dev/null || true
  else
    rc-service "lunarwing-${name}" stop >/dev/null 2>&1 || true
  fi

  # Connect over the container's LOCAL SOCKET — psql runs INSIDE the pg container via
  # `_ctr exec`, so NOT `-h 127.0.0.1 -p $pg_port`: that host-published port is not
  # reachable from inside the container under rootless podman, which silently broke
  # the rekey (the needs-check saw 0 rows -> "already clean" no-op, so a real
  # migration never ran). Mirrors restore_tenant_postgres (pg_restore -U lunarwing
  # -d lunarwing, no -h/-p). Do not add -h/-p back.
  local psql_cmd
  psql_cmd="psql -U lunarwing -d lunarwing"

  # Run the migration via the tenant's PG container. Each table's statements run
  # in ONE transaction (-1) with ON_ERROR_STOP, so a unique-constraint collision
  # or FK error rolls that table back atomically instead of half-applying — and
  # the error is SURFACED and counted (not swallowed) rather than aborting the
  # whole migration. Every table with a UNIQUE/PK on (user_id, ...) first deletes
  # the old-scope rows that would collide with an existing tenant-scoped row
  # (keeping the tenant row; memory_documents also salvages content into an empty
  # tenant row first), then updates. Tables with no user_id-bearing unique key get
  # a straight update.
  local total_migrated=0 migrate_errors=0
  for tbl in $tables; do
    # Check if the table exists in this DB (some may not if migrations haven't run).
    local exists
    exists="$(cd / && _ctr "$name" exec lunarwing-pg-$name $psql_cmd -tAc \
      "SELECT 1 FROM information_schema.tables WHERE table_name='$tbl' AND table_schema='public'" 2>/dev/null || true)"
    [[ "$exists" == "1" ]] || continue

    local sql
    case "$tbl" in
      settings)  # PRIMARY KEY (user_id, key)
        sql="DELETE FROM settings d USING settings t
               WHERE d.user_id='$old_scope' AND t.user_id='$name' AND d.key=t.key;
             UPDATE settings SET user_id='$name' WHERE user_id='$old_scope';" ;;
      memory_documents)  # UNIQUE (user_id, path, agent_id) NULLS NOT DISTINCT (V21)
        sql="UPDATE memory_documents t SET content = d.content
               FROM memory_documents d
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.path=t.path AND (d.agent_id IS NOT DISTINCT FROM t.agent_id)
                 AND (t.content IS NULL OR t.content = '');
             DELETE FROM memory_documents d USING memory_documents t
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.path=t.path AND (d.agent_id IS NOT DISTINCT FROM t.agent_id);
             UPDATE memory_documents SET user_id='$name' WHERE user_id='$old_scope';" ;;
      secrets)  # UNIQUE (user_id, name)
        sql="DELETE FROM secrets d USING secrets t
               WHERE d.user_id='$old_scope' AND t.user_id='$name' AND d.name=t.name;
             UPDATE secrets SET user_id='$name' WHERE user_id='$old_scope';" ;;
      wasm_tools)  # UNIQUE (user_id, name, version)
        sql="DELETE FROM wasm_tools d USING wasm_tools t
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.name=t.name AND d.version=t.version;
             UPDATE wasm_tools SET user_id='$name' WHERE user_id='$old_scope';" ;;
      routines)  # UNIQUE (user_id, name)
        sql="DELETE FROM routines d USING routines t
               WHERE d.user_id='$old_scope' AND t.user_id='$name' AND d.name=t.name;
             UPDATE routines SET user_id='$name' WHERE user_id='$old_scope';" ;;
      reflex_patterns)  # UNIQUE (user_id, normalized_pattern)
        sql="DELETE FROM reflex_patterns d USING reflex_patterns t
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.normalized_pattern=t.normalized_pattern;
             UPDATE reflex_patterns SET user_id='$name' WHERE user_id='$old_scope';" ;;
      wasm_channels)  # UNIQUE (user_id, name)
        sql="DELETE FROM wasm_channels d USING wasm_channels t
               WHERE d.user_id='$old_scope' AND t.user_id='$name' AND d.name=t.name;
             UPDATE wasm_channels SET user_id='$name' WHERE user_id='$old_scope';" ;;
      heartbeat_state)  # UNIQUE (user_id, agent_id) — plain (NULLs distinct), so a
                        # NULL agent_id never collides; only dedup non-NULL matches.
        sql="DELETE FROM heartbeat_state d USING heartbeat_state t
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.agent_id IS NOT NULL AND d.agent_id = t.agent_id;
             UPDATE heartbeat_state SET user_id='$name' WHERE user_id='$old_scope';" ;;
      tool_rate_limit_state)  # UNIQUE (wasm_tool_id, user_id)
        sql="DELETE FROM tool_rate_limit_state d USING tool_rate_limit_state t
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.wasm_tool_id=t.wasm_tool_id;
             UPDATE tool_rate_limit_state SET user_id='$name' WHERE user_id='$old_scope';" ;;
      conversations)  # partial UNIQUE idx (user_id, routine_id) + (user_id) heartbeat singleton (V11)
        sql="DELETE FROM conversations d USING conversations t
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.metadata->>'routine_id' IS NOT NULL
                 AND d.metadata->>'routine_id' = t.metadata->>'routine_id';
             DELETE FROM conversations d USING conversations t
               WHERE d.user_id='$old_scope' AND t.user_id='$name'
                 AND d.metadata->>'thread_type'='heartbeat'
                 AND t.metadata->>'thread_type'='heartbeat';
             UPDATE conversations SET user_id='$name' WHERE user_id='$old_scope';" ;;
      *)  # agent_jobs, api_tokens, user_identities, secret_usage_log, leak_detection_events: no user_id-bearing unique key
        sql="UPDATE $tbl SET user_id='$name' WHERE user_id='$old_scope';" ;;
    esac

    local out rc=0
    out="$(cd / && _ctr "$name" exec lunarwing-pg-$name \
      $psql_cmd -1 -v ON_ERROR_STOP=1 -c "$sql" 2>&1)" || rc=$?
    if [[ "$rc" -ne 0 ]]; then
      say "  WARNING: $tbl rekey failed (rc=$rc): ${out//$'\n'/ }"
      migrate_errors=$((migrate_errors + 1))
    fi

    local count
    count="$(cd / && _ctr "$name" exec lunarwing-pg-$name $psql_cmd -tAc \
      "SELECT count(*) FROM $tbl WHERE user_id='$name'" 2>/dev/null || echo 0)"
    say "  $tbl: $count rows now scoped to '$name'"
    total_migrated=$((total_migrated + count))
  done

  # Verify no old-scope rows remain — check ALL owner-scoped tables (not just 5)
  # and name the offenders so a swallowed collision can't hide a partial rekey.
  local remaining_total=0 offenders="" unchecked=""
  for tbl in $tables; do
    local exists2 r
    exists2="$(cd / && _ctr "$name" exec lunarwing-pg-$name $psql_cmd -tAc \
      "SELECT 1 FROM information_schema.tables WHERE table_name='$tbl' AND table_schema='public'" 2>/dev/null || true)"
    [[ "$exists2" == "1" ]] || continue
    r="$(cd / && _ctr "$name" exec lunarwing-pg-$name $psql_cmd -tAc \
      "SELECT count(*) FROM $tbl WHERE user_id='$old_scope'" 2>/dev/null || echo "?")"
    if [[ "$r" =~ ^[0-9]+$ ]]; then
      [[ "$r" -gt 0 ]] && { offenders+=" $tbl($r)"; remaining_total=$((remaining_total + r)); }
    else
      unchecked+=" $tbl"
    fi
  done

  say ""
  [[ "$migrate_errors" -gt 0 ]] && \
    say "WARNING: $migrate_errors table(s) errored during rekey — see the WARNINGs above"
  if [[ -n "$offenders" ]]; then
    say "WARNING: $remaining_total '$old_scope' row(s) still remain after migration:$offenders"
    say "  (usually a unique-constraint collision or a users-table FK gap — inspect before starting)"
  elif [[ -n "$unchecked" ]]; then
    say "migration done; could not re-verify:$unchecked (DB read failed)"
  else
    say "migration complete: no '$old_scope' rows remain in any owner-scoped table"
  fi
  [[ "$migrate_errors" -eq 0 && -z "$offenders" && -z "$unchecked" ]] || \
    say "review the WARNINGs above before running 'start-tenant'"
  say "restart the tenant: $0 start-tenant $name"
}

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

configure_pebble() {
  local name="$1"
  local nanogpt_api_key="$2"
  local model="$3"

  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"

  local env_dir env_path
  env_dir="$(tenant_env_dir "$name")"
  env_path="$env_dir/pebble.env"

  mkdir -p "$env_dir"

  : >"$env_path"

  if [[ -n "$nanogpt_api_key" ]]; then
    printf 'NANOGPT_API_KEY=%s\n' "$nanogpt_api_key" >>"$env_path"
  fi

  if [[ -n "$model" ]]; then
    printf 'PEBBLE_MODEL=%s\n' "$model" >>"$env_path"
  fi

  chown "$name:$name" "$env_path"
  chmod 600 "$env_path"
  say "pebble configured for tenant '$name' at $env_path"

  local container_name="lunarwing-pebble-$name"
  if _ctr "$name" inspect "$container_name" &>/dev/null 2>&1; then
    say "note: restart the pebble worker to pick up new config:"
    say "  sudo $0 stop-tenant $name && sudo $0 start-tenant $name"
  fi
}

# ── OpenCode worker container ──────────────────────────────────────────────────

configure_opencode() {
  local name="$1"
  local model="${2:-}"
  local base_url="${3:-}"

  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  [[ -n "$model" || -n "$base_url" ]] \
    || die "configure-opencode: pass --model <model> and/or --base-url <url>"

  local env_path
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$env_path" ]] || die "env file not found: $env_path (run add-tenant first)"

  local tmp
  tmp="$(mktemp)"
  grep -v -e '^OPENCODE_MODEL=' -e '^OPENCODE_BASE_URL=' "$env_path" >"$tmp" || true
  [[ -n "$model" ]]    && printf 'OPENCODE_MODEL=%s\n'    "$model"    >>"$tmp"
  [[ -n "$base_url" ]] && printf 'OPENCODE_BASE_URL=%s\n' "$base_url" >>"$tmp"
  cat "$tmp" >"$env_path"
  rm -f "$tmp"
  chown "$name:$name" "$env_path"
  chmod 600 "$env_path"
  say "opencode LLM overrides written to $env_path for tenant '$name'"

  local container_name="lunarwing-opencode-$name"
  if _ctr "$name" inspect "$container_name" &>/dev/null 2>&1; then
    say "note: restart the opencode worker to pick up the new config:"
    say "  sudo $0 stop-tenant $name && sudo $0 start-tenant $name"
  fi
}

snapshot_sqlite_database() {  # <source> <destination>
  require_cmd python3
  python3 - "$1" "$2" <<'PY'
import sqlite3
import sys
from pathlib import Path

source_path = Path(sys.argv[1]).resolve(strict=True)
destination_path = Path(sys.argv[2])
source = sqlite3.connect(f"{source_path.as_uri()}?mode=ro", uri=True)
destination = sqlite3.connect(destination_path)
try:
    source.backup(destination)
    result = destination.execute("PRAGMA quick_check").fetchall()
    if result != [("ok",)]:
        raise RuntimeError(f"SQLite quick_check failed: {result}")
finally:
    destination.close()
    source.close()
PY
}

initialize_study_notes_state() {  # <tenant> <code home> <state home>
  local name="$1" home="$2" state_home="$3"
  validate_study_notes_path "$state_home" "Study Notes state home"

  local home_real state_real
  home_real="$(readlink -f -- "$home")"
  state_real="$(readlink -m -- "$state_home")"
  [[ "$state_real" != "$home_real" && "$state_real" != "$home_real/"* \
      && "$home_real" != "$state_real/"* ]] \
    || die "Study Notes code and state directories must not overlap"

  if [[ -e "$state_home" ]]; then
    [[ -d "$state_home" ]] || die "Study Notes state home is not a directory: $state_home"
    if [[ -e "$state_home/agent-notes.sqlite3" \
        || -e "$state_home/agent-notes-by-agent" ]]; then
      validate_study_notes_state_home "$state_home"
      return 0
    fi
    [[ -z "$(find "$state_home" -mindepth 1 -maxdepth 1 -print -quit)" ]] \
      || die "Study Notes state home is non-empty but uninitialized: $state_home"
  fi

  local state_parent staged
  state_parent="$(dirname -- "$state_home")"
  mkdir -p "$state_parent"
  staged="$(mktemp -d "$state_parent/.study-notes-state.XXXXXX")"
  if ! snapshot_sqlite_database \
      "$home/agent-notes.sqlite3" "$staged/agent-notes.sqlite3" \
      || ! chmod u+rw,go-rwx "$staged/agent-notes.sqlite3" \
      || ! cp -a "$home/agent-notes-by-agent" "$staged/agent-notes-by-agent" \
      || ! chown -R "$name:$name" "$staged" \
      || ! chmod u+rwx "$staged" \
      || ! find "$staged/agent-notes-by-agent" -type d -exec chmod u+rwx {} +; then
    rm -rf -- "$staged"
    die "failed to seed Study Notes state: $state_home"
  fi
  validate_study_notes_state_home "$staged"
  if [[ -d "$state_home" ]] && ! rmdir -- "$state_home"; then
    rm -rf -- "$staged"
    die "Study Notes state home changed while seeding: $state_home"
  fi
  if ! mv -T -- "$staged" "$state_home"; then
    rm -rf -- "$staged"
    die "failed to install seeded Study Notes state: $state_home"
  fi
}

validate_study_notes_tenant_access() {  # <tenant> <code home> [state home] <write bool>
  local name="$1" home="$2" state_home="$3" allow_write="$4"
  local required
  for required in \
    "$home/study_notes_mcp.py" \
    "$home/memorydb.py" \
    "$home/agent-notes.sqlite3" \
    "$home/skills/study-notes/SKILL.md"; do
    sudo -u "$name" test -r "$required" \
      || die "tenant '$name' cannot read Study Notes file: $required"
  done
  sudo -u "$name" test -x "$home/agent-notes-by-agent" \
    || die "tenant '$name' cannot traverse Study Notes directory: $home/agent-notes-by-agent"

  [[ -n "$state_home" ]] || return 0
  sudo -u "$name" test -r "$state_home/agent-notes.sqlite3" \
    || die "tenant '$name' cannot read Study Notes state database"
  sudo -u "$name" test -x "$state_home/agent-notes-by-agent" \
    || die "tenant '$name' cannot traverse Study Notes state notes"
  [[ "$allow_write" == "true" ]] || return 0
  sudo -u "$name" test -w "$state_home" \
    || die "tenant '$name' cannot write Study Notes state home: $state_home"
  local unwritable
  unwritable="$(sudo -u "$name" find "$state_home/agent-notes-by-agent" \
    -type d ! -writable -print -quit)"
  [[ -z "$unwritable" ]] \
    || die "tenant '$name' cannot write Study Notes state directory: $unwritable"
  if [[ -e "$state_home/agent-notes-by-agent/.study-notes.lock" ]]; then
    sudo -u "$name" test -w "$state_home/agent-notes-by-agent/.study-notes.lock" \
      || die "tenant '$name' cannot write Study Notes lock file"
  fi
}

configure_study_mcp() {  # <tenant> <code home> <allow-write bool> [state home]
  local name="$1" home="$2" allow_write_raw="$3" state_home="${4:-}"

  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  validate_study_notes_home "$home"

  local allow_write=false
  if study_notes_write_enabled "$allow_write_raw"; then
    allow_write=true
  fi
  if [[ -z "$state_home" ]]; then
    state_home="$(tenant_study_notes_state_home "$name")"
  fi
  if [[ "$allow_write" == "true" && -z "$state_home" ]]; then
    state_home="$(tenant_state_dir "$name")/study-notes"
  fi
  if [[ -n "$state_home" ]]; then
    initialize_study_notes_state "$name" "$home" "$state_home"
  fi
  validate_study_notes_tenant_access "$name" "$home" "$state_home" "$allow_write"

  # Rootless Podman cannot relabel a root-owned shared checkout itself.
  if command -v selinuxenabled >/dev/null 2>&1 && selinuxenabled; then
    require_cmd chcon
    chcon -R -t container_file_t -- "$home" \
      || die "failed to label Study Notes checkout for container access: $home"
    if [[ -n "$state_home" ]]; then
      chcon -R -t container_file_t -- "$state_home" \
        || die "failed to label Study Notes state for container access: $state_home"
    fi
  fi

  local env_path tmp
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$env_path" ]] || die "env file not found: $env_path (run add-tenant first)"
  tmp="$(mktemp)"
  grep -vE '^(# Shared .*Study Notes MCP|LW2STUDYMCP_(HOME|STATE_HOME|ALLOW_WRITE)=)' \
    "$env_path" > "$tmp" || true
  printf '\n# Shared LunarWing Study Notes MCP\nLW2STUDYMCP_HOME=%s\n' \
    "$home" >> "$tmp"
  [[ -z "$state_home" ]] \
    || printf 'LW2STUDYMCP_STATE_HOME=%s\n' "$state_home" >> "$tmp"
  [[ "$allow_write" != "true" ]] \
    || printf 'LW2STUDYMCP_ALLOW_WRITE=1\n' >> "$tmp"
  cat "$tmp" > "$env_path"
  rm -f "$tmp"
  chown "$name:$name" "$env_path"
  chmod 600 "$env_path"

  say "Study Notes MCP configured for tenant '$name' from $home (write=$allow_write)"
  say "restart tenant to seed core MCP config and recreate opencode worker mount:"
  say "  sudo $0 restart-tenant $name"
}

start_tenant_opencode() {
  local name="$1"
  ensure_container_runtime

  local opencode_launch_contract=""
  local -a opencode_userns_args=()
  if [[ "$CONTAINER_RT" == "podman" && "$MT_ROOTLESS" == "true" ]]; then
    opencode_launch_contract="$OPENCODE_ROOTLESS_LAUNCH_CONTRACT"
    opencode_userns_args=(--userns "$OPENCODE_USERNS")
  fi

  local wss_port container_name opencode_dir
  # `|| true`: ports_get returns nonzero for a genuinely unallocated key; under
  # `set -euo pipefail` a bare assignment would abort the whole start-tenant run
  # before the `[[ -z "$wss_port" ]]` skip below (mirrors start_tenant_vision).
  wss_port="$(ports_get "$name" opencode_wss)" || true
  container_name="lunarwing-opencode-$name"
  opencode_dir="${LUNARWING_ROOT}/opencode4lunarwing"

  if [[ -z "$wss_port" ]]; then
    say "no opencode_wss port allocated for $name (skipping opencode worker)"
    return 0
  fi

  if ! _ensure_tenant_image "$name" lunarwing-worker-opencode:latest; then
    say "opencode worker image not available; run 'build-opencode-worker' first (skipping)"
    return 0
  fi
  local image_user
  image_user="$(_ctr "$name" image inspect --format '{{.Config.User}}' \
    lunarwing-worker-opencode:latest 2>/dev/null || true)"
  case "$image_user" in
    "$OPENCODE_CONTAINER_UID"|"$OPENCODE_CONTAINER_UID:$OPENCODE_CONTAINER_GID") ;;
    *)
      die "opencode worker image has user '${image_user:-unset}', expected $OPENCODE_CONTAINER_UID:$OPENCODE_CONTAINER_GID; rebuild it"
      ;;
  esac

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _wait_user_manager "$name"
    local opencode_service="lunarwing-opencode-${name}.service"
    local quadlet_file
    quadlet_file="$(tenant_quadlet_dir "$name")/lunarwing-opencode-${name}.container"
    render_worker_quadlet "$name" opencode 8443
    _systemctl_user "$name" daemon-reload 2>/dev/null || true
    if _container_config_changed "$name" "lunarwing-opencode-${name}" "$quadlet_file"; then
      _recreate_quadlet_container "$name" "$opencode_service" "lunarwing-opencode-${name}"
    fi
    # A bad pre-upgrade image can exhaust StartLimitBurst. Loading a fixed image
    # does not clear that systemd lockout, so reset it before the explicit retry.
    _systemctl_user "$name" reset-failed "$opencode_service" >/dev/null 2>&1 || true
    if _systemctl_user "$name" start "$opencode_service" >/dev/null 2>&1; then
      _store_container_hash "$name" "lunarwing-opencode-${name}" "$quadlet_file"
      say "opencode worker ready via quadlet ($opencode_service, WSS port $wss_port)"
    else
      say "WARNING: $opencode_service failed to start" >&2
      _systemctl_user "$name" status "$opencode_service" --no-pager --full >&2 || true
    fi
    return 0
  fi

  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    local env_file_for_hash
    env_file_for_hash="$(tenant_env_dir "$name")/lunarwing.env"
    if _container_config_changed \
        "$name" "$container_name" "$env_file_for_hash" "$opencode_launch_contract"; then
      say "config changed for $container_name; force-recreating"
      _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
      _ctr "$name" rm -f "$container_name" >/dev/null 2>&1 || true
    elif _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
      say "opencode worker already running ($container_name, WSS port $wss_port)"
      _store_container_hash \
        "$name" "$container_name" "$env_file_for_hash" "$opencode_launch_contract"
      _register_worker_unit "$name" opencode
      return 0
    else
      say "starting existing opencode worker container $container_name"
      _ctr "$name" start "$container_name" >/dev/null
      _store_container_hash \
        "$name" "$container_name" "$env_file_for_hash" "$opencode_launch_contract"
      _register_worker_unit "$name" opencode
      say "opencode worker ready ($container_name, WSS port $wss_port)"
      return 0
    fi
  fi

  if ! _ctr "$name" inspect "$container_name" &>/dev/null; then
    say "creating opencode worker container $container_name on WSS port $wss_port"

    local tenant_env_path
    tenant_env_path="$(tenant_env_dir "$name")/lunarwing.env"

    local opencode_env_path
    opencode_env_path="$(tenant_env_dir "$name")/opencode.env"

    local env_flags=()
    if [[ -f "$tenant_env_path" ]]; then
      local gateway_token
      gateway_token="$(_read_env_value "$tenant_env_path" GATEWAY_AUTH_TOKEN)"
      [[ -n "$gateway_token" ]] && env_flags+=(-e "AGENT_AUTH_TOKEN=$gateway_token")

      local llm_api_key
      llm_api_key="$(_read_env_value "$tenant_env_path" LLM_API_KEY)"
      [[ -n "$llm_api_key" ]] && env_flags+=(-e "TENSORZERO_API_KEY=$llm_api_key")

      local opencode_model opencode_base_url
      opencode_model="$(_read_env_value "$tenant_env_path" OPENCODE_MODEL)"
      [[ -n "$opencode_model" ]] && env_flags+=(-e "OPENCODE_MODEL=$opencode_model")
      opencode_base_url="$(_read_env_value "$tenant_env_path" OPENCODE_BASE_URL)"
      [[ -n "$opencode_base_url" ]] && env_flags+=(-e "OPENCODE_BASE_URL=$opencode_base_url")
    fi

    if [[ -f "$opencode_env_path" ]]; then
      env_flags+=(--env-file "$opencode_env_path")
    fi

    local study_notes_home study_notes_state_home study_notes_allow_write
    local -a study_notes_mount=()
    study_notes_home="$(tenant_study_notes_home "$name")"
    if [[ -n "$study_notes_home" ]]; then
      validate_study_notes_home "$study_notes_home"
      study_notes_mount=(
        -v "${study_notes_home}:${STUDY_NOTES_CONTAINER_HOME}:ro,z"
        -e "LW2STUDYMCP_HOME=${STUDY_NOTES_CONTAINER_HOME}"
      )
      study_notes_state_home="$(tenant_study_notes_state_home "$name")"
      study_notes_allow_write="$(tenant_study_notes_allow_write "$name")"
      if study_notes_write_enabled "$study_notes_allow_write"; then
        [[ -n "$study_notes_state_home" ]] \
          || die "Study Notes write mode requires LW2STUDYMCP_STATE_HOME"
      fi
      if [[ -n "$study_notes_state_home" ]]; then
        validate_study_notes_state_home "$study_notes_state_home"
        study_notes_mount+=(
          -v "${study_notes_state_home}:${STUDY_NOTES_CONTAINER_STATE_HOME}:ro,z"
          -e "LW2STUDYMCP_STATE_HOME=${STUDY_NOTES_CONTAINER_STATE_HOME}"
        )
      fi
    fi

    local workspace_dir
    workspace_dir="$(prepare_worker_workspace "$name" opencode)"

    local -a restart_arg=()
    [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)
    local -a health_publish=()
    local host_health_port
    host_health_port="$(ports_get "$name" opencode_health)" || true
    [[ -n "$host_health_port" ]] && health_publish=(-p "127.0.0.1:${host_health_port}:8443")
    local ssh_agent_socket
    ssh_agent_socket="$(tenant_run_dir "$name")/ssh-agent.sock"
    local -a ssh_mount=(-v "${ssh_agent_socket}:/tmp/ssh-agent.sock:z" -e SSH_AUTH_SOCK=/tmp/ssh-agent.sock)
    _ctr "$name" run -d \
      --name "$container_name" \
      "${opencode_userns_args[@]}" \
      -e LUNARWING_WORKER_ID="worker-opencode-${name}" \
      -e WS_PORT="$wss_port" \
      -e HEALTH_PORT="8443" \
      -e OPENCODE_MODE=websocket \
      -e WS_ROLE=server \
      -e WS_BIND_HOST=0.0.0.0 \
      -e WS_PATH=/ws/agent \
      "${env_flags[@]}" \
      "${ssh_mount[@]}" \
      "${study_notes_mount[@]}" \
      -p "127.0.0.1:${wss_port}:${wss_port}" \
      "${health_publish[@]}" \
      -v "$workspace_dir:/workspace:z" \
      "${restart_arg[@]}" \
      lunarwing-worker-opencode:latest \
      --mode websocket >/dev/null
    _store_container_hash \
      "$name" "$container_name" "$(tenant_env_dir "$name")/lunarwing.env" \
      "$opencode_launch_contract"
  fi

  _register_worker_unit "$name" opencode
  say "opencode worker ready ($container_name, WSS port $wss_port)"
}

stop_tenant_opencode() {
  local name="$1"
  ensure_container_runtime

  local container_name="lunarwing-opencode-$name"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" && -f "/etc/init.d/${container_name}" ]]; then
    _deregister_babysitter "$container_name"
    rc-service "$container_name" stop >/dev/null 2>&1 || true
    say "opencode worker stopped ($container_name)"
  elif _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
    say "opencode worker stopped ($container_name)"
  fi
}

# ── Kimi Code worker container ────────────────────────────────────────────────

prepare_worker_workspace() {
  local name="$1" worker="$2" workspace_dir container_uid container_gid
  workspace_dir="$(tenant_lw_root "$name")/${worker}-workspace"
  mkdir -p "$workspace_dir"

  case "$worker" in
    opencode)
      container_uid="$OPENCODE_CONTAINER_UID"
      container_gid="$OPENCODE_CONTAINER_GID"
      ;;
    kimi)
      container_uid="$KIMI_CONTAINER_UID"
      container_gid="$KIMI_CONTAINER_GID"
      ;;
    *) die "unsupported private worker workspace: $worker" ;;
  esac

  if [[ "$MT_ROOTLESS" != "true" ]]; then
    # Rootful runtimes do not apply keep-id, so bind mounts must use image IDs.
    chown -R "$container_uid:$container_gid" "$workspace_dir"
  else
    chown -R "$name:$name" "$workspace_dir"
  fi
  chmod 2770 "$workspace_dir"
  printf '%s\n' "$workspace_dir"
}

write_kimi_runtime_env() {
  local name="$1" source_path target_path temporary key value
  source_path="$(tenant_env_dir "$name")/lunarwing.env"
  target_path="$(tenant_env_dir "$name")/kimi-runtime.env"
  temporary="$(mktemp "${target_path}.tmp.XXXXXX")"
  chmod 600 "$temporary"

  for key in KIMI_MODEL KIMI_BASE_URL KIMI_API_KEY; do
    value="$(_read_env_value "$source_path" "$key")"
    [[ "$value" != *$'\n'* && "$value" != *$'\r'* ]] \
      || { rm -f "$temporary"; die "$key cannot contain a newline"; }
    [[ -z "$value" ]] || printf '%s=%s\n' "$key" "$value" >>"$temporary"
  done

  value="$(_read_env_value "$source_path" GATEWAY_AUTH_TOKEN)"
  [[ -n "$value" ]] \
    || { rm -f "$temporary"; die "Kimi worker requires GATEWAY_AUTH_TOKEN"; }
  # Worker bridge expects AGENT_AUTH_TOKEN, not daemon's gateway variable name.
  printf 'AGENT_AUTH_TOKEN=%s\n' "$value" >>"$temporary"
  chown "$name:$name" "$temporary"
  chmod 600 "$temporary"
  mv -f "$temporary" "$target_path"
  printf '%s\n' "$target_path"
}

configure_kimi() {
  local name="$1"
  local model="${2:-}"
  local base_url="${3:-}"
  local api_key="${4:-}"

  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  [[ -n "$model" || -n "$base_url" || -n "$api_key" ]] \
    || die "configure-kimi: pass --model <model>, --base-url <url>, or --api-key-file <path>"

  local env_path
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$env_path" ]] || die "env file not found: $env_path (run add-tenant first)"

  local tmp existing_model existing_base_url existing_api_key
  existing_model="$(_read_env_value "$env_path" KIMI_MODEL)"
  existing_base_url="$(_read_env_value "$env_path" KIMI_BASE_URL)"
  existing_api_key="$(_read_env_value "$env_path" KIMI_API_KEY)"
  [[ -n "$model" ]] || model="$existing_model"
  [[ -n "$base_url" ]] || base_url="$existing_base_url"
  [[ -n "$api_key" ]] || api_key="$existing_api_key"
  tmp="$(mktemp)"
  grep -v -e '^KIMI_MODEL=' -e '^KIMI_BASE_URL=' -e '^KIMI_API_KEY=' "$env_path" >"$tmp" || true
  [[ -n "$model" ]]    && printf 'KIMI_MODEL=%s\n'    "$model"    >>"$tmp"
  [[ -n "$base_url" ]] && printf 'KIMI_BASE_URL=%s\n' "$base_url" >>"$tmp"
  [[ -n "$api_key" ]] \
    && printf 'KIMI_API_KEY=%s\n' "$(_quote_env_value "$api_key")" >>"$tmp"
  cat "$tmp" >"$env_path"
  rm -f "$tmp"
  chown "$name:$name" "$env_path"
  chmod 600 "$env_path"
  say "kimi LLM overrides written to $env_path for tenant '$name'"

  local container_name="lunarwing-kimi-$name"
  if _ctr "$name" inspect "$container_name" &>/dev/null 2>&1; then
    say "note: restart the kimi worker to pick up the new config:"
    say "  sudo $0 stop-tenant $name && sudo $0 start-tenant $name"
  fi
}

start_tenant_kimi() {
  local name="$1"
  ensure_container_runtime

  local kimi_launch_contract=""
  local -a kimi_userns_args=()
  if [[ "$CONTAINER_RT" == "podman" && "$MT_ROOTLESS" == "true" ]]; then
    kimi_launch_contract="$KIMI_ROOTLESS_LAUNCH_CONTRACT"
    kimi_userns_args=(--userns "$KIMI_USERNS")
  fi

  local wss_port container_name kimi_dir
  wss_port="$(ports_get "$name" kimi_wss)" || true
  container_name="lunarwing-kimi-$name"
  kimi_dir="${LUNARWING_ROOT}/kimicode4lunarwing"

  if [[ -z "$wss_port" ]]; then
    say "no kimi_wss port allocated for $name (skipping kimi worker)"
    return 0
  fi

  if ! _ensure_tenant_image "$name" lunarwing-worker-kimi:latest; then
    say "kimi worker image not available; run 'build-kimi-worker' first (skipping)"
    return 0
  fi
  local image_user
  image_user="$(_ctr "$name" image inspect --format '{{.Config.User}}' \
    lunarwing-worker-kimi:latest 2>/dev/null || true)"
  case "$image_user" in
    "$KIMI_CONTAINER_UID"|"$KIMI_CONTAINER_UID:$KIMI_CONTAINER_GID") ;;
    *)
      die "kimi worker image has user '${image_user:-unset}', expected $KIMI_CONTAINER_UID:$KIMI_CONTAINER_GID; rebuild it"
      ;;
  esac

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _wait_user_manager "$name"
    local kimi_service="lunarwing-kimi-${name}.service"
    local quadlet_file
    quadlet_file="$(tenant_quadlet_dir "$name")/lunarwing-kimi-${name}.container"
    render_worker_quadlet "$name" kimi 8443
    _systemctl_user "$name" daemon-reload 2>/dev/null || true
    if _container_config_changed "$name" "lunarwing-kimi-${name}" "$quadlet_file"; then
      _recreate_quadlet_container "$name" "$kimi_service" "lunarwing-kimi-${name}"
    fi
    _systemctl_user "$name" reset-failed "$kimi_service" >/dev/null 2>&1 || true
    if _systemctl_user "$name" start "$kimi_service" >/dev/null 2>&1; then
      _store_container_hash "$name" "lunarwing-kimi-${name}" "$quadlet_file"
      say "kimi worker ready via quadlet ($kimi_service, WSS port $wss_port)"
    else
      say "WARNING: $kimi_service failed to start" >&2
      _systemctl_user "$name" status "$kimi_service" --no-pager --full >&2 || true
    fi
    return 0
  fi

  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    local env_file_for_hash
    env_file_for_hash="$(tenant_env_dir "$name")/lunarwing.env"
    if _container_config_changed \
        "$name" "$container_name" "$env_file_for_hash" "$kimi_launch_contract"; then
      say "config changed for $container_name; force-recreating"
      _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
      _ctr "$name" rm -f "$container_name" >/dev/null 2>&1 || true
    elif _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
      say "kimi worker already running ($container_name, WSS port $wss_port)"
      _store_container_hash \
        "$name" "$container_name" "$env_file_for_hash" "$kimi_launch_contract"
      _register_worker_unit "$name" kimi
      return 0
    else
      say "starting existing kimi worker container $container_name"
      _ctr "$name" start "$container_name" >/dev/null
      _store_container_hash \
        "$name" "$container_name" "$env_file_for_hash" "$kimi_launch_contract"
      _register_worker_unit "$name" kimi
      say "kimi worker ready ($container_name, WSS port $wss_port)"
      return 0
    fi
  fi

  if ! _ctr "$name" inspect "$container_name" &>/dev/null; then
    say "creating kimi worker container $container_name on WSS port $wss_port"

    local kimi_env_path
    kimi_env_path="$(tenant_env_dir "$name")/kimi.env"

    local runtime_env_path
    runtime_env_path="$(write_kimi_runtime_env "$name")"
    local env_flags=(--env-file "$runtime_env_path")

    if [[ -f "$kimi_env_path" ]]; then
      env_flags+=(--env-file "$kimi_env_path")
    fi

    local workspace_dir
    workspace_dir="$(prepare_worker_workspace "$name" kimi)"

    local -a restart_arg=()
    [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)
    local -a health_publish=()
    local host_health_port
    host_health_port="$(ports_get "$name" kimi_health)" || true
    [[ -n "$host_health_port" ]] && health_publish=(-p "127.0.0.1:${host_health_port}:8443")
    local ssh_agent_socket
    ssh_agent_socket="$(tenant_run_dir "$name")/ssh-agent.sock"
    local -a ssh_mount=(-v "${ssh_agent_socket}:/tmp/ssh-agent.sock:z" -e SSH_AUTH_SOCK=/tmp/ssh-agent.sock)
    _ctr "$name" run -d \
      --name "$container_name" \
      "${kimi_userns_args[@]}" \
      -e LUNARWING_WORKER_ID="worker-kimi-${name}" \
      -e WS_PORT="$wss_port" \
      -e HEALTH_PORT="8443" \
      -e KIMI_MODE=websocket \
      -e WS_ROLE=server \
      -e WS_BIND_HOST=0.0.0.0 \
      -e WS_PATH=/ws/agent \
      "${env_flags[@]}" \
      "${ssh_mount[@]}" \
      -p "127.0.0.1:${wss_port}:${wss_port}" \
      "${health_publish[@]}" \
      -v "$workspace_dir:/workspace:z" \
      "${restart_arg[@]}" \
      lunarwing-worker-kimi:latest \
      --mode websocket >/dev/null
    _store_container_hash \
      "$name" "$container_name" "$(tenant_env_dir "$name")/lunarwing.env" \
      "$kimi_launch_contract"
  fi

  _register_worker_unit "$name" kimi
  say "kimi worker ready ($container_name, WSS port $wss_port)"
}

stop_tenant_kimi() {
  local name="$1"
  ensure_container_runtime

  local container_name="lunarwing-kimi-$name"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" && -f "/etc/init.d/${container_name}" ]]; then
    _deregister_babysitter "$container_name"
    rc-service "$container_name" stop >/dev/null 2>&1 || true
    say "kimi worker stopped ($container_name)"
  elif _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
    say "kimi worker stopped ($container_name)"
  fi
}

# ── Pebble worker container ──────────────────────────────────────────────────

start_tenant_pebble() {
  local name="$1"
  ensure_container_runtime

  local wss_port container_name
  wss_port="$(ports_get "$name" pebble_wss)"
  container_name="lunarwing-pebble-$name"

  if [[ -z "$wss_port" ]]; then
    say "no pebble_wss port allocated for $name (skipping pebble worker)"
    return 0
  fi

  if ! _ensure_tenant_image "$name" lunarwing-worker-pebble:latest; then
    say "pebble worker image not available; run 'build-pebble-worker' first (skipping)"
    return 0
  fi

  # systemd + rootless podman: Quadlet .container owns the lifecycle (the unit's
  # [Container] spec creates+runs it), so skip the imperative `_ctr run` below.
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _wait_user_manager "$name"
    local quadlet_file
    quadlet_file="$(tenant_quadlet_dir "$name")/lunarwing-pebble-${name}.container"
    render_worker_quadlet "$name" pebble 8443
    _systemctl_user "$name" daemon-reload 2>/dev/null || true
    if _container_config_changed "$name" "lunarwing-pebble-${name}" "$quadlet_file"; then
      _recreate_quadlet_container "$name" "lunarwing-pebble-${name}.service" "lunarwing-pebble-${name}"
    fi
    if _systemctl_user "$name" start "lunarwing-pebble-${name}.service" >/dev/null 2>&1; then
      _store_container_hash "$name" "lunarwing-pebble-${name}" "$quadlet_file"
      say "pebble worker ready via quadlet (lunarwing-pebble-${name}.service, WSS port $wss_port)"
    else
      say "WARNING: lunarwing-pebble-${name}.service failed to start" >&2
      _systemctl_user "$name" status "lunarwing-pebble-${name}.service" --no-pager >&2 || true
    fi
    return 0
  fi

  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    local env_file_for_hash
    env_file_for_hash="$(tenant_env_dir "$name")/lunarwing.env"
    if _container_config_changed "$name" "$container_name" "$env_file_for_hash"; then
      say "config changed for $container_name; force-recreating"
      _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
      _ctr "$name" rm -f "$container_name" >/dev/null 2>&1 || true
    elif _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
      say "pebble worker already running ($container_name, WSS port $wss_port)"
      _store_container_hash "$name" "$container_name" "$env_file_for_hash"
      _register_worker_unit "$name" pebble
      return 0
    else
      say "starting existing pebble worker container $container_name"
      _ctr "$name" start "$container_name" >/dev/null
      _store_container_hash "$name" "$container_name" "$env_file_for_hash"
      _register_worker_unit "$name" pebble
      say "pebble worker ready ($container_name, WSS port $wss_port)"
      return 0
    fi
  fi

  if ! _ctr "$name" inspect "$container_name" &>/dev/null; then
    say "creating pebble worker container $container_name on WSS port $wss_port"

    local tenant_env_path
    tenant_env_path="$(tenant_env_dir "$name")/lunarwing.env"

    local pebble_env_path
    pebble_env_path="$(tenant_env_dir "$name")/pebble.env"

    local env_flags=()
    if [[ -f "$tenant_env_path" ]]; then
      local gateway_token
      gateway_token="$(_read_env_value "$tenant_env_path" GATEWAY_AUTH_TOKEN)"
      [[ -n "$gateway_token" ]] && env_flags+=(-e "AGENT_AUTH_TOKEN=$gateway_token")
    fi

    if [[ -f "$pebble_env_path" ]]; then
      env_flags+=(--env-file "$pebble_env_path")
    fi

    local workspace_dir
    workspace_dir="$(tenant_lw_root "$name")/pebble-workspace"
    mkdir -p "$workspace_dir"
    chown "$name:$name" "$workspace_dir"
    chmod 777 "$workspace_dir"

    # HEALTH_PORT=8443 matches the image's baked HEALTHCHECK (in-container).
    # v8: also publish the tenant's dedicated pebble_health port -> container
    # 8443, so the host self-heal pipeline can probe /health directly.
    local -a restart_arg=()
    [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)
    local -a health_publish=()
    local host_health_port
    host_health_port="$(ports_get "$name" pebble_health)" || true
    [[ -n "$host_health_port" ]] && health_publish=(-p "127.0.0.1:${host_health_port}:8443")
    # SSH agent socket (always included — daemon creates it at startup).
    # :z label for SELinux (Enforcing on Fedora).
    local -a ssh_mount=()
    local ssh_agent_socket
    ssh_agent_socket="$(tenant_run_dir "$name")/ssh-agent.sock"
    ssh_mount=(-v "${ssh_agent_socket}:/tmp/ssh-agent.sock:z" -e SSH_AUTH_SOCK=/tmp/ssh-agent.sock)
    _ctr "$name" run -d \
      --name "$container_name" \
      -e LUNARWING_WORKER_ID="worker-pebble-${name}" \
      -e WS_PORT="$wss_port" \
      -e HEALTH_PORT="8443" \
      -e PEBBLE_MODE=websocket \
      -e WS_BIND_HOST=0.0.0.0 \
      -e WS_PATH=/ws/agent \
      "${env_flags[@]}" \
      "${ssh_mount[@]}" \
      -p "127.0.0.1:${wss_port}:${wss_port}" \
      "${health_publish[@]}" \
      -v "$workspace_dir:/workspace:z" \
      "${restart_arg[@]}" \
      lunarwing-worker-pebble:latest >/dev/null
    _store_container_hash "$name" "$container_name" "$(tenant_env_dir "$name")/lunarwing.env"
  fi

  _register_worker_unit "$name" pebble
  say "pebble worker ready ($container_name, WSS port $wss_port)"
}

stop_tenant_pebble() {
  local name="$1"
  ensure_container_runtime

  local container_name="lunarwing-pebble-$name"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" && -f "/etc/init.d/${container_name}" ]]; then
    _deregister_babysitter "$container_name"
    rc-service "$container_name" stop >/dev/null 2>&1 || true
    say "pebble worker stopped ($container_name)"
  elif _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
    say "pebble worker stopped ($container_name)"
  fi
}

# ── LunarVision OCR/vision sidecar ──────────────────────────────────────────

VISION_SIDECAR_IMAGE=lunarwing/vision-service:latest
VISION_SIDECAR_INTERNAL_PORT=8088
VISION_SIDECAR_HEALTH_PORT=8089

build_vision_sidecar_image() {
  ensure_container_runtime
  local sidecar_dir="$SOURCE_REPO/projects/ocr-sidecar"
  [[ -d "$sidecar_dir" ]] || { say "WARNING: $sidecar_dir not found; skipping vision sidecar build"; return 0; }
  if [[ -f "$sidecar_dir/Dockerfile" ]]; then
    "$CONTAINER_RT" build --network=host --format docker -t "$VISION_SIDECAR_IMAGE" -f "$sidecar_dir/Dockerfile" "$sidecar_dir" >/dev/null \
      || { say "WARNING: vision sidecar image build failed (run 'build-vision-sidecar' to retry)"; return 1; }
    say "vision sidecar image built: $VISION_SIDECAR_IMAGE"
  else
    say "WARNING: $sidecar_dir/Dockerfile missing; cannot build vision sidecar"
    return 1
  fi
}

write_tenant_vision_env() {
  local name="$1"
  local env_dir env_path
  env_dir="$(tenant_env_dir "$name")"
  env_path="$env_dir/vision.env"
  local token
  token="$(_env_existing "$env_path" LUNARWING_AUTH_TOKEN)"
  token="${token:-$(generate_token)}"
  # Resolve VL backend URL: explicit override wins, else fleet default.
  # Empty (LUNARWING_MT_VL_URL= and DEFAULT_VL_URL unset) = no VL line written;
  # sidecar comes up with VL disabled. Idempotent — vision.env is fully rewritten.
  local vl_url
  vl_url="${LUNARWING_MT_VL_URL:-$DEFAULT_VL_URL}"
  mkdir -p "$env_dir"
  (
    umask 077
    if [[ -n "$vl_url" ]]; then
      cat >"$env_path" <<ENVEOF
LUNARWING_AUTH_TOKEN=$token
OCR_PORT=$VISION_SIDECAR_INTERNAL_PORT
OCR_HEALTH_PORT=$VISION_SIDECAR_HEALTH_PORT
VL_URL=$vl_url
VL_MODEL=qwen3-vl
ENVEOF
    else
      cat >"$env_path" <<ENVEOF
LUNARWING_AUTH_TOKEN=$token
OCR_PORT=$VISION_SIDECAR_INTERNAL_PORT
OCR_HEALTH_PORT=$VISION_SIDECAR_HEALTH_PORT
ENVEOF
    fi
  )
  chown "$name:$name" "$env_path"
  printf '%s' "$token"
}

start_tenant_vision() {
  local name="$1"
  ensure_container_runtime

  local vision_port
  vision_port="$(ports_get "$name" vision_service)" || true
  if [[ -z "$vision_port" ]]; then
    say "no vision_service port allocated for $name (skipping vision sidecar)"
    return 0
  fi

  if ! _ensure_tenant_image "$name" "$VISION_SIDECAR_IMAGE"; then
    say "vision sidecar image not available; run 'build-vision-sidecar' first (skipping)"
    return 0
  fi

  local container_name="lunarwing-vision-$name"

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _wait_user_manager "$name"
    local quadlet_file
    quadlet_file="$(tenant_quadlet_dir "$name")/lunarwing-vision-${name}.container"
    render_vision_quadlet "$name"
    _systemctl_user "$name" daemon-reload 2>/dev/null || true
    if _container_config_changed "$name" "lunarwing-vision-${name}" "$quadlet_file"; then
      _recreate_quadlet_container "$name" "lunarwing-vision-${name}.service" "lunarwing-vision-${name}"
    fi
    if _systemctl_user "$name" start "lunarwing-vision-${name}.service" >/dev/null 2>&1; then
      _store_container_hash "$name" "lunarwing-vision-${name}" "$quadlet_file"
      say "vision sidecar ready via quadlet (lunarwing-vision-${name}.service, port $vision_port)"
    else
      say "WARNING: lunarwing-vision-${name}.service failed to start" >&2
      _systemctl_user "$name" status "lunarwing-vision-${name}.service" --no-pager >&2 || true
    fi
    return 0
  fi

  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    if _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
      say "vision sidecar already running ($container_name, port $vision_port)"
    else
      _ctr "$name" start "$container_name" >/dev/null
      say "vision sidecar restarted ($container_name, port $vision_port)"
    fi
  else
    local vision_env_path
    vision_env_path="$(tenant_env_dir "$name")/vision.env"
    [[ -f "$vision_env_path" ]] || write_tenant_vision_env "$name" >/dev/null

    local -a restart_arg=()
    [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)

    # v10: also publish the tenant's dedicated vision_health port -> container
    # 8089, so the host self-heal pipeline can probe /health directly (mirrors
    # the external-worker health_publish pattern).
    local -a health_publish=()
    local host_health_port
    host_health_port="$(ports_get "$name" vision_health)" || true
    [[ -n "$host_health_port" ]] && health_publish=(-p "127.0.0.1:${host_health_port}:${VISION_SIDECAR_HEALTH_PORT}")

    say "creating vision sidecar container $container_name on port $vision_port"
    _ctr "$name" run -d \
      --name "$container_name" \
      --env-file "$vision_env_path" \
      -p "127.0.0.1:${vision_port}:${VISION_SIDECAR_INTERNAL_PORT}" \
      "${health_publish[@]}" \
      "${restart_arg[@]}" \
      "$VISION_SIDECAR_IMAGE" >/dev/null
    say "vision sidecar ready ($container_name, port $vision_port)"
  fi

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" ]]; then
    render_vision_openrc_unit "$name"
    rc-update add "$container_name" default >/dev/null 2>&1 || true
    rc-service "$container_name" start >/dev/null 2>&1 || \
      say "WARNING: rc-service $container_name start returned non-zero (container may already be up)"
  fi
}

stop_tenant_vision() {
  local name="$1"
  ensure_container_runtime

  local container_name="lunarwing-vision-$name"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "openrc" && -f "/etc/init.d/${container_name}" ]]; then
    rc-service "$container_name" stop >/dev/null 2>&1 || true
    say "vision sidecar stopped ($container_name)"
  elif _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
    say "vision sidecar stopped ($container_name)"
  fi
}

render_vision_openrc_unit() {
  local name="$1"
  ensure_container_runtime
  local vision_port runtime_bin container uid home
  vision_port="$(ports_get "$name" vision_service)"
  [[ -n "$vision_port" ]] || return 0
  [[ -n "${CONTAINER_RT:-}" ]] && runtime_bin="$(command -v "$CONTAINER_RT" 2>/dev/null || true)"
  container="lunarwing-vision-${name}"
  uid="$(id -u "$name" 2>/dev/null || echo "")"
  home="$(tenant_home "$name")"

  cat >"/etc/init.d/${container}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing vision sidecar ($name)"

: "\${vis_runtime:=$runtime_bin}"
: "\${vis_container:=$container}"
: "\${vis_rootless:=$MT_ROOTLESS}"
: "\${vis_user:=$name}"
: "\${vis_home:=$home}"
: "\${vis_uid:=$uid}"
: "\${vis_port:=$vision_port}"
: "\${vis_internal_port:=$VISION_SIDECAR_INTERNAL_PORT}"
: "\${vis_health_internal_port:=$VISION_SIDECAR_HEALTH_PORT}"
: "\${vis_image:=$VISION_SIDECAR_IMAGE}"
: "\${vis_env_file:=$(tenant_env_dir "$name")/vision.env}"
: "\${vis_wait:=30}"

depend() {
    need net localmount
    after firewall lunarwing-${name}
}

_vis() {
    if [ "\${vis_rootless}" = "true" ]; then
        sudo -u "\${vis_user}" env HOME="\${vis_home}" XDG_RUNTIME_DIR="/run/user/\${vis_uid}" "\${vis_runtime}" "\$@"
    else
        "\${vis_runtime}" "\$@"
    fi
}

_vis_healthy() {
    [ "\$(_vis inspect -f '{{.State.Running}}' "\${vis_container}" 2>/dev/null)" = "true" ] || return 1
    _vis exec "\${vis_container}" curl -sf -o /dev/null --max-time 3 "http://127.0.0.1:\${vis_health_internal_port}/health" 2>/dev/null
}

start() {
    [ -n "\${vis_runtime}" ] && [ -x "\${vis_runtime}" ] || { ewarn "no container runtime; skipping vision for $name"; return 0; }
    ebegin "Starting vision sidecar (\${vis_container})"
    if [ "\${vis_rootless}" = "true" ]; then
        checkpath -d -m 0700 -o "\${vis_user}:\${vis_user}" "/run/user/\${vis_uid}"
    fi
    _vis start "\${vis_container}" >/dev/null 2>&1 || { eend 1 "container start failed"; return 1; }
    _w=0
    while ! _vis_healthy; do
        _w=\$((_w + 1))
        [ "\$_w" -lt "\${vis_wait}" ] || { eend 1 "vision sidecar not healthy after \${vis_wait}s"; return 1; }
        sleep 1
    done
    eend 0
}

stop() {
    [ -n "\${vis_runtime}" ] && [ -x "\${vis_runtime}" ] || return 0
    ebegin "Stopping vision sidecar (\${vis_container})"
    _vis stop --time 30 "\${vis_container}" >/dev/null 2>&1
    eend 0
}

status() {
    if _vis_healthy; then
        einfo "\${vis_container}: started"; return 0
    fi
    einfo "\${vis_container}: stopped"; return 3
}
INITEOF
  chmod 0755 "/etc/init.d/${container}"
}

render_vision_quadlet() {
  local name="$1"
  local qdir vision_port vision_health_port env_path token
  qdir="$(tenant_quadlet_dir "$name")"
  vision_port="$(ports_get "$name" vision_service)"
  [[ -n "$vision_port" ]] || return 0
  vision_health_port="$(ports_get "$name" vision_health)" || true
  env_path="$(tenant_env_dir "$name")/vision.env"
  [[ -f "$env_path" ]] || write_tenant_vision_env "$name" >/dev/null
  token="$(_read_env_value "$env_path" LUNARWING_AUTH_TOKEN)"
  token="${token//%/%%}"
  mkdir -p "$qdir"

  {
    cat <<EOF
[Unit]
Description=LunarWing vision sidecar ($name)
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=300
StartLimitBurst=5

[Container]
ContainerName=lunarwing-vision-${name}
Image=${VISION_SIDECAR_IMAGE}
PublishPort=127.0.0.1:${vision_port}:${VISION_SIDECAR_INTERNAL_PORT}
Environment=OCR_PORT=${VISION_SIDECAR_INTERNAL_PORT}
Environment=OCR_HEALTH_PORT=${VISION_SIDECAR_HEALTH_PORT}
Environment=LUNARWING_AUTH_TOKEN=${token}
EOF
    # Publish the per-tenant dedicated vision health port (v10) -> container's
    # 8089, so the host self-heal pipeline can probe /health independently of
    # OCR traffic. The sidecar listens on OCR_HEALTH_PORT=8089 internally.
    [[ -n "$vision_health_port" ]] && printf 'PublishPort=127.0.0.1:%s:%s\n' "$vision_health_port" "$VISION_SIDECAR_HEALTH_PORT"
    if [[ -f "$env_path" ]]; then
      printf 'EnvironmentFile=%s\n' "$env_path"
    fi

    cat <<EOF

[Service]
Restart=on-failure
RestartSec=10

[Install]
WantedBy=default.target
EOF
  } > "$qdir/lunarwing-vision-${name}.container"
}

# ── PostgreSQL container ─────────────────────────────────────────────────────

start_tenant_postgres() {
  local name="$1"
  ensure_container_runtime

  local pg_port container_name
  pg_port="$(ports_get "$name" postgres)"
  container_name="lunarwing-pg-$name"

  # ── Rootful→rootless data-orphan guard ──────────────────────────────────────
  # v1.1.0 ran PG ROOTFUL (root's container store, data in the container writable
  # layer, no named volume); v1.1.4 defaults MT_ROOTLESS=true (rootless, tenant
  # store). If a legacy ROOT-store container still exists and the tenant has NO
  # rootless container yet, creating the rootless PG here would silently bring the
  # tenant up on an EMPTY database and orphan the v1.1.0 data. Refuse by default.
  # Fires ONLY in this exact window (rootless target + root container present +
  # rootless container absent), so fresh tenants and already-migrated tenants are
  # never affected. Escape hatches: migrate (upgrade-tenant.sh), keep rootful
  # (LUNARWING_MT_ROOTLESS=false), or acknowledge an intended fresh DB
  # (LUNARWING_MT_ACK_ROOTLESS_FLIP=<tenant> — set automatically by the migration
  # tool once it has a verified backup in hand). The ack is a comma-separated list
  # of tenant NAMES, not a global boolean, so a stray `export …=1` cannot silence
  # the guard for OTHER un-migrated tenants in the same shell/CI session.
  #
  # Coverage note: this fires only when the rootless container is ABSENT. The
  # "rootless exists but is EMPTY while a root orphan persists" case is deliberately
  # NOT caught here — the post-migration steady state (live rootless DB + a root
  # container kept as a rollback net) also has both present, and a start-time probe
  # can't tell them apart without the container running, so guarding it here would
  # break every legitimate restart. That case is surfaced by upgrade-preflight.sh
  # and blocked by `upgrade-tenant.sh --prune-old-root` (refuses to delete the root
  # copy while the rootless DB is empty).
  if [[ "$MT_ROOTLESS" == "true" && "$CONTAINER_RT" == "podman" ]] \
     && ! _ctr "$name" inspect "$container_name" &>/dev/null \
     && "$CONTAINER_RT" inspect "$container_name" &>/dev/null; then
    {
      say "############################################################"
      say "# DATA-ORPHAN GUARD — tenant '$name'"
      say "# A ROOTFUL (root-store) '$container_name' exists, but MT_ROOTLESS=true"
      say "# would create a NEW, EMPTY rootless database and orphan the existing"
      say "# v1.1.0 data (still recoverable from the root container until removed)."
      say "#"
      say "# Choose one:"
      say "#   migrate data:     sudo ic/scripts/upgrade-tenant.sh $name"
      say "#   keep rootful:     LUNARWING_MT_ROOTLESS=false <re-run this command>"
      say "#   intended fresh DB: LUNARWING_MT_ACK_ROOTLESS_FLIP=$name <re-run>"
      say "############################################################"
    } >&2
    if [[ ",${LUNARWING_MT_ACK_ROOTLESS_FLIP:-}," != *",$name,"* ]]; then
      die "refusing to create an empty rootless PG over existing root-store data for '$name' (see guard above)"
    fi
    say "LUNARWING_MT_ACK_ROOTLESS_FLIP lists '$name' — proceeding with a fresh rootless DB; root-store data left intact." >&2
  fi

  # systemd + rootless podman: a Quadlet .container owns the lifecycle (boot-
  # persistent, health-monitored, self-healable). Quadlet creates the container,
  # so skip the imperative `_ctr run` below; keep the pg_isready gate (via exec).
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    _wait_user_manager "$name"
    render_pg_quadlet "$name"
    _systemctl_user "$name" daemon-reload 2>/dev/null || true
    if ! _systemctl_user "$name" start "lunarwing-pg-${name}.service" >/dev/null 2>&1; then
      say "WARNING: lunarwing-pg-${name}.service failed to start" >&2
      _systemctl_user "$name" status "lunarwing-pg-${name}.service" --no-pager >&2 || true
    fi
    # Gate on the container's own health (the Quadlet defines a pg_isready
    # HealthCmd) OR a direct pg_isready, rather than a bare `exec` loop: on a fresh
    # volume the initdb cycle (temp server up→down→restart) makes a single exec
    # probe flap, which previously exhausted the budget and `die`d — aborting the
    # WHOLE provision and leaving a half-baked tenant (F2). A slow first boot must
    # NOT strand the tenant: warn and continue (the unit has Restart=on-failure and
    # the host health pipeline/self-heal converge it), so add-tenant still renders
    # units and installs the pipeline.
    local q_attempts=0 q_ready=false q_health=""
    while (( q_attempts < 120 )); do
      q_health="$(_ctr "$name" inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{end}}' "$container_name" 2>/dev/null || true)"
      if [[ "$q_health" == healthy ]] || _ctr "$name" exec "$container_name" pg_isready -U lunarwing -q 2>/dev/null; then
        q_ready=true; break
      fi
      q_attempts=$((q_attempts + 1)); sleep 1
    done
    if [[ "$q_ready" == true ]]; then
      say "PostgreSQL ready via quadlet ($container_name, port $pg_port)"
    else
      say "WARNING: PostgreSQL for $name not confirmed ready after ${q_attempts}s (health=${q_health:-none}); continuing — pg has Restart=on-failure and the health pipeline will converge it." >&2
    fi
    return 0
  fi

  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    if _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
      say "PostgreSQL already running ($container_name, port $pg_port)"
      return 0
    fi
    say "starting existing PostgreSQL container $container_name"
    if ! _ctr "$name" start "$container_name" >/dev/null; then
      say "WARNING: could not start existing PostgreSQL container $container_name for $name; continuing — re-run start-tenant or let the health pipeline converge it." >&2
      return 0
    fi
  else
    say "creating PostgreSQL container $container_name on port $pg_port"
    # Named volume (not anonymous) so the data has a stable, inspectable,
    # exportable identity for backups; rootless podman auto-chowns it inside the
    # tenant user namespace. --restart is a no-op under rootless podman (no
    # daemon — OpenRC owns lifecycle), so only set it for rootful docker.
    local -a restart_arg=()
    [[ "$MT_ROOTLESS" == "true" ]] || restart_arg=(--restart unless-stopped)
    # Pass POSTGRES_PASSWORD via a transient 0600 --env-file rather than `-e` so
    # the per-tenant secret never lands on the container-runtime argv (readable in
    # /proc/<pid>/cmdline by other local users). Removed right after creation; the
    # Quadlet path keeps it in its own 0600 unit file for the same reason.
    local pg_init_env
    pg_init_env="$(tenant_env_dir "$name")/.pg-init.env"
    ( umask 077; printf 'POSTGRES_PASSWORD=%s\n' "$(tenant_pg_password "$name")" >"$pg_init_env" )
    chown "$name:$name" "$pg_init_env" 2>/dev/null || true
    # O2: don't let a pg CREATE failure abort the whole provision (set -e). The F2
    # gate already downgrades a slow-readiness race to warn+continue; mirror that
    # for the create call (and the start above) so a transient runtime hiccup
    # doesn't strand the tenant half-baked (no units rendered, no health pipeline
    # installed). add-tenant is resumable (F4): fix the cause and re-run.
    if ! _ctr "$name" run -d \
      --name "$container_name" \
      -e POSTGRES_USER=lunarwing \
      --env-file "$pg_init_env" \
      -e POSTGRES_DB=lunarwing \
      -p "127.0.0.1:${pg_port}:5432" \
      -v "lunarwing-pg-${name}:/var/lib/postgresql/data" \
      "${restart_arg[@]}" \
      "$PG_IMAGE" >/dev/null; then
      rm -f "$pg_init_env"
      # Remove the partial/failed container so a resumed start-tenant re-creates it
      # cleanly, rather than taking the "start existing" branch on a broken shell.
      _ctr "$name" rm -f "$container_name" >/dev/null 2>&1 || true
      say "WARNING: PostgreSQL container create failed for $name; add-tenant still renders units + installs the health pipeline (so exit 0 does NOT imply pg is up). Fix the cause and re-run start-tenant." >&2
      return 0
    fi
    rm -f "$pg_init_env"
  fi

  local attempts=0
  while ! _ctr "$name" exec "$container_name" pg_isready -U lunarwing -q 2>/dev/null; do
    attempts=$((attempts + 1))
    if (( attempts >= 90 )); then
      say "WARNING: PostgreSQL for $name not confirmed ready after ${attempts}s; continuing — the host health pipeline/self-heal will converge it." >&2
      return 0
    fi
    sleep 1
  done
  say "PostgreSQL ready ($container_name, port $pg_port)"
}

stop_tenant_postgres() {
  local name="$1"
  ensure_container_runtime

  local container_name="lunarwing-pg-$name"
  if _ctr "$name" inspect "$container_name" &>/dev/null; then
    _ctr "$name" stop "$container_name" >/dev/null 2>&1 || true
    say "PostgreSQL stopped ($container_name)"
  fi
}

reset_tenant_postgres() {
  local name="$1"
  ensure_container_runtime

  local container_name="lunarwing-pg-$name"
  stop_tenant_postgres "$name"
  _ctr "$name" rm -f "$container_name" >/dev/null 2>&1 || true
  # Remove the named data volume too so a subsequent create starts fresh (matches
  # the pre-named-volume behaviour where the anonymous volume was orphaned on rm).
  _ctr "$name" volume rm "lunarwing-pg-${name}" >/dev/null 2>&1 || true
  say "PostgreSQL removed ($container_name, data volume cleared)"
}

# Rotate an existing tenant's PG password to a fresh random one. Unlike a new
# tenant (where POSTGRES_PASSWORD seeds an empty datadir), an initialised DB needs
# an in-place ALTER ROLE, then the persisted secret + DATABASE_URL updated, then a
# daemon restart so it reconnects with the new credential. The new password is hex
# (URL/SQL-safe) and is never echoed.
rotate_tenant_pg_password() {
  local name="$1"
  ensure_container_runtime
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"

  local container_name="lunarwing-pg-$name"
  _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true \
    || die "PostgreSQL container $container_name is not running; start the tenant first"

  local envf pg_port new_pw secret_file tmp
  envf="$(tenant_env_dir "$name")/lunarwing.env"
  [[ -f "$envf" ]] || die "tenant env not found: $envf"
  pg_port="$(ports_get "$name" postgres)"
  new_pw="$(generate_token | cut -c1-32)"

  # Change the live role password. psql connects over the container's local unix
  # socket (trust auth in the postgres image), and the new value is fed on stdin —
  # never on argv or in logs. A hex value carries no SQL-quoting hazard.
  if ! printf "ALTER ROLE lunarwing PASSWORD '%s';\n" "$new_pw" \
       | _ctr "$name" exec -i "$container_name" psql -v ON_ERROR_STOP=1 -U lunarwing -d lunarwing -q >/dev/null 2>&1; then
    die "failed to ALTER ROLE password inside $container_name (is the DB healthy?)"
  fi

  # Persist the new secret (source of truth) ...
  secret_file="$(tenant_env_dir "$name")/pg.secret"
  ( umask 077; printf '%s\n' "$new_pw" > "$secret_file" )
  chown "$name:$name" "$secret_file" 2>/dev/null || true

  # ... and rewrite DATABASE_URL in place (preserving the env file's owner/perms).
  # Temp lives beside the env file (0600), not in shared /tmp, so the cleartext
  # password isn't briefly exposed there — matching the convention used elsewhere.
  tmp="$(mktemp "$envf.tmp.XXXXXX")"
  sed "s#^DATABASE_URL=.*#DATABASE_URL=postgres://lunarwing:${new_pw}@127.0.0.1:${pg_port}/lunarwing#" "$envf" >"$tmp"
  cat "$tmp" >"$envf"
  rm -f "$tmp"

  say "Rotated PostgreSQL password for tenant '$name'."
  say "IMPORTANT: the running daemon still holds the old credential — restart to apply:"
  say "    lunarwing-mt-admin.sh restart-tenant $name"
}

# ── PostgreSQL backup / restore ──────────────────────────────────────────────

# pg_dump a tenant's database to a timestamped custom-format file under
# $BACKUP_DIR/<tenant>/. Runs pg_dump inside the tenant's pg container via _ctr,
# so it is init- and runtime-agnostic (rootless podman or rootful docker). Safe
# on a live database (MVCC snapshot). Prunes to the most recent $BACKUP_KEEP.
backup_tenant_postgres() {
  local name="$1"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  ensure_container_runtime
  local container_name="lunarwing-pg-$name"

  _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true \
    || die "PostgreSQL not running for '$name' ($container_name) — start the tenant first"

  local dir ts dest tmp
  dir="$BACKUP_DIR/$name"
  mkdir -p "$dir"
  chmod 0700 "$BACKUP_DIR" "$dir" 2>/dev/null || true
  ts="$(date +%Y%m%d%H%M%S)"
  dest="$dir/${name}-${ts}.dump"
  tmp="$dest.partial"

  say "backing up '$name' -> $dest"
  # -Fc: compressed custom format (restore via pg_restore). umask 077 so the
  # .partial is never world-readable, even mid-dump; rename on success so an
  # interrupted dump never looks complete.
  if ( umask 077; _ctr "$name" exec "$container_name" pg_dump -U lunarwing -Fc lunarwing > "$tmp" ); then
    mv "$tmp" "$dest"
    say "backup complete: $dest ($(du -h "$dest" 2>/dev/null | cut -f1))"
  else
    rm -f "$tmp"
    die "pg_dump failed for '$name'"
  fi
  _prune_tenant_backups "$name"
}

# Keep only the most recent $BACKUP_KEEP dumps for a tenant (0/unset = keep all).
_prune_tenant_backups() {
  local name="$1" dir="$BACKUP_DIR/$1"
  [[ "${BACKUP_KEEP:-0}" =~ ^[0-9]+$ && "$BACKUP_KEEP" -gt 0 ]] || return 0
  local -a dumps=("$dir"/*.dump)
  [[ -e "${dumps[0]:-}" ]] || return 0          # glob did not match -> nothing to prune
  local n=${#dumps[@]} i
  (( n > BACKUP_KEEP )) || return 0
  for (( i=0; i < n - BACKUP_KEEP; i++ )); do   # glob is ascending (timestamp) = oldest first
    rm -f "${dumps[$i]}"
    say "pruned old backup: ${dumps[$i]}"
  done
}

# List existing backups for one tenant or all.
list_tenant_backups() {
  local filter="${1:-}" names
  if [[ -n "$filter" ]]; then names="$(sanitize_name "$filter")"; else names="$(all_tenant_names)"; fi
  [[ -n "$names" ]] || { say "no tenants"; return 0; }
  say "=== Backups (under $BACKUP_DIR) ==="
  while IFS= read -r name; do
    [[ -n "$name" ]] || continue
    say ""
    say "$name:"
    if compgen -G "$BACKUP_DIR/$name/*.dump" >/dev/null 2>&1; then
      ls -1sh "$BACKUP_DIR/$name"/*.dump 2>/dev/null | sed 's/^/  /'
    else
      say "  (no backups)"
    fi
  done <<< "$names"
}

# Restore a tenant's database from a custom-format dump. DESTRUCTIVE: pg_restore
# --clean --if-exists DROPs and recreates objects. Requires the tenant daemon to
# be stopped (no concurrent writes) and an explicit --yes.
restore_tenant_postgres() {
  local name="$1" file="$2" confirmed="${3:-false}"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  [[ -f "$file" ]] || die "backup file not found: $file"
  # Reject anything that is not a custom-format archive before touching the DB
  # (custom-format pg_dump files begin with the magic "PGDMP").
  [[ "$(head -c5 "$file" 2>/dev/null)" == "PGDMP" ]] \
    || die "not a custom-format pg_dump archive (missing PGDMP header): $file"
  [[ "$confirmed" == "true" ]] || die "restore DROPs and recreates the database for '$name'. Re-run with --yes to confirm."
  ensure_container_runtime
  ensure_init_system
  local container_name="lunarwing-pg-$name"

  _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true \
    || die "PostgreSQL not running for '$name' — start the tenant's pg container first"

  # Refuse unless we can POSITIVELY confirm the daemon is stopped (concurrent
  # writes corrupt a restore). Fail closed: if the service tool is missing or the
  # state is indeterminate, never assume "stopped".
  local daemon_state="unknown"
  if [[ "$INIT_SYSTEM" == "systemd" ]] && command -v systemctl >/dev/null 2>&1; then
    if _systemctl_user "$name" is-active --quiet "lunarwing-${name}.service" 2>/dev/null; then daemon_state="active"; else daemon_state="stopped"; fi
  elif [[ "$INIT_SYSTEM" == "openrc" ]] && command -v rc-service >/dev/null 2>&1; then
    if rc-service "lunarwing-${name}" status >/dev/null 2>&1; then daemon_state="active"; else daemon_state="stopped"; fi
  fi
  case "$daemon_state" in
    stopped) : ;;
    active)  die "stop the daemon first: $0 stop-tenant $name  (restart after restore)" ;;
    *)       die "cannot confirm the '$name' daemon is stopped (no $INIT_SYSTEM service tool?); stop it manually, then re-run" ;;
  esac

  say "restoring '$name' from $file (single transaction, DROP + recreate) ..."
  # --single-transaction: all-or-nothing. A mid-restore failure rolls the whole
  # DROP+recreate back, so a failed restore never leaves the DB half-dropped.
  if _ctr "$name" exec -i "$container_name" pg_restore -U lunarwing -d lunarwing --single-transaction --clean --if-exists < "$file"; then
    say "restore complete for '$name'. Restart the tenant: $0 start-tenant $name"
  else
    die "pg_restore failed for '$name' — rolled back (single transaction); the database is unchanged"
  fi
}

# ── Systemd service units ────────────────────────────────────────────────────

# Render a per-tenant Quadlet .container for the Postgres container. The podman
# user-generator turns this into lunarwing-pg-<t>.service at `systemctl --user
# daemon-reload`; [Install] makes the lingering user manager start it at boot.
# Quadlet owns creation (the imperative `_ctr run` is skipped on this path), so
# the named volume below preserves data across container replacement.
render_pg_quadlet() {
  local name="$1"
  local qdir pg_port pg_password
  qdir="$(tenant_quadlet_dir "$name")"
  pg_port="$(ports_get "$name" postgres)"
  pg_password="$(tenant_pg_password "$name")"   # inlined into the 0600 tenant-owned .container
  mkdir -p "$qdir"
  cat >"$qdir/lunarwing-pg-${name}.container" <<EOF
[Unit]
Description=LunarWing Postgres container ($name)
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=300
StartLimitBurst=5

[Container]
ContainerName=lunarwing-pg-${name}
Image=${PG_IMAGE}
PublishPort=127.0.0.1:${pg_port}:5432
Volume=lunarwing-pg-${name}:/var/lib/postgresql/data
Environment=POSTGRES_USER=lunarwing
Environment=POSTGRES_PASSWORD=${pg_password}
Environment=POSTGRES_DB=lunarwing
HealthCmd=pg_isready -U lunarwing -q
HealthInterval=10s
HealthTimeout=3s
HealthRetries=5
HealthStartPeriod=30s

[Service]
Restart=on-failure
RestartSec=5
TimeoutStartSec=120

[Install]
WantedBy=default.target
EOF
  chmod 0600 "$qdir/lunarwing-pg-${name}.container"
  chown -R "$name:$name" "$(tenant_home "$name")/.config/containers"
}

# Render a per-tenant Quadlet .container for an external worker.
# Mirrors the imperative env from start_tenant_<worker>: the GATEWAY_AUTH_TOKEN ->
# AGENT_AUTH_TOKEN and LLM_API_KEY -> TENSORZERO_API_KEY remap is resolved here and
# inlined as Environment= in a 0600 tenant-owned unit. Kimi uses a separate
# mode-0600 runtime env file so its API key never enters generated unit commands.
# Returns early (no unit) when the worker has no allocated wss port.
render_worker_quadlet() {
  local name="$1" worker="$2" health_port="${3:-8443}"
  case "$worker" in
    pebble|opencode|kimi) ;;
    *) die "unsupported external worker '$worker'" ;;
  esac
  local qdir wss_port workspace_dir env_dir tenant_env_path worker_env_path host_health_port
  local runtime_env_path="" runtime_env_hash="" health_path="health"
  qdir="$(tenant_quadlet_dir "$name")"
  wss_port="$(ports_get "$name" "${worker}_wss")"
  host_health_port="$(ports_get "$name" "${worker}_health")"
  [[ -n "$wss_port" ]] || return 0
  env_dir="$(tenant_env_dir "$name")"
  tenant_env_path="$env_dir/lunarwing.env"
  worker_env_path="$env_dir/${worker}.env"
  workspace_dir="$(tenant_lw_root "$name")/${worker}-workspace"
  mkdir -p "$workspace_dir"
  if [[ "$worker" == "kimi" || "$worker" == "opencode" ]]; then
    workspace_dir="$(prepare_worker_workspace "$name" "$worker")"
  else
    chown "$name:$name" "$workspace_dir"
  fi
  [[ "$worker" == "kimi" || "$worker" == "opencode" ]] || chmod 777 "$workspace_dir"
  [[ "$worker" != "kimi" ]] || runtime_env_path="$(write_kimi_runtime_env "$name")"
  [[ -z "$runtime_env_path" ]] \
    || runtime_env_hash="$(sha256sum "$runtime_env_path" | cut -d' ' -f1)"
  [[ "$worker" != "kimi" ]] || health_path="ready"
  mkdir -p "$qdir"

  local agent_token tz_key
  agent_token="$(_read_env_value "$tenant_env_path" GATEWAY_AUTH_TOKEN)"
  tz_key="$(_read_env_value "$tenant_env_path" LLM_API_KEY)"
  # systemd treats % as a unit specifier; escape so a token containing % survives.
  agent_token="${agent_token//%/%%}"
  tz_key="${tz_key//%/%%}"

  # SSH agent socket: the gateway creates an SSH agent server at startup,
  # with its socket at <tenant_home>/lunarwing/run/ssh-agent.sock (NOT /tmp,
  # because the daemon runs with PrivateTmp=true — a /tmp socket would be
  # invisible to podman containers). Always include the volume mount + env var
  # in the quadlet, even if the socket doesn't exist yet at render time — the
  # daemon creates it before the worker needs it (workers only use SSH when a
  # create_job with SSH commands is invoked, well after startup). If SSH is
  # not configured for the tenant, the socket simply won't exist and SSH
  # commands from the worker will fail with a clear "no agent" error.
  local ssh_agent_socket
  ssh_agent_socket="$(tenant_run_dir "$name")/ssh-agent.sock"
  local study_notes_home="" study_notes_state_home="" study_notes_allow_write=""
  if [[ "$worker" == "opencode" ]]; then
    study_notes_home="$(tenant_study_notes_home "$name")"
    [[ -z "$study_notes_home" ]] || validate_study_notes_home "$study_notes_home"
    if [[ -n "$study_notes_home" ]]; then
      study_notes_state_home="$(tenant_study_notes_state_home "$name")"
      study_notes_allow_write="$(tenant_study_notes_allow_write "$name")"
      if study_notes_write_enabled "$study_notes_allow_write"; then
        [[ -n "$study_notes_state_home" ]] \
          || die "Study Notes write mode requires LW2STUDYMCP_STATE_HOME"
      fi
      [[ -z "$study_notes_state_home" ]] \
        || validate_study_notes_state_home "$study_notes_state_home"
    fi
  fi

  {
    cat <<EOF
[Unit]
Description=LunarWing ${worker} worker ($name)
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=300
StartLimitBurst=5

[Container]
ContainerName=lunarwing-${worker}-${name}
Image=lunarwing-worker-${worker}:latest
PublishPort=127.0.0.1:${wss_port}:${wss_port}
Volume=${workspace_dir}:/workspace:z
Environment=LUNARWING_WORKER_ID=worker-${worker}-${name}
Environment=WS_PORT=${wss_port}
Environment=HEALTH_PORT=${health_port}
Environment=WS_BIND_HOST=0.0.0.0
Environment=WS_PATH=/ws/agent
EOF
    if [[ -n "$study_notes_home" ]]; then
      printf 'Volume=%s:%s:ro,z\n' "$study_notes_home" "$STUDY_NOTES_CONTAINER_HOME"
      printf 'Environment=LW2STUDYMCP_HOME=%s\n' "$STUDY_NOTES_CONTAINER_HOME"
      if [[ -n "$study_notes_state_home" ]]; then
        printf 'Volume=%s:%s:ro,z\n' \
          "$study_notes_state_home" "$STUDY_NOTES_CONTAINER_STATE_HOME"
        printf 'Environment=LW2STUDYMCP_STATE_HOME=%s\n' "$STUDY_NOTES_CONTAINER_STATE_HOME"
      fi
    fi
    if [[ "$worker" == "opencode" ]]; then
      printf 'UserNS=%s\n' "$OPENCODE_USERNS"
    elif [[ "$worker" == "kimi" ]]; then
      printf 'UserNS=%s\n' "$KIMI_USERNS"
    fi
    # SSH agent socket mount + env (always included — the daemon creates the
    # socket at startup; if SSH isn't configured, the socket won't exist and
    # SSH commands from the worker will fail with a clear "no agent" error).
    # :z label so SELinux (Enforcing on Fedora) permits container_t access to
    # the tenant-home-labeled socket.
    printf 'Volume=%s:/tmp/ssh-agent.sock:z\n' "$ssh_agent_socket"
    printf 'Environment=SSH_AUTH_SOCK=/tmp/ssh-agent.sock\n'
    # Publish the per-tenant dedicated health port (v8) -> container's 8443, so
    # the host self-heal pipeline can probe the worker directly. Container still
    # listens on HEALTH_PORT=8443 internally (matches image healthcheck).
    [[ -n "$host_health_port" ]] && printf 'PublishPort=127.0.0.1:%s:8443\n' "$host_health_port"
    if [[ "$worker" == "pebble" ]]; then
      printf 'Environment=PEBBLE_MODE=websocket\n'
    elif [[ "$worker" == "opencode" ]]; then
      printf 'Environment=OPENCODE_MODE=websocket\n'
      printf 'Environment=WS_ROLE=server\n'
    elif [[ "$worker" == "kimi" ]]; then
      printf 'Environment=KIMI_MODE=websocket\n'
      printf 'Environment=WS_ROLE=server\n'
    fi
    [[ "$worker" == "kimi" || -z "$agent_token" ]] \
      || printf 'Environment=AGENT_AUTH_TOKEN=%s\n' "$agent_token"
    [[ "$worker" == "opencode" && -n "$tz_key" ]] && printf 'Environment=TENSORZERO_API_KEY=%s\n' "$tz_key"
    local oc_model oc_base_url
    oc_model="$(_read_env_value "$tenant_env_path" OPENCODE_MODEL)"
    oc_base_url="$(_read_env_value "$tenant_env_path" OPENCODE_BASE_URL)"
    [[ "$worker" == "opencode" && -n "$oc_model" ]] && printf 'Environment=OPENCODE_MODEL=%s\n' "$oc_model"
    [[ "$worker" == "opencode" && -n "$oc_base_url" ]] && printf 'Environment=OPENCODE_BASE_URL=%s\n' "$oc_base_url"
    if [[ -n "$runtime_env_path" ]]; then
      printf '# RuntimeEnvHash=%s\n' "$runtime_env_hash"
      printf 'EnvironmentFile=%s\n' "$runtime_env_path"
    fi
    # Operator override file (optional). EnvironmentFile= has existed since the
    # Quadlet 4.4 debut, so it is safe at our >= 4.6 floor.
    [[ -f "$worker_env_path" ]] && printf 'EnvironmentFile=%s\n' "$worker_env_path"
    [[ "$worker" == "opencode" || "$worker" == "kimi" ]] && printf 'Exec=--mode websocket\n'
    cat <<EOF
HealthCmd=curl -sf http://127.0.0.1:${health_port}/${health_path} || exit 1
HealthInterval=15s
HealthTimeout=5s
HealthRetries=3
HealthStartPeriod=30s

[Service]
Restart=on-failure
RestartSec=5
TimeoutStartSec=120

[Install]
WantedBy=default.target
EOF
  } >"$qdir/lunarwing-${worker}-${name}.container"
  chmod 0600 "$qdir/lunarwing-${worker}-${name}.container"
  chown -R "$name:$name" "$(tenant_home "$name")/.config/containers"
}

# Render the WeeChat systemd user unit into <out_dir>.
# Loads only weechat.env (RELAY_PASSWORD), not the full lunarwing.env.
# ExecStop sends /upgrade -quit via the WeeChat FIFO so WeeChat saves its
# session (buffers, lines, connections) before exiting; next start restores
# it. Falls back to tmux kill-session if the graceful stop fails.
_render_weechat_systemd_unit() {  # <tenant> <out_dir>
  local name="$1" out_dir="$2"
  local weechat_home env_dir start_helper stop_helper weechat_binary
  weechat_home="$(tenant_weechat_home "$name")"
  env_dir="$(tenant_env_dir "$name")"
  start_helper="$(tenant_lw_root "$name")/ic/scripts/lunarwing-weechat-start.sh"
  stop_helper="$(tenant_lw_root "$name")/ic/scripts/lunarwing-weechat-stop.sh"
  weechat_binary="$(command -v weechat)"
  cat >"$out_dir/lunarwing-weechat-${name}.service" <<EOF
[Unit]
Description=WeeChat IRC client ($name)
After=network.target

[Service]
Type=forking
ExecStart=$(command -v tmux) -L weechat-${name} new-session -d -s weechat '$start_helper --weechat-bin $weechat_binary --weechat-home ${weechat_home}'
ExecStop=$stop_helper --weechat-home ${weechat_home} --tmux-socket weechat-${name} --session weechat
TimeoutStopSec=30
EnvironmentFile=$env_dir/weechat.env
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
EOF
}

render_tenant_systemd_units() {
  local name="$1"
  ensure_container_runtime
  local user_unit_dir
  user_unit_dir="$(tenant_home "$name")/.config/systemd/user"
  mkdir -p "$user_unit_dir"
  chown -R "$name:$name" "$(tenant_home "$name")/.config"

  local repo env_dir state_dir proxy_port bridge_port weechat_port
  repo="$(tenant_repo "$name")"
  env_dir="$(tenant_env_dir "$name")"
  state_dir="$(tenant_state_dir "$name")"
  proxy_port="$(ports_get "$name" proxy)"
  bridge_port="$(ports_get "$name" bridge)"
  weechat_port="$(ports_get "$name" weechat)"

  local proxy_bin
  # Run from the tenant's own clone (in their home), not the admin's source repo,
  # so a tenant's services aren't coupled to another user's home directory.
  proxy_bin="$(tenant_lw_root "$name")/tensorzero-proxy-configurations/lunarwing-proxy.py"

  local ws_adapter_path
  ws_adapter_path="$(tenant_lw_root "$name")/lunarwing_weechat_wss/weechat_relay/ws_adapter.py"
  local weechat_enabled=false
  tenant_weechat_enabled "$name" && weechat_enabled=true

  # Proxy unit (only when enabled)
  if tenant_proxy_enabled "$name"; then
  cat >"$user_unit_dir/lunarwing-proxy-${name}.service" <<EOF
[Unit]
Description=LunarWing TensorZero proxy ($name)
After=network.target

[Service]
Type=simple
ExecStart=$(command -v python3) $proxy_bin --port $proxy_port --bind 127.0.0.1 --tensorzero $DEFAULT_TENSORZERO_URL
EnvironmentFile=$env_dir/proxy.env
Restart=on-failure
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=default.target
EOF
  fi

  if [[ "$weechat_enabled" == "true" ]]; then
    # WeeChat unit (runs in tmux so you can attach: tmux -L weechat-${name} attach)
    local weechat_home
    weechat_home="$(tenant_home "$name")/.config/weechat"
    mkdir -p "$weechat_home"
    chown "$name:$name" "$weechat_home"

    _render_weechat_systemd_unit "$name" "$user_unit_dir"

    # WeeChat WS adapter unit
    cat >"$user_unit_dir/lunarwing-weechat-adapter-${name}.service" <<EOF
[Unit]
Description=LunarWing WeeChat WS adapter ($name)
After=network.target lunarwing-weechat-${name}.service
Requires=lunarwing-weechat-${name}.service
PartOf=lunarwing-${name}.service

[Service]
Type=simple
WorkingDirectory=$(dirname "$ws_adapter_path")
EnvironmentFile=$env_dir/lunarwing.env
ExecStart=$(command -v python3) $ws_adapter_path
Restart=on-failure
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=default.target
EOF
  else
    # Remove stale files and enablement links so render-units also reconciles a
    # tenant whose registry flag was changed outside the lifecycle command.
    rm -f \
      "$user_unit_dir/lunarwing-weechat-${name}.service" \
      "$user_unit_dir/lunarwing-weechat-adapter-${name}.service" \
      "$user_unit_dir/default.target.wants/lunarwing-weechat-${name}.service" \
      "$user_unit_dir/default.target.wants/lunarwing-weechat-adapter-${name}.service"
  fi

  if tenant_darkirc_enabled "$name"; then
    local darkirc_adapter_path
    darkirc_adapter_path="$(tenant_lw_root "$name")/darkirc_channel_for_lunarwing/darkirc/adapter/darkirc_adapter.py"

    cat >"$user_unit_dir/lunarwing-darkirc-adapter-${name}.service" <<EOF
[Unit]
Description=LunarWing DarkIRC adapter ($name)
After=network.target

[Service]
Type=simple
WorkingDirectory=$(dirname "$darkirc_adapter_path")
EnvironmentFile=$env_dir/darkirc-adapter.env
ExecStart=$(command -v python3) $darkirc_adapter_path
Restart=on-failure
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=default.target
EOF

    # DarkIRC daemon unit
    cat >"$user_unit_dir/lunarwing-darkirc-${name}.service" <<EOF
[Unit]
Description=DarkIRC daemon ($name)
After=network.target

[Service]
Type=simple
ExecStart=$DARKIRC_BIN --config $(tenant_state_dir "$name")/darkirc/darkirc_config.toml
Restart=always
RestartSec=10
NoNewPrivileges=true
UMask=0077

[Install]
WantedBy=default.target
EOF
  fi

  # Bridge unit
  cat >"$user_unit_dir/xmpp-bridge-${name}.service" <<EOF
[Unit]
Description=LunarWing XMPP bridge ($name)
After=network.target
PartOf=lunarwing-${name}.service

[Service]
Type=simple
WorkingDirectory=$repo/bridges/xmpp-bridge
EnvironmentFile=$env_dir/xmpp-bridge.env
ExecStart=$repo/bridges/xmpp-bridge/target/${PROFILE}/xmpp-bridge
Restart=on-failure
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=default.target
EOF

  # Postgres dependency — only when the pg Quadlet is rendered (systemd + rootless
  # podman with Quadlet support). Mirrors the OpenRC `need lunarwing-pg-<t>`.
  # Rootful docker has no pg unit (the container survives via --restart), so the
  # dependency is omitted there to avoid a Requires on a non-existent unit.
  local pg_dep_after="" pg_dep_requires=""
  if [[ "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
    pg_dep_after="lunarwing-pg-${name}.service "
    pg_dep_requires="Requires=lunarwing-pg-${name}.service"
  fi

  local darkirc_sd_after="" darkirc_sd_wants=""
  if tenant_darkirc_enabled "$name"; then
    darkirc_sd_after=" lunarwing-darkirc-adapter-${name}.service"
    darkirc_sd_wants=" lunarwing-darkirc-adapter-${name}.service"
  fi

  local proxy_sd_after="" proxy_sd_wants=""
  if tenant_proxy_enabled "$name"; then
    proxy_sd_after=" lunarwing-proxy-${name}.service"
    proxy_sd_wants=" lunarwing-proxy-${name}.service"
  fi

  local weechat_sd_after="" weechat_sd_wants=""
  if [[ "$weechat_enabled" == "true" ]]; then
    weechat_sd_after=" lunarwing-weechat-${name}.service lunarwing-weechat-adapter-${name}.service"
    weechat_sd_wants=" lunarwing-weechat-${name}.service lunarwing-weechat-adapter-${name}.service"
  fi

  # Main daemon unit
  cat >"$user_unit_dir/lunarwing-${name}.service" <<EOF
[Unit]
Description=LunarWing AI assistant ($name)
After=network.target ${pg_dep_after}xmpp-bridge-${name}.service${proxy_sd_after}${weechat_sd_after}${darkirc_sd_after}
Wants=xmpp-bridge-${name}.service${proxy_sd_wants}${weechat_sd_wants}${darkirc_sd_wants}
${pg_dep_requires}

[Service]
Type=simple
WorkingDirectory=$repo
EnvironmentFile=$env_dir/lunarwing.env
Environment=PATH=/usr/local/bin:/usr/bin:/bin:/home/${name}/.cargo/bin
ExecStart=$repo/target/${PROFILE}/lunarwing --no-onboard run
Restart=always
RestartSec=5
TimeoutStartSec=60
TimeoutStopSec=30
KillSignal=SIGTERM
UMask=0077
NoNewPrivileges=true
PrivateTmp=true
# BUG-005: the daemon is capped separately from its transient workloads;
# shell/compile jobs run in lunarwing-workloads.slice scopes (see
# ic/src/tools/process.rs), so their memory never lands here.
MemoryHigh=$DAEMON_MEMORY_HIGH
MemoryMax=$DAEMON_MEMORY_MAX
TasksMax=$DAEMON_TASKS_MAX

[Install]
WantedBy=default.target
EOF

  # BUG-005: transient workload slices. Agent-spawned shell/compile jobs run
  # as scopes under lunarwing-workloads.slice; operator-driven builds
  # (build_tenant) run under lunarwing-build.slice. Slice names are constant
  # across tenants because each tenant has its own systemd user manager.
  cat >"$user_unit_dir/$WORKLOAD_SLICE" <<EOF
[Unit]
Description=LunarWing transient agent workloads ($name)

[Slice]
MemoryHigh=$WORKLOAD_MEMORY_HIGH
MemoryMax=$WORKLOAD_MEMORY_MAX
CPUQuota=$WORKLOAD_CPU_QUOTA
TasksMax=$WORKLOAD_TASKS_MAX
EOF

  cat >"$user_unit_dir/$BUILD_SLICE" <<EOF
[Unit]
Description=LunarWing operator build workloads ($name)

[Slice]
MemoryHigh=$BUILD_MEMORY_HIGH
MemoryMax=$BUILD_MEMORY_MAX
EOF

  # The pg + worker Quadlet .container units are rendered by the start functions
  # (start_tenant_postgres / start_tenant_<worker>), NOT here: those guard on
  # image availability, so a not-yet-built worker image never produces a unit
  # that would crash-loop at boot (Restart=always). The daemon's Requires= above
  # still resolves because start_tenant_postgres renders + starts pg first.

  chown -R "$name:$name" "$user_unit_dir"
  say "rendered systemd units for $name in $user_unit_dir"
}

_systemctl_user() {
  local name="$1"
  shift
  local uid
  uid="$(id -u "$name")"
  sudo -u "$name" XDG_RUNTIME_DIR="/run/user/$uid" systemctl --user "$@"
}

# Best-effort wait for the tenant's `systemd --user` manager + bus to be ready,
# so `systemctl --user` and the Quadlet generator work right after enable-linger
# (which can return before the user manager is fully up). Proceeds after ~10s.
_wait_user_manager() {
  local name="$1" uid i
  uid="$(id -u "$name" 2>/dev/null)" || return 0
  for i in $(seq 1 20); do
    [[ -S "/run/user/$uid/bus" ]] && return 0
    sleep 0.5
  done
  return 0
}

start_tenant_systemd() {
  local name="$1"
  _systemctl_user "$name" daemon-reload
  # Quadlet-generated units (pg, supported workers, vision) are NOT in the
  # enable_list: `systemctl enable` fails on generated/transient units with
  # "Failed to enable unit: ... is transient or generated", and under
  # `set -euo pipefail` that non-zero exit aborts the entire enable batch —
  # leaving every regular service disabled. Each Quadlet unit is already
  # started by its own start_tenant_* function (which renders the quadlet,
  # reloads the daemon, and starts the unit). The imperative `start` below
  # is belt-and-suspenders for the vision unit (harmless if already running).
  local enable_list=("lunarwing-${name}.service" "xmpp-bridge-${name}.service")
  # WeeChat units join the enable list only when the tenant's registry flag
  # allows them (ISSUE-006): a disabled tenant must not be re-enabled for
  # future boots by start-tenant.
  if tenant_weechat_enabled "$name"; then
    enable_list+=("lunarwing-weechat-${name}.service" "lunarwing-weechat-adapter-${name}.service")
  fi
  if tenant_proxy_enabled "$name"; then
    enable_list+=("lunarwing-proxy-${name}.service")
  fi
  if tenant_darkirc_enabled "$name"; then
    enable_list+=("lunarwing-darkirc-${name}.service" "lunarwing-darkirc-adapter-${name}.service")
  fi
  _systemctl_user "$name" enable "${enable_list[@]}"
  if tenant_darkirc_enabled "$name"; then
    _systemctl_user "$name" start "lunarwing-darkirc-${name}.service"
    sleep 2
  fi
  _systemctl_user "$name" start "lunarwing-vision-${name}.service" 2>/dev/null || true
  _systemctl_user "$name" start "lunarwing-${name}.service"
  sleep 2
  if _systemctl_user "$name" is-active --quiet "lunarwing-${name}.service"; then
    say "lunarwing-${name}.service is active"
  else
    say "WARNING: lunarwing-${name}.service failed to start" >&2
    _systemctl_user "$name" status "lunarwing-${name}.service" --no-pager >&2 || true
    return 1
  fi
}

stop_tenant_systemd() {
  local name="$1"
  local uid
  uid="$(id -u "$name" 2>/dev/null)" || return 0

  for svc in "lunarwing-${name}.service" "xmpp-bridge-${name}.service" "lunarwing-proxy-${name}.service" "lunarwing-weechat-adapter-${name}.service" "lunarwing-weechat-${name}.service" "lunarwing-darkirc-adapter-${name}.service" "lunarwing-darkirc-${name}.service" "lunarwing-nanocode-${name}.service" "lunarwing-pebble-${name}.service" "lunarwing-opencode-${name}.service" "lunarwing-kimi-${name}.service" "lunarwing-vision-${name}.service" "lunarwing-pg-${name}.service"; do
    if _systemctl_user "$name" is-active --quiet "$svc" 2>/dev/null; then
      _systemctl_user "$name" stop "$svc"
      say "stopped $svc"
    fi
  done
}

uninstall_tenant_systemd() {
  local name="$1"
  local user_unit_dir
  user_unit_dir="$(tenant_home "$name")/.config/systemd/user"

  for svc in "lunarwing-${name}.service" "xmpp-bridge-${name}.service" "lunarwing-proxy-${name}.service" "lunarwing-weechat-adapter-${name}.service" "lunarwing-weechat-${name}.service" "lunarwing-darkirc-adapter-${name}.service" "lunarwing-darkirc-${name}.service"; do
    rm -f "$user_unit_dir/$svc"
  done

  # Quadlet .container units (rootless pg + workers + vision sidecar). Remove the
  # worker containers (their workspace data is bind-mounted in the home); the pg
  # container + named volume are handled by stop_tenant_postgres /
  # reset_tenant_postgres so non-purge removals keep the data for a later re-add.
  local qdir; qdir="$(tenant_quadlet_dir "$name")"
  rm -f "$qdir/lunarwing-pg-${name}.container" \
        "$qdir/lunarwing-nanocode-${name}.container" \
        "$qdir/lunarwing-pebble-${name}.container" \
        "$qdir/lunarwing-opencode-${name}.container" \
        "$qdir/lunarwing-kimi-${name}.container" \
        "$qdir/lunarwing-vision-${name}.container"
  if id -u "$name" >/dev/null 2>&1; then
    # Retirement preserves stopped Nanocode containers for rollback and manual
    # recovery. Supported workers remain disposable because their state lives
    # in bind-mounted workspaces.
    for w in pebble opencode kimi; do
      _ctr "$name" rm -f "lunarwing-${w}-${name}" >/dev/null 2>&1 || true
    done
    _ctr "$name" rm -f "lunarwing-vision-${name}" >/dev/null 2>&1 || true
  fi

  _systemctl_user "$name" daemon-reload 2>/dev/null || true
  say "uninstalled systemd units for $name"
}

# Render the WeeChat OpenRC init script into <out_file>.
_render_weechat_openrc_unit() {  # <tenant> <out_file>
  local name="$1" out_file="$2"
  local weechat_home env_dir run_dir log_dir
  weechat_home="$(tenant_weechat_home "$name")"
  env_dir="$(tenant_env_dir "$name")"
  run_dir="$(tenant_run_dir "$name")"
  log_dir="$(tenant_log_dir "$name")"
  local start_helper stop_helper
  start_helper="$(tenant_lw_root "$name")/ic/scripts/lunarwing-weechat-start.sh"
  stop_helper="$(tenant_lw_root "$name")/ic/scripts/lunarwing-weechat-stop.sh"
  cat >"$out_file" <<INITEOF
#!/sbin/openrc-run

description="WeeChat IRC client ($name)"

: "\${weechat_user:=$name}"
: "\${weechat_group:=$name}"
: "\${weechat_home:=$weechat_home}"
: "\${weechat_pidfile:=$run_dir/weechat.pid}"
: "\${weechat_runtime_dir:=$run_dir}"
: "\${weechat_log_dir:=$log_dir}"
: "\${weechat_output_log:=\${weechat_log_dir}/weechat.log}"
: "\${weechat_error_log:=\${weechat_log_dir}/weechat.err}"
: "\${weechat_retry:=SIGTERM/30/KILL/5}"
: "\${weechat_env_file:=$env_dir/weechat.env}"
: "\${weechat_openrc_env_exec:=$OPENRC_ENV_EXEC}"
: "\${weechat_command:=$(command -v tmux)}"
: "\${weechat_binary:=$(command -v weechat)}"
: "\${weechat_start_helper:=$start_helper}"
: "\${weechat_stop_helper:=$stop_helper}"

command="\${weechat_openrc_env_exec}"
command_args="--env-file \${weechat_env_file} -- \${weechat_command} -L weechat-${name} new-session -d -s weechat '\${weechat_start_helper} --weechat-bin \${weechat_binary} --weechat-home \${weechat_home}'"
command_user="\${weechat_user}:\${weechat_group}"
required_files="\${weechat_openrc_env_exec} \${weechat_command} \${weechat_binary} \${weechat_start_helper} \${weechat_env_file}"

depend() {
    need net
    use dns
    after firewall
    before lunarwing-weechat-adapter-${name} lunarwing-${name}
}

start() {
    ebegin "Starting WeeChat ($name)"
    checkpath -d -m 0750 -o "\${weechat_user}:\${weechat_group}" "\${weechat_home}"
    checkpath -d -m 0750 -o "\${weechat_user}:\${weechat_group}" "\${weechat_runtime_dir}"
    start-stop-daemon --start --background --user "\${weechat_user}" \\
        --exec "\${weechat_openrc_env_exec}" -- \\
        --env-file "\${weechat_env_file}" -- "\${weechat_command}" \\
        -L weechat-${name} new-session -d -s weechat \
        "\${weechat_start_helper} --weechat-bin \${weechat_binary} --weechat-home \${weechat_home}"
    eend \$?
}

stop() {
    ebegin "Stopping WeeChat ($name)"
    # Graceful: /upgrade -quit via FIFO saves buffers before exit; falls back
    # to tmux kill-session if the FIFO is unavailable or times out. Must run
    # under the tenant UID because tmux sockets are UID-scoped. --chuid drops
    # privileges before exec; --user only filters by owner and would leave the
    # helper running as root, unable to reach the tenant's tmux socket.
    if [ -x "\${weechat_stop_helper}" ]; then
        (
            unset RC_SVCNAME
            start-stop-daemon --start \\
                --chuid "\${weechat_user}:\${weechat_group}" \\
                --exec "\${weechat_stop_helper}" -- \\
                --weechat-home "\${weechat_home}" \\
                --tmux-socket "weechat-${name}" \\
                --session weechat \\
                --timeout 20
        )
        stop_rc=\$?
    else
        (
            unset RC_SVCNAME
            start-stop-daemon --start \\
                --chuid "\${weechat_user}:\${weechat_group}" \\
                --exec "\${weechat_command}" -- \\
                -L "weechat-${name}" kill-session -t weechat
        ) >/dev/null 2>&1
        stop_rc=\$?
    fi
    eend "\${stop_rc}"
    return "\${stop_rc}"
}
INITEOF
  chmod 0755 "$out_file"
}

# ── OpenRC service units ─────────────────────────────────────────────────────

install_openrc_env_exec() {
  local install_dir owner group mode links
  [[ -f "$OPENRC_ENV_EXEC_SRC" && ! -L "$OPENRC_ENV_EXEC_SRC" ]] \
    || die "OpenRC tenant env launcher is missing: $OPENRC_ENV_EXEC_SRC"

  install_dir="$(dirname "$OPENRC_ENV_EXEC")"
  install -d -o root -g root -m 0755 "$install_dir" \
    || die "failed to prepare OpenRC tenant env launcher directory"
  [[ -d "$install_dir" && ! -L "$install_dir" ]] \
    || die "unsafe OpenRC tenant env launcher directory: $install_dir"
  owner="$(stat -c '%u' "$install_dir" 2>/dev/null || true)"
  group="$(stat -c '%g' "$install_dir" 2>/dev/null || true)"
  mode="$(stat -c '%a' "$install_dir" 2>/dev/null || true)"
  [[ "$owner" == 0 && "$group" == 0 && "$mode" == 755 ]] \
    || die "OpenRC tenant env launcher directory must be root:root mode 0755"

  install -o root -g root -m 0755 "$OPENRC_ENV_EXEC_SRC" "$OPENRC_ENV_EXEC" \
    || die "failed to install OpenRC tenant env launcher"
  [[ -f "$OPENRC_ENV_EXEC" && ! -L "$OPENRC_ENV_EXEC" && -x "$OPENRC_ENV_EXEC" ]] \
    || die "unsafe OpenRC tenant env launcher: $OPENRC_ENV_EXEC"
  owner="$(stat -c '%u' "$OPENRC_ENV_EXEC" 2>/dev/null || true)"
  group="$(stat -c '%g' "$OPENRC_ENV_EXEC" 2>/dev/null || true)"
  mode="$(stat -c '%a' "$OPENRC_ENV_EXEC" 2>/dev/null || true)"
  links="$(stat -c '%h' "$OPENRC_ENV_EXEC" 2>/dev/null || true)"
  [[ "$owner" == 0 && "$group" == 0 && "$mode" == 755 && "$links" == 1 ]] \
    || die "OpenRC tenant env launcher must be root:root, single-link mode 0755"
}

render_tenant_openrc_units() {
  local name="$1"
  local repo env_dir state_dir log_dir run_dir
  repo="$(tenant_repo "$name")"
  env_dir="$(tenant_env_dir "$name")"
  state_dir="$(tenant_state_dir "$name")"
  log_dir="$(tenant_log_dir "$name")"
  run_dir="$(tenant_run_dir "$name")"
  install_openrc_env_exec
  mkdir -p "$OPENRC_INITD_DIR" "$OPENRC_CONFD_DIR"

  local proxy_port bridge_port weechat_port
  proxy_port="$(ports_get "$name" proxy)"
  bridge_port="$(ports_get "$name" bridge)"
  weechat_port="$(ports_get "$name" weechat)"

  local proxy_bin
  # Run from the tenant's own clone (in their home), not the admin's source repo,
  # so a tenant's services aren't coupled to another user's home directory.
  proxy_bin="$(tenant_lw_root "$name")/tensorzero-proxy-configurations/lunarwing-proxy.py"

  local ws_adapter_path
  ws_adapter_path="$(tenant_lw_root "$name")/lunarwing_weechat_wss/weechat_relay/ws_adapter.py"
  local ws_adapter_dir
  ws_adapter_dir="$(dirname "$ws_adapter_path")"
  local weechat_enabled=false
  tenant_weechat_enabled "$name" && weechat_enabled=true

  local darkirc_rc_after=""
  if tenant_darkirc_enabled "$name"; then
    darkirc_rc_after=" lunarwing-darkirc-adapter-${name}"
  fi

  local proxy_rc_after=""
  if tenant_proxy_enabled "$name"; then
    proxy_rc_after=" lunarwing-proxy-${name}"
  fi

  local weechat_rc_after=""
  if [[ "$weechat_enabled" == "true" ]]; then
    weechat_rc_after=" lunarwing-weechat-${name} lunarwing-weechat-adapter-${name}"
  fi

  local weechat_home
  weechat_home="$(tenant_home "$name")/.config/weechat"

  # Resolve the container runtime path + tenant identity so the dedicated
  # Postgres init service can bring the container up (rootless: as the tenant
  # user; rootful docker: as root). Empty runtime -> the pg service no-ops.
  ensure_container_runtime
  local pg_runtime_bin="" pg_container="lunarwing-pg-$name"
  [[ -n "${CONTAINER_RT:-}" ]] && pg_runtime_bin="$(command -v "$CONTAINER_RT" 2>/dev/null || true)"
  local pg_uid pg_home
  pg_uid="$(id -u "$name" 2>/dev/null || echo "")"
  pg_home="$(tenant_home "$name")"

  # ── Postgres container init script (dedicated service; the daemon needs it) ──
  # A first-class unit (not a daemon start_pre side-effect) so the host self-heal
  # pipeline — which auto-discovers /etc/init.d units — can remediate a crashed
  # Postgres independently.
  cat >"$OPENRC_INITD_DIR/lunarwing-pg-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing Postgres container ($name)"

: "\${pg_runtime:=$pg_runtime_bin}"
: "\${pg_container:=$pg_container}"
: "\${pg_rootless:=$MT_ROOTLESS}"
: "\${pg_user:=$name}"
: "\${pg_home:=$pg_home}"
: "\${pg_uid:=$pg_uid}"
: "\${pg_wait:=60}"

depend() {
    need net localmount
    after firewall
    before lunarwing-${name}
}

# Run the container runtime for this tenant's container: rootless -> as the
# tenant user with their runtime dir + HOME; rootful -> as root unchanged.
_pg() {
    if [ "\${pg_rootless}" = "true" ]; then
        sudo -u "\${pg_user}" env HOME="\${pg_home}" XDG_RUNTIME_DIR="/run/user/\${pg_uid}" "\${pg_runtime}" "\$@"
    else
        "\${pg_runtime}" "\$@"
    fi
}

start() {
    [ -n "\${pg_runtime}" ] && [ -x "\${pg_runtime}" ] || { ewarn "no container runtime; skipping Postgres for $name"; return 0; }
    ebegin "Starting Postgres container (\${pg_container})"
    if [ "\${pg_rootless}" = "true" ]; then
        checkpath -d -m 0700 -o "\${pg_user}:\${pg_user}" "/run/user/\${pg_uid}"
    fi
    _pg start "\${pg_container}" >/dev/null 2>&1 || { eend 1 "container start failed"; return 1; }
    _w=0
    while ! _pg exec "\${pg_container}" pg_isready -U lunarwing -q 2>/dev/null; do
        _w=\$((_w + 1))
        [ "\$_w" -lt "\${pg_wait}" ] || { eend 1 "Postgres not ready after \${pg_wait}s"; return 1; }
        sleep 1
    done
    eend 0
}

stop() {
    [ -n "\${pg_runtime}" ] && [ -x "\${pg_runtime}" ] || return 0
    ebegin "Stopping Postgres container (\${pg_container})"
    _pg stop --time 30 "\${pg_container}" >/dev/null 2>&1
    eend 0
}

status() {
    # Emit the standard OpenRC "started"/"stopped" wording (not "running") so the
    # health-check parser (grep started|stopped) and the mt-admin status display
    # classify the container correctly instead of relying on the rc_exit fallback.
    # "started" requires the container be running AND Postgres actually accept
    # connections (pg_isready) — a Running-but-wedged DB (crash recovery, disk
    # full, max_connections) otherwise reports healthy and is never remediated.
    # Mirrors the worker units' _wk_healthy and this unit's own start() gate.
    if [ "\$(_pg inspect -f '{{.State.Running}}' "\${pg_container}" 2>/dev/null)" = "true" ] \\
       && _pg exec "\${pg_container}" pg_isready -U lunarwing -q -t 3 2>/dev/null; then
        einfo "\${pg_container}: started"; return 0
    fi
    einfo "\${pg_container}: stopped"; return 3
}
INITEOF
  chmod 0755 "$OPENRC_INITD_DIR/lunarwing-pg-${name}"
  render_container_babysitter_unit "$name" pg "$pg_container" "$pg_uid" "$pg_home"

  # ── Main daemon init script ──
  cat >"$OPENRC_INITD_DIR/lunarwing-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing AI assistant ($name)"

: "\${lunarwing_command:=$repo/target/${PROFILE}/lunarwing}"
: "\${lunarwing_args:=--no-onboard run}"
: "\${lunarwing_user:=$name}"
: "\${lunarwing_group:=$name}"
: "\${lunarwing_workdir:=$repo}"
: "\${lunarwing_pidfile:=$run_dir/lunarwing.pid}"
: "\${lunarwing_state_dir:=$state_dir}"
: "\${lunarwing_runtime_dir:=$run_dir}"
: "\${lunarwing_log_dir:=$log_dir}"
: "\${lunarwing_output_log:=\${lunarwing_log_dir}/lunarwing.log}"
: "\${lunarwing_error_log:=\${lunarwing_log_dir}/lunarwing.err}"
: "\${lunarwing_env_file:=$env_dir/lunarwing.env}"
: "\${lunarwing_openrc_env_exec:=$OPENRC_ENV_EXEC}"
: "\${lunarwing_umask:=0077}"
: "\${lunarwing_respawn_delay:=5}"
: "\${lunarwing_respawn_max:=5}"
: "\${lunarwing_respawn_period:=60}"
: "\${lunarwing_retry:=SIGTERM/30/KILL/5}"

command="\${lunarwing_openrc_env_exec}"
command_args="--env-file \${lunarwing_env_file} -- \${lunarwing_command} \${lunarwing_args}"
command_user="\${lunarwing_user}:\${lunarwing_group}"
directory="\${lunarwing_workdir}"
pidfile="\${lunarwing_pidfile}"
supervisor="supervise-daemon"
retry="\${lunarwing_retry}"
respawn_delay="\${lunarwing_respawn_delay}"
respawn_max="\${lunarwing_respawn_max}"
respawn_period="\${lunarwing_respawn_period}"
output_log="\${lunarwing_output_log}"
error_log="\${lunarwing_error_log}"
required_files="\${lunarwing_openrc_env_exec} \${lunarwing_command} \${lunarwing_env_file}"

depend() {
    need net localmount lunarwing-pg-${name}
    want \${lunarwing_rc_need}
    use dns logger
    after firewall lunarwing-pg-${name} xmpp-bridge-${name}${proxy_rc_after}${weechat_rc_after}${darkirc_rc_after}
}

start_pre() {
    checkpath -d -m 0750 -o "\${lunarwing_user}:\${lunarwing_group}" "\${lunarwing_state_dir}"
    checkpath -d -m 0750 -o "\${lunarwing_user}:\${lunarwing_group}" "\${lunarwing_log_dir}"
    checkpath -d -m 0750 -o "\${lunarwing_user}:\${lunarwing_group}" "\${lunarwing_runtime_dir}"
    checkpath -f -m 0640 -o "\${lunarwing_user}:\${lunarwing_group}" "\${output_log}"
    checkpath -f -m 0640 -o "\${lunarwing_user}:\${lunarwing_group}" "\${error_log}"
    # Postgres is brought up by the dedicated lunarwing-pg-${name} service, which
    # this unit declares as a hard dependency (need), so the DB is already up.
    umask "\${lunarwing_umask}"
}
INITEOF
  chmod 0755 "$OPENRC_INITD_DIR/lunarwing-${name}"

  # ── XMPP bridge init script ──
  cat >"$OPENRC_INITD_DIR/xmpp-bridge-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing XMPP bridge ($name)"

: "\${xmpp_bridge_command:=$repo/bridges/xmpp-bridge/target/${PROFILE}/xmpp-bridge}"
: "\${xmpp_bridge_user:=$name}"
: "\${xmpp_bridge_group:=$name}"
: "\${xmpp_bridge_workdir:=$repo/bridges/xmpp-bridge}"
: "\${xmpp_bridge_pidfile:=$run_dir/xmpp-bridge.pid}"
: "\${xmpp_bridge_state_dir:=$state_dir}"
: "\${xmpp_bridge_runtime_dir:=$run_dir}"
: "\${xmpp_bridge_log_dir:=$log_dir}"
: "\${xmpp_bridge_output_log:=\${xmpp_bridge_log_dir}/xmpp-bridge.log}"
: "\${xmpp_bridge_error_log:=\${xmpp_bridge_log_dir}/xmpp-bridge.err}"
: "\${xmpp_bridge_env_file:=$env_dir/xmpp-bridge.env}"
: "\${xmpp_bridge_openrc_env_exec:=$OPENRC_ENV_EXEC}"
: "\${xmpp_bridge_umask:=0077}"
: "\${xmpp_bridge_respawn_delay:=5}"
: "\${xmpp_bridge_respawn_max:=5}"
: "\${xmpp_bridge_respawn_period:=60}"
: "\${xmpp_bridge_retry:=SIGTERM/30/KILL/5}"

command="\${xmpp_bridge_openrc_env_exec}"
command_args="--env-file \${xmpp_bridge_env_file} -- \${xmpp_bridge_command}"
command_user="\${xmpp_bridge_user}:\${xmpp_bridge_group}"
directory="\${xmpp_bridge_workdir}"
pidfile="\${xmpp_bridge_pidfile}"
supervisor="supervise-daemon"
retry="\${xmpp_bridge_retry}"
respawn_delay="\${xmpp_bridge_respawn_delay}"
respawn_max="\${xmpp_bridge_respawn_max}"
respawn_period="\${xmpp_bridge_respawn_period}"
output_log="\${xmpp_bridge_output_log}"
error_log="\${xmpp_bridge_error_log}"
required_files="\${xmpp_bridge_openrc_env_exec} \${xmpp_bridge_command} \${xmpp_bridge_env_file}"

depend() {
    need net localmount
    use dns logger
    after firewall
    before lunarwing-${name}
}

start_pre() {
    checkpath -d -m 0750 -o "\${xmpp_bridge_user}:\${xmpp_bridge_group}" "\${xmpp_bridge_state_dir}"
    checkpath -d -m 0750 -o "\${xmpp_bridge_user}:\${xmpp_bridge_group}" "\${xmpp_bridge_log_dir}"
    checkpath -d -m 0750 -o "\${xmpp_bridge_user}:\${xmpp_bridge_group}" "\${xmpp_bridge_runtime_dir}"
    checkpath -f -m 0640 -o "\${xmpp_bridge_user}:\${xmpp_bridge_group}" "\${output_log}"
    checkpath -f -m 0640 -o "\${xmpp_bridge_user}:\${xmpp_bridge_group}" "\${error_log}"
    umask "\${xmpp_bridge_umask}"
}
INITEOF
  chmod 0755 "$OPENRC_INITD_DIR/xmpp-bridge-${name}"

  # ── DarkIRC daemon init script (only when enabled) ──
  if tenant_darkirc_enabled "$name"; then
  cat >"$OPENRC_INITD_DIR/lunarwing-darkirc-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing DarkIRC daemon ($name)"

: "\${darkirc_command:=$DARKIRC_BIN}"
: "\${darkirc_args:=--config $state_dir/darkirc/darkirc_config.toml}"
: "\${darkirc_user:=$name}"
: "\${darkirc_group:=$name}"
: "\${darkirc_workdir:=$state_dir/darkirc}"
: "\${darkirc_pidfile:=$run_dir/darkirc.pid}"
: "\${darkirc_state_dir:=$state_dir}"
: "\${darkirc_runtime_dir:=$run_dir}"
: "\${darkirc_log_dir:=$log_dir}"
: "\${darkirc_output_log:=\${darkirc_log_dir}/darkirc.log}"
: "\${darkirc_error_log:=\${darkirc_log_dir}/darkirc.err}"
: "\${darkirc_umask:=0077}"
: "\${darkirc_respawn_delay:=5}"
: "\${darkirc_respawn_max:=5}"
: "\${darkirc_respawn_period:=60}"
: "\${darkirc_retry:=SIGTERM/30/KILL/5}"

command="\${darkirc_command}"
command_args="\${darkirc_args}"
command_user="\${darkirc_user}:\${darkirc_group}"
directory="\${darkirc_workdir}"
pidfile="\${darkirc_pidfile}"
supervisor="supervise-daemon"
retry="\${darkirc_retry}"
respawn_delay="\${darkirc_respawn_delay}"
respawn_max="\${darkirc_respawn_max}"
respawn_period="\${darkirc_respawn_period}"
output_log="\${darkirc_output_log}"
error_log="\${darkirc_error_log}"
required_files="\${command}"

depend() {
    need net localmount
    use dns logger
    after firewall
    before lunarwing-darkirc-adapter-${name}
}

start_pre() {
    checkpath -d -m 0750 -o "\${darkirc_user}:\${darkirc_group}" "\${darkirc_state_dir}/darkirc"
    checkpath -d -m 0750 -o "\${darkirc_user}:\${darkirc_group}" "\${darkirc_log_dir}"
    checkpath -d -m 0750 -o "\${darkirc_user}:\${darkirc_group}" "\${darkirc_runtime_dir}"
    checkpath -f -m 0640 -o "\${darkirc_user}:\${darkirc_group}" "\${output_log}"
    checkpath -f -m 0640 -o "\${darkirc_user}:\${darkirc_group}" "\${error_log}"
    umask "\${darkirc_umask}"
}
INITEOF
  chmod 0755 "$OPENRC_INITD_DIR/lunarwing-darkirc-${name}"
  fi

  # ── TensorZero proxy init script (only when enabled) ──
  if tenant_proxy_enabled "$name"; then
  cat >"$OPENRC_INITD_DIR/lunarwing-proxy-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing TensorZero proxy ($name)"

: "\${proxy_command:=$(command -v python3)}"
: "\${proxy_args:=$proxy_bin --port $proxy_port --bind 127.0.0.1 --tensorzero $DEFAULT_TENSORZERO_URL}"
: "\${proxy_user:=$name}"
: "\${proxy_group:=$name}"
: "\${proxy_pidfile:=$run_dir/proxy.pid}"
: "\${proxy_runtime_dir:=$run_dir}"
: "\${proxy_log_dir:=$log_dir}"
: "\${proxy_output_log:=\${proxy_log_dir}/proxy.log}"
: "\${proxy_error_log:=\${proxy_log_dir}/proxy.err}"
: "\${proxy_env_file:=$env_dir/proxy.env}"
: "\${proxy_openrc_env_exec:=$OPENRC_ENV_EXEC}"
: "\${proxy_umask:=0077}"
: "\${proxy_respawn_delay:=5}"
: "\${proxy_respawn_max:=5}"
: "\${proxy_respawn_period:=60}"
: "\${proxy_retry:=SIGTERM/30/KILL/5}"

command="\${proxy_openrc_env_exec}"
command_args="--env-file \${proxy_env_file} -- \${proxy_command} \${proxy_args}"
command_user="\${proxy_user}:\${proxy_group}"
pidfile="\${proxy_pidfile}"
supervisor="supervise-daemon"
retry="\${proxy_retry}"
respawn_delay="\${proxy_respawn_delay}"
respawn_max="\${proxy_respawn_max}"
respawn_period="\${proxy_respawn_period}"
output_log="\${proxy_output_log}"
error_log="\${proxy_error_log}"
required_files="\${proxy_openrc_env_exec} \${proxy_command} \${proxy_env_file}"

depend() {
    need net
    use dns
    after firewall
    before lunarwing-${name}
}

start_pre() {
    checkpath -d -m 0750 -o "\${proxy_user}:\${proxy_group}" "\${proxy_runtime_dir}"
    checkpath -d -m 0750 -o "\${proxy_user}:\${proxy_group}" "\${proxy_log_dir}"
    checkpath -f -m 0640 -o "\${proxy_user}:\${proxy_group}" "\${output_log}"
    checkpath -f -m 0640 -o "\${proxy_user}:\${proxy_group}" "\${error_log}"
    umask "\${proxy_umask}"
}
INITEOF
  chmod 0755 "$OPENRC_INITD_DIR/lunarwing-proxy-${name}"
  fi

  if [[ "$weechat_enabled" == "true" ]]; then
    # ── WeeChat init script (tmux-based) ──
    _render_weechat_openrc_unit "$name" "$OPENRC_INITD_DIR/lunarwing-weechat-${name}"

    # WeeChat WS adapter init script
    cat >"$OPENRC_INITD_DIR/lunarwing-weechat-adapter-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing WeeChat WS adapter ($name)"

: "\${adapter_command:=$(command -v python3)}"
: "\${adapter_args:=$ws_adapter_path}"
: "\${adapter_user:=$name}"
: "\${adapter_group:=$name}"
: "\${adapter_pidfile:=$run_dir/weechat-adapter.pid}"
: "\${adapter_runtime_dir:=$run_dir}"
: "\${adapter_log_dir:=$log_dir}"
: "\${adapter_output_log:=\${adapter_log_dir}/weechat-adapter.log}"
: "\${adapter_error_log:=\${adapter_log_dir}/weechat-adapter.err}"
: "\${adapter_env_file:=$env_dir/lunarwing.env}"
: "\${adapter_openrc_env_exec:=$OPENRC_ENV_EXEC}"
: "\${adapter_umask:=0077}"
: "\${adapter_respawn_delay:=5}"
: "\${adapter_respawn_max:=5}"
: "\${adapter_respawn_period:=60}"
: "\${adapter_retry:=SIGTERM/30/KILL/5}"

command="\${adapter_openrc_env_exec}"
command_args="--env-file \${adapter_env_file} -- \${adapter_command} \${adapter_args}"
command_user="\${adapter_user}:\${adapter_group}"
directory="$ws_adapter_dir"
pidfile="\${adapter_pidfile}"
supervisor="supervise-daemon"
retry="\${adapter_retry}"
respawn_delay="\${adapter_respawn_delay}"
respawn_max="\${adapter_respawn_max}"
respawn_period="\${adapter_respawn_period}"
output_log="\${adapter_output_log}"
error_log="\${adapter_error_log}"
required_files="\${adapter_openrc_env_exec} \${adapter_command} \${adapter_env_file}"

depend() {
    need net lunarwing-weechat-${name}
    use dns
    after firewall lunarwing-weechat-${name}
    before lunarwing-${name}
}

start_pre() {
    checkpath -d -m 0750 -o "\${adapter_user}:\${adapter_group}" "\${adapter_runtime_dir}"
    checkpath -d -m 0750 -o "\${adapter_user}:\${adapter_group}" "\${adapter_log_dir}"
    checkpath -f -m 0640 -o "\${adapter_user}:\${adapter_group}" "\${output_log}"
    checkpath -f -m 0640 -o "\${adapter_user}:\${adapter_group}" "\${error_log}"
    umask "\${adapter_umask}"
}
INITEOF
    chmod 0755 "$OPENRC_INITD_DIR/lunarwing-weechat-adapter-${name}"
  else
    rm -f \
      "$OPENRC_INITD_DIR/lunarwing-weechat-${name}" \
      "$OPENRC_INITD_DIR/lunarwing-weechat-adapter-${name}" \
      "$OPENRC_CONFD_DIR/lunarwing-weechat-${name}" \
      "$OPENRC_CONFD_DIR/lunarwing-weechat-adapter-${name}" \
      "$OPENRC_RUNLEVELS_DIR"/*/"lunarwing-weechat-${name}" \
      "$OPENRC_RUNLEVELS_DIR"/*/"lunarwing-weechat-adapter-${name}"
  fi

  # DarkIRC adapter init script (only when enabled)
  if tenant_darkirc_enabled "$name"; then
  local darkirc_adapter_path darkirc_adapter_dir
  darkirc_adapter_path="$(tenant_lw_root "$name")/darkirc_channel_for_lunarwing/darkirc/adapter/darkirc_adapter.py"
  darkirc_adapter_dir="$(dirname "$darkirc_adapter_path")"

  cat >"$OPENRC_INITD_DIR/lunarwing-darkirc-adapter-${name}" <<INITEOF
#!/sbin/openrc-run

description="LunarWing DarkIRC adapter ($name)"

: "\${darkirc_adapter_command:=$(command -v python3)}"
: "\${darkirc_adapter_args:=$darkirc_adapter_path}"
: "\${darkirc_adapter_user:=$name}"
: "\${darkirc_adapter_group:=$name}"
: "\${darkirc_adapter_pidfile:=$run_dir/darkirc-adapter.pid}"
: "\${darkirc_adapter_runtime_dir:=$run_dir}"
: "\${darkirc_adapter_log_dir:=$log_dir}"
: "\${darkirc_adapter_output_log:=\${darkirc_adapter_log_dir}/darkirc-adapter.log}"
: "\${darkirc_adapter_error_log:=\${darkirc_adapter_log_dir}/darkirc-adapter.err}"
: "\${darkirc_adapter_env_file:=$env_dir/darkirc-adapter.env}"
: "\${darkirc_adapter_openrc_env_exec:=$OPENRC_ENV_EXEC}"
: "\${darkirc_adapter_umask:=0077}"
: "\${darkirc_adapter_respawn_delay:=5}"
: "\${darkirc_adapter_respawn_max:=5}"
: "\${darkirc_adapter_respawn_period:=60}"
: "\${darkirc_adapter_retry:=SIGTERM/30/KILL/5}"

command="\${darkirc_adapter_openrc_env_exec}"
command_args="--env-file \${darkirc_adapter_env_file} -- \${darkirc_adapter_command} \${darkirc_adapter_args}"
command_user="\${darkirc_adapter_user}:\${darkirc_adapter_group}"
directory="$darkirc_adapter_dir"
pidfile="\${darkirc_adapter_pidfile}"
supervisor="supervise-daemon"
retry="\${darkirc_adapter_retry}"
respawn_delay="\${darkirc_adapter_respawn_delay}"
respawn_max="\${darkirc_adapter_respawn_max}"
respawn_period="\${darkirc_adapter_respawn_period}"
output_log="\${darkirc_adapter_output_log}"
error_log="\${darkirc_adapter_error_log}"
required_files="\${darkirc_adapter_openrc_env_exec} \${darkirc_adapter_command} \${darkirc_adapter_env_file}"

depend() {
    need net
    use dns
    after firewall
    before lunarwing-${name}
}

start_pre() {
    checkpath -d -m 0750 -o "\${darkirc_adapter_user}:\${darkirc_adapter_group}" "\${darkirc_adapter_runtime_dir}"
    checkpath -d -m 0750 -o "\${darkirc_adapter_user}:\${darkirc_adapter_group}" "\${darkirc_adapter_log_dir}"
    checkpath -f -m 0640 -o "\${darkirc_adapter_user}:\${darkirc_adapter_group}" "\${output_log}"
    checkpath -f -m 0640 -o "\${darkirc_adapter_user}:\${darkirc_adapter_group}" "\${error_log}"
    umask "\${darkirc_adapter_umask}"
}
INITEOF
  chmod 0755 "$OPENRC_INITD_DIR/lunarwing-darkirc-adapter-${name}"
  fi

  # ── Conf.d files ──
  local darkirc_rc_need=""
  if tenant_darkirc_enabled "$name"; then
    darkirc_rc_need=" lunarwing-darkirc-adapter-${name}"
  fi
  local proxy_rc_need=""
  if tenant_proxy_enabled "$name"; then
    proxy_rc_need=" lunarwing-proxy-${name}"
  fi
  local weechat_rc_need=""
  if [[ "$weechat_enabled" == "true" ]]; then
    weechat_rc_need=" lunarwing-weechat-${name} lunarwing-weechat-adapter-${name}"
  fi
  cat >"$OPENRC_CONFD_DIR/lunarwing-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
lunarwing_rc_need="xmpp-bridge-${name}${proxy_rc_need}${weechat_rc_need}${darkirc_rc_need}"

# BUG-005 note: agent-spawned shell/compile workloads are isolated into their
# own process group and reaped on timeout/cancel by the daemon itself (see
# ic/src/tools/process.rs), so runaway trees cannot leak on any init system.
# Hard cgroup memory/CPU caps for those workloads are currently applied only
# on systemd hosts (lunarwing-workloads.slice); OpenRC has no equivalent
# transient-scope mechanism. Optional process-level caps can be added here:
#rc_ulimit="-v 8388608"
CONFD

  cat >"$OPENRC_CONFD_DIR/xmpp-bridge-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
xmpp_bridge_rc_before="lunarwing-${name}"
CONFD

  if tenant_proxy_enabled "$name"; then
    cat >"$OPENRC_CONFD_DIR/lunarwing-proxy-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
CONFD
  fi

  if [[ "$weechat_enabled" == "true" ]]; then
    cat >"$OPENRC_CONFD_DIR/lunarwing-weechat-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
CONFD

    cat >"$OPENRC_CONFD_DIR/lunarwing-weechat-adapter-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
CONFD
  fi

  if tenant_darkirc_enabled "$name"; then
    cat >"$OPENRC_CONFD_DIR/lunarwing-darkirc-adapter-${name}" <<CONFD
# Auto-generated by lunarwing-mt-admin.sh for tenant: $name
CONFD
  fi

  say "rendered OpenRC init scripts and conf.d for $name"
}

start_tenant_openrc() {
  local name="$1"
  # Record operator intent UP FRONT: enabling the primary daemon in the default
  # runlevel marks this tenant as "started" (boot-persistent) regardless of whether
  # any unit's FIRST start succeeds. health-openrc.sh's started-gate
  # (tenant_started) keys on exactly this, so a daemon that crashes on its first
  # start is still reported `critical` (a real outage) and remediated — not masked
  # as `skipped`. The selective rc-update-add loop below additionally boot-enables
  # the optional units that actually came up. Idempotent.
  rc-update add "lunarwing-${name}" default >/dev/null 2>&1 || true

  # Postgres first: the daemon `need`s it (and it's idempotent if already up).
  rc-service "lunarwing-pg-${name}" start
  # Start PG babysitter (supervises the container via podman wait)
  rc-service "lunarwing-pg-${name}-sup" start 2>/dev/null || true
  # Optional channels next, non-fatal: a missing weechat/aiohttp must not abort
  # the core stack (the main daemon does not depend on them). WeeChat units are
  # skipped entirely when the tenant's registry flag disables them (ISSUE-006):
  # start-tenant must not reactivate a service the operator turned off.
  if tenant_weechat_enabled "$name"; then
    rc-service "lunarwing-weechat-${name}" start 2>/dev/null || say "  (lunarwing-weechat-${name} skipped — optional)"
    rc-service "lunarwing-weechat-adapter-${name}" start 2>/dev/null || say "  (lunarwing-weechat-adapter-${name} skipped — optional)"
  else
    say "  (weechat disabled for $name — not starting lunarwing-weechat{,-adapter}-${name})"
  fi
  if tenant_darkirc_enabled "$name"; then
    rc-service "lunarwing-darkirc-${name}" start 2>/dev/null || say "  (lunarwing-darkirc-${name} skipped — optional)"
    rc-service "lunarwing-darkirc-adapter-${name}" start 2>/dev/null || say "  (lunarwing-darkirc-adapter-${name} skipped — optional)"
  fi
  if tenant_proxy_enabled "$name"; then
    rc-service "lunarwing-proxy-${name}" start
  fi
  rc-service "xmpp-bridge-${name}" start
  rc-service "lunarwing-${name}" start
  # Start worker babysitters (if workers are configured)
  for worker in pebble opencode kimi; do
    rc-service "lunarwing-${worker}-${name}-sup" start 2>/dev/null || true
  done
  say "OpenRC services started for $name"

  # Auto-enable on boot whatever is actually running (idempotent, OpenRC only).
  local svc
  local boot_svcs=("lunarwing-pg-${name}" "lunarwing-pg-${name}-sup" "xmpp-bridge-${name}" "lunarwing-${name}" "lunarwing-vision-${name}")
  if tenant_weechat_enabled "$name"; then
    boot_svcs+=("lunarwing-weechat-${name}" "lunarwing-weechat-adapter-${name}")
  fi
  if tenant_proxy_enabled "$name"; then
    boot_svcs+=("lunarwing-proxy-${name}")
  fi
  if tenant_darkirc_enabled "$name"; then
    boot_svcs+=("lunarwing-darkirc-${name}" "lunarwing-darkirc-adapter-${name}")
  fi
  for svc in "${boot_svcs[@]}"; do
    if rc-service "$svc" status >/dev/null 2>&1; then
      rc-update add "$svc" default >/dev/null 2>&1 || true
    fi
  done
  # Auto-enable worker babysitters
  for worker in pebble opencode kimi; do
    if rc-service "lunarwing-${worker}-${name}-sup" status >/dev/null 2>&1; then
      rc-update add "lunarwing-${worker}-${name}-sup" default >/dev/null 2>&1 || true
    fi
  done
  say "enabled boot persistence (default runlevel) for $name's running services"
}

stop_tenant_openrc() {
  local name="$1"
  rc-service "lunarwing-${name}" stop 2>/dev/null || true
  rc-service "xmpp-bridge-${name}" stop 2>/dev/null || true
  rc-service "lunarwing-proxy-${name}" stop 2>/dev/null || true
  rc-service "lunarwing-weechat-adapter-${name}" stop 2>/dev/null || true
  if rc-service "lunarwing-weechat-${name}" status >/dev/null 2>&1; then
    if ! rc-service "lunarwing-weechat-${name}" stop; then
      say "failed to stop lunarwing-weechat-${name}; refusing to continue"
      return 1
    fi
  fi
  rc-service "lunarwing-darkirc-adapter-${name}" stop 2>/dev/null || true
  rc-service "lunarwing-darkirc-${name}" stop 2>/dev/null || true
  # Worker babysitters: stop the supervisors so they don't respawn the stopped containers.
  for worker in nanocode pebble opencode kimi; do
    rc-service "lunarwing-${worker}-${name}-sup" stop 2>/dev/null || true
  done
  rc-service "lunarwing-vision-${name}" stop 2>/dev/null || true
  # Postgres last: the daemon depends on it, so it stops after its consumers.
  rc-service "lunarwing-pg-${name}-sup" stop 2>/dev/null || true
  rc-service "lunarwing-pg-${name}" stop 2>/dev/null || true
  say "OpenRC services stopped for $name"
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

uninstall_tenant_openrc() {
  local name="$1"
  for svc in "lunarwing-${name}" "xmpp-bridge-${name}" "lunarwing-proxy-${name}" "lunarwing-weechat-adapter-${name}" "lunarwing-weechat-${name}" "lunarwing-darkirc-adapter-${name}" "lunarwing-darkirc-${name}" "lunarwing-pg-${name}" "lunarwing-nanocode-${name}" "lunarwing-pebble-${name}" "lunarwing-opencode-${name}" "lunarwing-kimi-${name}" "lunarwing-vision-${name}" "lunarwing-pg-${name}-sup" "lunarwing-nanocode-${name}-sup" "lunarwing-pebble-${name}-sup" "lunarwing-opencode-${name}-sup" "lunarwing-kimi-${name}-sup"; do
    rc-update del "$svc" default 2>/dev/null || true
    rm -f "$OPENRC_INITD_DIR/$svc" "$OPENRC_CONFD_DIR/$svc"
  done
  say "uninstalled OpenRC services for $name"
}

# ── Compound commands ────────────────────────────────────────────────────────

# WeeChat's ws_adapter.py needs the `aiohttp` Python package importable by the
# TENANT user's python3 (system-wide, or in that user's ~/.local — an --user
# install for a different account, e.g. the admin, is NOT visible). Non-fatal:
# the adapter is optional, so we only warn with how to fix it.
warn_if_adapter_deps_missing() {
  local name="$1"
  if ! command -v python3 >/dev/null 2>&1; then
    say "WARNING: python3 not found — the WeeChat adapter cannot run."
    return 0
  fi
  if sudo -u "$name" python3 -c 'import aiohttp' >/dev/null 2>&1; then
    return 0
  fi
  say ""
  say "WARNING: Python package 'aiohttp' is not importable by user '$name'."
  say "         lunarwing-weechat-adapter-${name} will exit on start until it is installed:"
  say "           system-wide (preferred): sudo pacman -S python-aiohttp"
  say "             (Debian: sudo apt install python3-aiohttp  ·  Fedora: sudo dnf install python3-aiohttp)"
  say "           per-tenant fallback:     sudo -u ${name} pip install --user --break-system-packages aiohttp"
  say ""
}

# ── Host-global health-check / self-heal pipeline (OpenRC + systemd) ─────────

_ensure_cron_runlevel() {
  # Ensure a cron daemon is enabled at boot + running so the schedule fires.
  local want="${1:-}" cron
  for cron in ${want:+$want} fcron cronie dcron crond busybox-cron; do
    if [[ -x "/etc/init.d/$cron" ]]; then
      rc-update add "$cron" default >/dev/null 2>&1 || true
      rc-service "$cron" start >/dev/null 2>&1 || true
      say "cron daemon '$cron' enabled + running"
      return 0
    fi
  done
  say "WARNING: no cron daemon init script found; install fcron/cronie/dcron so the schedule runs"
}

_install_health_cron() {
  local sched="*/${HEALTH_INTERVAL_MIN} * * * * $HEALTH_LAUNCHER"
  local begin="# BEGIN lunarwing-mt-health managed block"
  local end="# END lunarwing-mt-health managed block"
  local cmd=""
  command -v fcrontab >/dev/null 2>&1 && cmd="fcrontab"
  [[ -z "$cmd" ]] && command -v crontab >/dev/null 2>&1 && cmd="crontab"
  if [[ -z "$cmd" ]]; then
    say "WARNING: no fcrontab/crontab found — cannot schedule the pipeline. Install a cron daemon or run $HEALTH_LAUNCHER periodically."
    return 0
  fi
  local tmp; tmp="$(mktemp)"
  "$cmd" -l 2>/dev/null | sed "/^${begin}$/,/^${end}$/d" > "$tmp" || true
  { printf '%s\n' "$begin" "$sched" "$end"; } >> "$tmp"
  if "$cmd" "$tmp" 2>/dev/null; then
    say "scheduled health pipeline via $cmd: every ${HEALTH_INTERVAL_MIN} min"
  else
    say "WARNING: failed to install $cmd schedule"
  fi
  rm -f "$tmp"
  _ensure_cron_runlevel "$([[ "$cmd" == "fcrontab" ]] && echo fcron)"
}

_install_health_systemd_timer() {
  # systemd hosts: a root, system-level oneshot service + timer. Persistent=true
  # provides the missed-run catch-up fcron gives on OpenRC; system-level (not
  # --user) because one run remediates many tenants' user units via sudo.
  local svc="/etc/systemd/system/lunarwing-mt-health.service"
  local tmr="/etc/systemd/system/lunarwing-mt-health.timer"
  if ! command -v systemctl >/dev/null 2>&1; then
    say "WARNING: systemctl not found — cannot schedule the pipeline on systemd."
    return 0
  fi
  cat >"$svc" <<UNITEOF
[Unit]
Description=LunarWing MT health-check + self-heal pipeline
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=$HEALTH_LAUNCHER
UNITEOF
  cat >"$tmr" <<UNITEOF
[Unit]
Description=Schedule LunarWing MT health/self-heal pipeline (every ${HEALTH_INTERVAL_MIN} min)

[Timer]
OnBootSec=5min
OnCalendar=*:0/${HEALTH_INTERVAL_MIN}
Persistent=true
AccuracySec=30s
Unit=lunarwing-mt-health.service

[Install]
WantedBy=timers.target
UNITEOF
  systemctl daemon-reload >/dev/null 2>&1 || true
  if systemctl enable --now lunarwing-mt-health.timer >/dev/null 2>&1; then
    say "scheduled health pipeline via systemd timer: every ${HEALTH_INTERVAL_MIN} min"
  else
    say "WARNING: failed to enable lunarwing-mt-health.timer"
  fi
}

ensure_health_pipeline() {
  # Idempotently install + schedule the host-global health-check -> self-heal
  # pipeline. Auto-discovers all tenants, so one install covers every tenant.
  # Wired for OpenRC (fcron/cron) and systemd (timer); other inits are skipped.
  ensure_init_system
  case "$INIT_SYSTEM" in
    openrc|systemd) : ;;
    *) say "health pipeline: not wired for INIT_SYSTEM=$INIT_SYSTEM; skipping"; return 0 ;;
  esac
  [[ -d "$HEALTH_SRC_DIR" ]] || { say "WARNING: health source dir not found ($HEALTH_SRC_DIR); skipping"; return 0; }

  say "--- Ensuring host-global health/self-heal pipeline ---"

  # 1) Stable copy of the pipeline scripts (survives repo/worktree moves).
  mkdir -p "$HEALTH_LIB_DIR"
  cp -a "$HEALTH_SRC_DIR/." "$HEALTH_LIB_DIR/"
  chmod 0755 "$HEALTH_LIB_DIR"/*.sh 2>/dev/null || true
  say "synced pipeline scripts -> $HEALTH_LIB_DIR"

  # 2) Host-level report/state dir.
  mkdir -p "$HEALTH_BASE_DIR/workspace/reports/health"

  # 3) Config env file (write-if-absent so operator edits survive).
  mkdir -p /etc/lunarwing
  if [[ ! -f "$HEALTH_ENV_FILE" ]]; then
    ( umask 077
      cat >"$HEALTH_ENV_FILE" <<ENVEOF
# /etc/lunarwing/health.env — host-global health/self-heal pipeline config.
# Auto-generated by lunarwing-mt-admin.sh (write-if-absent; safe to edit).
LUNARWING_BASE_DIR=$HEALTH_BASE_DIR
LUNARWING_SERVICE_MANAGER=$INIT_SYSTEM
SELF_HEAL_TENANTS_FILE=$PORTS_REGISTRY

# MT hardening: remediate only auto-discovered per-tenant init units; disable
# checks that are N/A host-globally; page only on self-heal escalation (not on
# every non-healthy run); ignore stale reports.
SELF_HEAL_REMEDY_LOGICAL=false
HEALTH_XMPP_SERVER=
HEALTH_MODELS_ENABLED=false
HEALTH_OMEMO_ENABLED=false
HEALTHCHECK_NOTIFY=false
SELF_HEAL_MAX_REPORT_AGE=$(( HEALTH_INTERVAL_MIN * 60 * 4 ))

# LunarVision: leave HEALTH_LUNARVISION_URL unset so health-lunarvision.sh
# auto-discovers every tenant's vision_health port from the ports registry
# (single-node still falls back to http://127.0.0.1:8088). Only set the URL
# if you intentionally force a single override target.
# HEALTH_LUNARVISION_URL=

# Escalation notifications (fill in to enable Gotify pushes).
GOTIFY_URL=$HEALTH_GOTIFY_URL
GOTIFY_TOKEN=$HEALTH_GOTIFY_TOKEN
ENVEOF
    )
    chmod 0600 "$HEALTH_ENV_FILE"
    say "wrote $HEALTH_ENV_FILE (mode 0600)"
  else
    say "$HEALTH_ENV_FILE already exists (preserving)"
    # Reconcile only the service-manager line in case a prior run wrote a
    # different init system (self-heal honors LUNARWING_SERVICE_MANAGER ahead of
    # its own autodetection, so a stale value silently misroutes remediation).
    # Operator edits (e.g. Gotify tokens) are preserved — we rewrite one line.
    if ! grep -q "^LUNARWING_SERVICE_MANAGER=${INIT_SYSTEM}$" "$HEALTH_ENV_FILE"; then
      local _tmp; _tmp="$(mktemp)"
      if grep -q '^LUNARWING_SERVICE_MANAGER=' "$HEALTH_ENV_FILE"; then
        awk -v v="$INIT_SYSTEM" '/^LUNARWING_SERVICE_MANAGER=/{print "LUNARWING_SERVICE_MANAGER=" v; next} {print}' "$HEALTH_ENV_FILE" > "$_tmp"
      else
        cp "$HEALTH_ENV_FILE" "$_tmp"
        printf 'LUNARWING_SERVICE_MANAGER=%s\n' "$INIT_SYSTEM" >> "$_tmp"
      fi
      cat "$_tmp" > "$HEALTH_ENV_FILE"   # overwrite content, preserve mode/owner
      rm -f "$_tmp"
      say "reconciled LUNARWING_SERVICE_MANAGER=$INIT_SYSTEM in $HEALTH_ENV_FILE"
    fi
    # Reconcile the tenants-file line with the ACTIVE ports registry
    # (ISSUE-012): the file is write-once, so if PORTS_REGISTRY moved after
    # first install, health kept reading the old registry while admin
    # commands updated the new one. Same targeted-line rewrite as above;
    # operator edits and file mode/owner are preserved.
    if ! grep -q "^SELF_HEAL_TENANTS_FILE=${PORTS_REGISTRY}$" "$HEALTH_ENV_FILE"; then
      local _tmp2; _tmp2="$(mktemp)"
      if grep -q '^SELF_HEAL_TENANTS_FILE=' "$HEALTH_ENV_FILE"; then
        awk -v v="$PORTS_REGISTRY" '/^SELF_HEAL_TENANTS_FILE=/{print "SELF_HEAL_TENANTS_FILE=" v; next} {print}' "$HEALTH_ENV_FILE" > "$_tmp2"
      else
        cp "$HEALTH_ENV_FILE" "$_tmp2"
        printf 'SELF_HEAL_TENANTS_FILE=%s\n' "$PORTS_REGISTRY" >> "$_tmp2"
      fi
      cat "$_tmp2" > "$HEALTH_ENV_FILE"   # overwrite content, preserve mode/owner
      rm -f "$_tmp2"
      say "reconciled SELF_HEAL_TENANTS_FILE=$PORTS_REGISTRY in $HEALTH_ENV_FILE"
    fi
  fi

  # 4) Launcher: source env, then run the pipeline (health-check -> self-heal LIVE).
  cat >"$HEALTH_LAUNCHER" <<LAUNCHEOF
#!/bin/sh
# Auto-generated by lunarwing-mt-admin.sh. Host-global health/self-heal pipeline.
set -a
[ -r $HEALTH_ENV_FILE ] && . $HEALTH_ENV_FILE
set +a
exec $HEALTH_LIB_DIR/cron-wrapper.sh "\$@"
LAUNCHEOF
  chmod 0755 "$HEALTH_LAUNCHER"
  say "wrote $HEALTH_LAUNCHER"

  # 5) Schedule it (per init system).
  case "$INIT_SYSTEM" in
    openrc)  _install_health_cron ;;
    systemd) _install_health_systemd_timer ;;
  esac
  say "health pipeline ready (every ${HEALTH_INTERVAL_MIN} min; covers all tenants)"
}

remove_health_pipeline() {
  ensure_init_system
  local begin="# BEGIN lunarwing-mt-health managed block"
  local end="# END lunarwing-mt-health managed block"
  case "$INIT_SYSTEM" in
    openrc)
      command -v fcrontab >/dev/null 2>&1 && fcrontab -l 2>/dev/null | sed "/^${begin}$/,/^${end}$/d" | fcrontab - 2>/dev/null || true
      command -v crontab  >/dev/null 2>&1 && crontab  -l 2>/dev/null | sed "/^${begin}$/,/^${end}$/d" | crontab  - 2>/dev/null || true
      ;;
    systemd)
      if command -v systemctl >/dev/null 2>&1; then
        systemctl disable --now lunarwing-mt-health.timer >/dev/null 2>&1 || true
        rm -f /etc/systemd/system/lunarwing-mt-health.timer /etc/systemd/system/lunarwing-mt-health.service
        systemctl daemon-reload >/dev/null 2>&1 || true
      fi
      ;;
    *) return 0 ;;
  esac
  rm -f "$HEALTH_LAUNCHER"
  say "retired host-global health pipeline schedule (no tenants remain)"
  # $HEALTH_LIB_DIR + $HEALTH_ENV_FILE left in place (harmless; preserves config/state).
}

add_tenant() {
  local name="$1"
  local docker_group="${2:-false}"
  local xmpp_jid="${3:-}"   # resolved below (preserve an existing one on re-run)
  local xmpp_password="${4:-}"
  local tensorzero_url="${5:-$DEFAULT_TENSORZERO_URL}"
  local gotify_url="${6:-$DEFAULT_GOTIFY_URL}"
  local gotify_title="${7:-$DEFAULT_GOTIFY_TITLE}"
  local llm_api_key="${8:-}"
  local llm_base_url="${9:-$DEFAULT_LLM_BASE_URL}"
  local enable_darkirc="${10:-false}"
  local enable_proxy="${11:-false}"
  local llm_model="${12:-}"
  local gateway_host="${13:-}"
  local xmpp_allow_from="${14:-}"
  local opencode_model="${15:-}"
  local opencode_base_url="${16:-}"
  # External worker selection (default off). Persisted into the port registry so
  # start_tenant_<worker> starts only what this tenant chose, not every worker
  # whose shared host image happens to exist. See PER_TENANT_WORKER_GATING.md.
  local with_pebble="${17:-false}"
  local with_opencode="${18:-false}"
  local with_kimi="${19:-false}"
  local kimi_model="${20:-}"
  local kimi_base_url="${21:-}"
  local kimi_api_key="${22:-}"
  local darkirc_scope_override="${23:-}"

  name="$(sanitize_name "$name")"
  [[ -n "$name" ]] || die "invalid tenant name"
  # A tenant whose name begins with a reserved per-service prefix would make its
  # primary daemon unit (lunarwing-<name>) collide with another tenant's
  # per-service unit — e.g. tenant 'pebble-1' -> lunarwing-pebble-1, byte-identical
  # to tenant '1's pebble worker unit (lunarwing-pebble-1). That collision is
  # undisambiguatable downstream (health-openrc.sh's started-gate would mis-key the
  # unit and could mask a real outage as `skipped`), so forbid such names at the
  # source. (weechat-* also covers weechat-adapter-*.)
  case "$name" in
    pg-*|proxy-*|nanocode-*|pebble-*|opencode-*|kimi-*|weechat-*)
      die "tenant name '$name' collides with a reserved per-service unit prefix (pg-/proxy-/nanocode-/pebble-/opencode-/kimi-/weechat-/weechat-adapter-); choose another name" ;;
  esac

  # Resolve after sanitize_name so a preserved/default JID keys off the real name.
  local xmpp_jid_prev
  xmpp_jid_prev="$(_env_existing "$(tenant_env_dir "$name")/lunarwing.env" XMPP_JID)"
  xmpp_jid="$(resolve_tenant_xmpp_jid "$name" "$xmpp_jid")"

  say "=== Adding tenant: $name ==="
  say ""
  if [[ -n "$xmpp_jid_prev" && "$xmpp_jid" == "$xmpp_jid_prev" ]]; then
    say "preserving existing XMPP JID: $xmpp_jid"
  elif [[ -n "$xmpp_jid_prev" && "$xmpp_jid" != "$xmpp_jid_prev" ]]; then
    say "WARNING: changing XMPP JID: $xmpp_jid_prev -> $xmpp_jid (bridge restart required)" >&2
  fi

  ports_registry_init
  local base_port workers_json
  workers_json="$(printf '{"nanocode":false,"pebble":%s,"opencode":%s,"kimi":%s}' \
    "$with_pebble" "$with_opencode" "$with_kimi")"
  base_port="$(ports_allocate "$name" "$enable_darkirc" "$enable_proxy" "$workers_json" "$darkirc_scope_override")"
  say ""

  create_tenant_user "$name" "$docker_group"
  say ""

  warn_if_adapter_deps_missing "$name"

  clone_tenant_repo "$name"
  say ""

  retire_tenant_nanocode "$name" \
    || die "failed to retire legacy Nanocode resources for tenant '$name'"
  say ""

  say "--- Generating environment files ---"
  write_tenant_vision_env "$name" >/dev/null
  write_tenant_lunarwing_env "$name" "$xmpp_jid" "$xmpp_password" "$tensorzero_url" "$llm_api_key" "$llm_base_url" "$llm_model" "$gateway_host" "$xmpp_allow_from" "$opencode_model" "$opencode_base_url" "$kimi_model" "$kimi_base_url" "$kimi_api_key"
  write_tenant_bridge_env "$name" "$xmpp_jid" "$xmpp_password" "$xmpp_allow_from"
  if [[ "$enable_proxy" == "true" ]]; then
    write_tenant_proxy_env "$name" "$tensorzero_url"
  fi
  if [[ "$enable_darkirc" == "true" ]]; then
    ensure_darkirc_scope_id "$name" >/dev/null
    write_tenant_darkirc_adapter_env "$name"
    if darkirc_validate_root_executable "$DARKIRC_KEY_HELPER" \
        && [[ -f "$DARKIRC_COMPAT_FILE" ]]; then
      generate_darkirc_config "$name"
    else
      say "DarkIRC config deferred until build-darkirc installs the attested helper"
    fi
  fi
  write_tenant_gotify_config "$name" "$gotify_url" "$gotify_title"
  ensure_external_worker_config "$name" "pebble" "pebble_wss"
  ensure_external_worker_config "$name" "opencode" "opencode_wss"
  ensure_external_worker_config "$name" "kimi" "kimi_wss"

  # WeeChat relay auto-bootstrap: write a dedicated minimal weechat.env, then
  # attempt a one-shot relay configuration. Non-fatal: a degraded WeeChat setup
  # must not strand the base tenant. Recovery: configure-weechat-relay <name>.
  local weechat_bootstrap_ok="configured"
  if [[ "$WEECHAT_BOOTSTRAP_OPT_OUT" == "true" ]]; then
    local relay_password
    relay_password="$(_read_env_value "$(tenant_env_dir "$name")/lunarwing.env" RELAY_PASSWORD)"
    if [[ -n "$relay_password" ]] && _write_weechat_env "$name" "$relay_password"; then
      weechat_bootstrap_ok="disabled (--no-weechat-bootstrap)"
    else
      weechat_bootstrap_ok="disabled (credential env setup failed)"
      say "WARNING: WeeChat bootstrap was disabled, but the minimal credential env could not be written." >&2
    fi
  elif ! configure_weechat_relay "$name" 2>&1; then
    weechat_bootstrap_ok="needs recovery"
    say ""
    say "WARNING: WeeChat relay auto-bootstrap failed for tenant '$name'."
    say "         WeeChat will start but the relay is not configured."
    say "         Recovery: sudo $0 configure-weechat-relay $name"
    say ""
  fi

  # SSH harness: config.toml [[ssh.hosts]] block + ed25519 key pair.
  # Enabled by default; opt out with --no-ssh or LUNARWING_MT_SSH_ENABLED=false.
  if [[ "$DEFAULT_SSH_ENABLED" == "true" && "$SSH_OPT_OUT" != "true" ]]; then
    say "--- Provisioning SSH harness ---"
    ensure_ssh_config "$name"
    provision_tenant_ssh_key "$name"
    warn_if_sshd_unreachable "$name"
  else
    say "SSH harness: disabled (enabled=$DEFAULT_SSH_ENABLED, opt-out=$SSH_OPT_OUT)"
  fi

  if ! "$CONTAINER_RT" image inspect "$VISION_SIDECAR_IMAGE" &>/dev/null; then
    build_vision_sidecar_image || true
  fi
  say ""

  say "--- Starting PostgreSQL ---"
  start_tenant_postgres "$name"
  say ""

  ensure_init_system
  say "--- Rendering $INIT_SYSTEM services ---"
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    render_tenant_systemd_units "$name"
  else
    render_tenant_openrc_units "$name"
  fi
  say ""

  # Host-global health-check + self-heal pipeline (auto-covers every tenant).
  if [[ "$DEFAULT_HEALTH_ENABLED" == "true" && "$HEALTH_OPT_OUT" != "true" ]]; then
    ensure_health_pipeline
  else
    say "health pipeline: disabled (enabled=$DEFAULT_HEALTH_ENABLED, opt-out=$HEALTH_OPT_OUT)"
  fi
  say ""

  say "=== Tenant '$name' added ==="
  say ""
  say "Port block: $base_port-$((base_port + PORT_BLOCK_SIZE - 1))"
  say "  gateway:          $(ports_get "$name" gateway)"
  say "  http:             $(ports_get "$name" http)"
  say "  bridge:           $(ports_get "$name" bridge)"
  say "  postgres:         $(ports_get "$name" postgres)"
  say "  proxy:            $(ports_get "$name" proxy)"
  say "  weechat:          $(ports_get "$name" weechat)"
  say "  orchestrator:     $(ports_get "$name" orchestrator)"
  say "  nanocode_wss:     $(ports_get "$name" nanocode_wss) (reserved legacy slot)"
  say "  pebble_wss:       $(ports_get "$name" pebble_wss)"
  say "  opencode_wss:     $(ports_get "$name" opencode_wss)"
  say "  kimi_wss:         $(ports_get "$name" kimi_wss)"
  say "  weechat_adapter:  $(ports_get "$name" weechat_adapter)"
  say "  darkirc:          $( [[ "$enable_darkirc" == "true" ]] && echo "enabled" || echo "disabled (pass --enable-darkirc to enable)" )"
  say "  proxy:            $( [[ "$enable_proxy" == "true" ]] && echo "enabled" || echo "disabled (pass --enable-proxy to enable)" )"
  say "  workers:          $( _selected="$( [[ "$with_pebble" == "true" ]] && printf 'pebble '; [[ "$with_opencode" == "true" ]] && printf 'opencode '; [[ "$with_kimi" == "true" ]] && printf 'kimi ' )"; [[ -n "$_selected" ]] && echo "${_selected% }" || echo "none (pass --with-pebble/--with-opencode/--with-kimi to select)" )"
  say "  ssh:              $( [[ "$DEFAULT_SSH_ENABLED" == "true" && "$SSH_OPT_OUT" != "true" ]] && echo "enabled (key upload + activation handled by start-tenant)" || echo "disabled (pass --no-ssh)" )"
  say "  weechat relay:    $weechat_bootstrap_ok"
  say ""
  say "Next steps:"
  say "  sudo $0 build-tenant $name --with-wasm"
  say "  sudo $0 start-tenant $name"
}

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

remove_tenant() {
  local name="$1"
  local purge="${2:-false}"

  name="$(sanitize_name "$name")"
  say "=== Removing tenant: $name ==="
  say ""

  retire_tenant_nanocode "$name" \
    || die "failed to retire legacy Nanocode resources for tenant '$name'; refusing removal"

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    stop_tenant_systemd "$name"
    uninstall_tenant_systemd "$name"
  else
    stop_tenant_openrc "$name"
    uninstall_tenant_openrc "$name"
  fi
  say ""

  stop_tenant_postgres "$name"
  if [[ "$purge" == "true" ]]; then
    reset_tenant_postgres "$name"
  fi
  say ""

  ports_deallocate "$name"
  say ""

  remove_tenant_user "$name" "$purge"
  say ""

  # If that was the last tenant, retire the host-global health pipeline schedule.
  if [[ -z "$(all_tenant_names)" ]]; then
    remove_health_pipeline
    say ""
  fi

  say "=== Tenant '$name' removed ==="
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
    render_tenant_systemd_units "$name"
  else
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
_restart_tenant_daemon() {
  local name="$1"
  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    _systemctl_user "$name" restart "lunarwing-${name}.service"
  else
    rc-service "lunarwing-${name}" restart
  fi
}

start_tenant() {
  local name="$1"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"

  say "=== Starting tenant: $name ==="

  retire_tenant_nanocode "$name" \
    || die "failed to retire legacy Nanocode resources for tenant '$name'; refusing start"

  # Safety net: if the tenant has LUNARWING_OWNER_ID set but the DB still has
  # orphaned 'default'-scoped rows (e.g. an upgrade that didn't run patch-env),
  # migrate them before starting the daemon so no data is orphaned.
  local env_path
  env_path="$(tenant_env_dir "$name")/lunarwing.env"
  if grep -q '^LUNARWING_OWNER_ID=' "$env_path" 2>/dev/null && _owner_scope_needs_migration "$name"; then
    say "found orphaned 'default'-scoped DB data; auto-migrating to '$name' scope"
    migrate_owner_scope "$name" || say "WARNING: owner-scope migration failed (run 'migrate-owner-scope $name' manually)"
  fi

  # Pre-create the SSH agent socket path so podman can bind-mount it into
  # worker containers. The daemon creates the actual Unix socket here at
  # startup; without a pre-existing path, podman would create it as a
  # directory (breaking the daemon's socket bind). A touch-file is safe —
  # the daemon removes it and binds the real socket.
  local ssh_socket_path
  ssh_socket_path="$(tenant_run_dir "$name")/ssh-agent.sock"
  if [[ ! -S "$ssh_socket_path" ]]; then
    sudo -u "$name" mkdir -p "$(tenant_run_dir "$name")" 2>/dev/null || true
    sudo -u "$name" touch "$ssh_socket_path" 2>/dev/null || true
  fi

  # Start the daemon BEFORE the workers so the SSH agent socket exists when
  # the worker containers are created (podman bind-mounts the file at creation
  # time; if the socket doesn't exist yet, the mount is a stale touch-file).
  # The daemon's SSH agent creates the real Unix socket at
  # <run_dir>/ssh-agent.sock, which the workers bind-mount.
  start_tenant_postgres "$name"
  start_tenant_vision "$name"

  with_tenant_lifecycle_lock "$name" _start_tenant_init_services "$name" \
    || die "failed to start init services for tenant '$name'"

  # Upload the staged SSH key to the secrets store (if one was provisioned by
  # add-tenant but not yet uploaded), BEFORE the workers start. If a key was
  # actually ingested, bounce the daemon once so the agent loads it (keys are
  # only read from the secrets store at startup); the workers then bind-mount
  # the post-bounce socket inode, so they are never left on a stale socket.
  local staged_key
  staged_key="$(tenant_env_dir "$name")/ssh_key_staged"
  warn_if_sshd_unreachable "$name"
  if [[ -f "$staged_key" ]]; then
    upload_tenant_ssh_key "$name" || true
    if [[ ! -f "$staged_key" ]]; then
      # Upload succeeded (upload_tenant_ssh_key deletes the staged file).
      say "restarting lunarwing-${name} so the SSH agent loads the new key ..."
      if _restart_tenant_daemon "$name"; then
        if _wait_tenant_gateway "$name"; then
          say "lunarwing-${name} restarted; SSH key active"
        else
          say "WARNING: gateway not reachable after SSH-key restart (check 'status $name')" >&2
        fi
      else
        say "WARNING: daemon restart failed after SSH key upload; run '$0 restart-tenant $name' manually" >&2
      fi
    fi
  fi

  # Seed the optional DarkIRC adapter secret into the encrypted secrets store
  # so the WASM channel authenticates outbound adapter requests. Best-effort:
  # a failed upload warns and continues; the WASM channel refreshes the credential
  # live from the store, so no daemon restart is required.
  upload_tenant_darkirc_secret "$name" || true

  # Workers start AFTER the daemon (and after any SSH-key bounce) so the SSH
  # agent socket is already a real, current Unix socket when podman bind-mounts it.
  #
  # Gate each worker on the tenant's persisted selection (add-tenant --with-*).
  # Without this, start_tenant_<worker> would start every worker whose SHARED
  # host image happens to exist — regardless of what this tenant chose — because
  # its only guards are "port allocated" (always true) and "image present"
  # (host-wide). See tenant_worker_enabled / PER_TENANT_WORKER_GATING.md.
  local _w
  for _w in pebble opencode kimi; do
    if tenant_worker_enabled "$name" "$_w"; then
      "start_tenant_${_w}" "$name"
    else
      say "$_w worker not selected for $name (skipping; enable with 'add-tenant $name --with-$_w')"
    fi
  done
  _ssh_ready_summary "$name" || true
}

stop_tenant() {
  local name="$1"
  name="$(sanitize_name "$name")"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"

  say "=== Stopping tenant: $name ==="

  ensure_init_system
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    stop_tenant_systemd "$name"
  else
    stop_tenant_openrc "$name"
  fi

  stop_tenant_vision "$name"
  stop_tenant_pebble "$name"
  stop_tenant_opencode "$name"
  stop_tenant_kimi "$name"
  stop_tenant_postgres "$name"

  # Shutdown must not be blocked by malformed legacy config or an unsafe
  # rollback path. Retire Nanocode only after every supported service is down.
  retire_tenant_nanocode "$name" \
    || die "tenant '$name' stopped, but legacy Nanocode retirement failed"
}

# External upgrade wrappers use this boundary so migration readiness cannot race
# another DarkIRC writer between the check and the completed tenant stop.
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

restart_tenant() {
  local name="$1"
  stop_tenant "$name"
  start_tenant "$name"
}

require_worker_name() {
  case "${1:-}" in
    nanocode) die "Nanocode worker is retired" ;;
    pebble|opencode|kimi) ;;
    *) die "unknown worker '${1:-}'; expected pebble, opencode, or kimi" ;;
  esac
}

prepare_worker_lifecycle() {
  local name worker
  name="$(sanitize_name "$1")"
  worker="$2"
  require_worker_name "$worker"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  printf '%s %s\n' "$name" "$worker"
}

start_worker() {
  local prepared name worker
  prepared="$(prepare_worker_lifecycle "$1" "$2")" || return
  read -r name worker <<<"$prepared"
  tenant_worker_enabled "$name" "$worker" \
    || die "worker '$worker' is not enabled for tenant '$name'"
  "start_tenant_${worker}" "$name"
}

stop_worker() {
  local prepared name worker container service
  prepared="$(prepare_worker_lifecycle "$1" "$2")" || return
  read -r name worker <<<"$prepared"
  container="lunarwing-${worker}-${name}"
  service="${container}.service"
  ensure_container_runtime
  ensure_init_system

  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] \
      && podman_supports_quadlet; then
    _systemctl_user "$name" stop "$service" >/dev/null 2>&1 \
      || _ctr "$name" stop "$container" >/dev/null 2>&1 \
      || die "failed to stop $service"
  elif [[ "$INIT_SYSTEM" == "openrc" ]]; then
    if [[ -f "/etc/init.d/${container}-sup" ]]; then
      rc-service "${container}-sup" stop >/dev/null 2>&1 \
        || die "failed to stop OpenRC supervisor ${container}-sup"
    else
      rc-service "${container}-sup" stop >/dev/null 2>&1 || true
    fi
    if [[ -f "/etc/init.d/${container}" ]]; then
      rc-service "$container" stop >/dev/null 2>&1 \
        || die "failed to stop OpenRC service $container"
    elif _ctr "$name" inspect "$container" &>/dev/null; then
      _ctr "$name" stop "$container" >/dev/null 2>&1 \
        || die "failed to stop container $container"
    fi
  elif _ctr "$name" inspect "$container" &>/dev/null; then
    _ctr "$name" stop "$container" >/dev/null 2>&1 \
      || die "failed to stop container $container"
  fi
  say "$worker worker stopped for $name"
}

kill_worker() {
  local prepared name worker container service
  prepared="$(prepare_worker_lifecycle "$1" "$2")" || return
  read -r name worker <<<"$prepared"
  container="lunarwing-${worker}-${name}"
  service="${container}.service"
  ensure_container_runtime
  ensure_init_system

  # Stop supervision before the zero-grace container stop. Otherwise
  # Restart=on-failure/supervise-daemon can resurrect the worker immediately.
  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] \
      && podman_supports_quadlet; then
    _systemctl_user "$name" stop --no-block "$service" >/dev/null 2>&1 || true
  elif [[ "$INIT_SYSTEM" == "openrc" ]]; then
    rc-service "${container}-sup" stop >/dev/null 2>&1 || true
  fi

  if _ctr "$name" inspect "$container" &>/dev/null; then
    _ctr "$name" stop --time 0 "$container" >/dev/null 2>&1 \
      || die "failed to kill container $container"
  fi

  if [[ "$INIT_SYSTEM" == "systemd" && "$MT_ROOTLESS" == "true" ]] \
      && podman_supports_quadlet; then
    _systemctl_user "$name" stop "$service" >/dev/null 2>&1 || true
  elif [[ "$INIT_SYSTEM" == "openrc" && -f "/etc/init.d/${container}" ]]; then
    rc-service "$container" stop >/dev/null 2>&1 || true
  fi
  say "$worker worker killed and left stopped for $name"
}

restart_worker() {
  stop_worker "$1" "$2"
  start_worker "$1" "$2"
}

# Cargo builds can refresh tracked lockfiles in nested standalone crates. Restore
# them only when they are the sole tracked changes; otherwise preserve everything
# and fail. Untracked tenant runtime directories do not block checkout.
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

upgrade_tenant_darkirc_locked() {
  local name="$1" scope_id
  scope_id="$(darkirc_scope_id "$name")"
  validate_darkirc_scope_id "$scope_id"
  run_darkirc_key_helper "$name" migration-ready \
    --tenant "$name" --scope-id "$scope_id" --json >/dev/null \
    || die "DarkIRC managed state is not settled; upgrade refused"
  # The caller retains the DarkIRC writer lock while upgrade_tenant_locked stops
  # the tenant, closing the readiness-check/stop race.
  upgrade_tenant_locked "$@"
}

# In-place tenant upgrade: composes the verified sequence (backup -> fetch ->
# clean-tree gate -> stop -> checkout as the tenant user -> rebuild with WASM ->
# re-render units -> patch env -> start). The verbs it calls dispatch
# OpenRC/systemd themselves, so this works on any supported init system. See
# docs/ops/TENANT-RENAME-MIGRATION-1.1.9.md for the 1.1.9 rename specifics.
upgrade_tenant() {
  local name="$1"
  shift
  name="$(sanitize_name "$name")"
  with_tenant_lifecycle_lock "$name" _upgrade_tenant_lifecycle_locked "$name" "$@"
}

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

upgrade_tenant_locked() {
  local name="$1" target="$2" source_repo="$3" do_backup="$4" do_render="$5"
  local lw_root repo
  lw_root="$(tenant_lw_root "$name")"
  repo="$(tenant_repo "$name")"

  [[ -d "$lw_root/.git" ]] || die "no git checkout at $lw_root; run add-tenant first"

  local tgit=(sudo -u "$name" git -c safe.directory="$lw_root" -C "$lw_root")

  if [[ -n "$source_repo" ]]; then
    [[ -d "$source_repo/.git" || -f "$source_repo/HEAD" ]] || die "--source-repo $source_repo is not a git repository"
    say "pointing origin at $source_repo ..."
    "${tgit[@]}" remote set-url origin "$source_repo" || die "failed to retarget origin"
  fi

  local before
  before="$("${tgit[@]}" describe --tags --always 2>/dev/null || echo unknown)"
  say "upgrading tenant '$name': $before -> $target"

  if [[ "$do_backup" == "true" ]]; then
    backup_tenant_postgres "$name"
  else
    say "skipping pre-upgrade backup (--no-backup)"
  fi

  say "fetching tags + refs from origin (as $name) ..."
  "${tgit[@]}" fetch --tags --prune origin || die "git fetch failed"
  "${tgit[@]}" rev-parse --verify --quiet "${target}^{commit}" >/dev/null 2>&1 \
    || "${tgit[@]}" rev-parse --verify --quiet "origin/${target}^{commit}" >/dev/null 2>&1 \
    || die "target ref '$target' not found after fetch (need a branch, tag, or commit reachable from origin)"

  local checkout_ref target_commit
  checkout_ref="$target"
  if "${tgit[@]}" rev-parse --verify --quiet "refs/remotes/origin/$target" >/dev/null 2>&1; then
    checkout_ref="origin/$target"
  fi
  target_commit="$("${tgit[@]}" rev-parse --verify "${checkout_ref}^{commit}")" \
    || die "failed to resolve target ref '$target'"

  local build_lock_fd migrations_type previous_commit
  say "acquiring build lock for checkout ..."
  exec {build_lock_fd}>"$BUILD_LOCK" || die "failed to open build lock for checkout"
  flock -x "$build_lock_fd" || die "failed to acquire build lock for checkout"
  prepare_tenant_checkout "$name" "$lw_root" "$target_commit"
  migrations_type="$("${tgit[@]}" cat-file -t "${target_commit}:ic/migrations" 2>/dev/null || true)"
  [[ "$migrations_type" == "tree" ]] \
    || die "target ref '$target' has no ic/migrations directory (tenant was not stopped)"

  previous_commit="$("${tgit[@]}" rev-parse --verify HEAD)" \
    || die "failed to record current tenant commit"
  stop_tenant "$name"

  say "checking out $target at $target_commit (as $name) ..."
  if [[ "$checkout_ref" != "$target" ]]; then
    # Branch on origin: (re)create the local branch on the pinned fetched
    # commit so concurrent ref updates cannot change the deployed source.
    "${tgit[@]}" checkout -B "$target" "$target_commit" \
      || recover_failed_tenant_checkout "$name" "$lw_root" "$target" "$previous_commit"
  else
    "${tgit[@]}" checkout "$target_commit" \
      || recover_failed_tenant_checkout "$name" "$lw_root" "$target" "$previous_commit"
  fi
  [[ -d "$repo/migrations" ]] || die "ic/migrations missing after checkout — wrong ref?"
  say "  now at: $("${tgit[@]}" describe --tags --always 2>/dev/null)"
  flock -u "$build_lock_fd" || die "failed to release build lock after checkout"
  exec {build_lock_fd}>&-

  local with_opencode="false"
  if tenant_worker_enabled "$name" opencode; then
    with_opencode="true"
  fi
  local with_kimi="false"
  if tenant_worker_enabled "$name" kimi; then
    with_kimi="true"
  fi
  build_tenant "$name" "true" "false" "$with_opencode" "$with_kimi" "false" "true"

  if [[ "$do_render" == "true" ]]; then
    render_tenant_units "$name"
  else
    say "skipping render-units (--skip-render); unit files keep their embedded paths"
  fi

  patch_tenant_env "$name"
  start_tenant "$name"

  # Pre-1.1.9 units embed renamed adapter paths that only keep working through
  # the 1.1.9-only compat symlinks — warn (don't fail) if any remain.
  local stale
  stale="$(grep -rlE 'ironclaw_weechat_wss|darkirc_channel_for_ironclaw' \
    /etc/init.d /etc/conf.d "$(tenant_home "$name")/.config/systemd/user" 2>/dev/null \
    | grep -F -- "$name" || true)"
  if [[ -n "$stale" ]]; then
    say "WARNING: these units still embed pre-rename paths (run render-units before v2.0.0):"
    say "$stale"
  fi

  say ""
  say "tenant '$name' upgraded: $before -> $("${tgit[@]}" describe --tags --always 2>/dev/null)"
  say "verify with: $0 status $name"
}

status_tenant() {
  local name="$1"
  name="$(sanitize_name "$name")"

  if ! tenant_exists_in_registry "$name"; then
    say "tenant '$name' not found in registry"
    return 1
  fi

  say "=== Tenant: $name ==="
  say ""
  say "Ports:"
  say "  gateway:          $(ports_get "$name" gateway)"
  say "  http:             $(ports_get "$name" http)"
  say "  bridge:           $(ports_get "$name" bridge)"
  say "  postgres:         $(ports_get "$name" postgres)"
  say "  proxy:            $(ports_get "$name" proxy)"
  say "  weechat:          $(ports_get "$name" weechat)"
  say "  orchestrator:     $(ports_get "$name" orchestrator)"
  say "  nanocode_wss:     $(ports_get "$name" nanocode_wss) (reserved legacy slot)"
  say "  pebble_wss:       $(ports_get "$name" pebble_wss)"
  say "  opencode_wss:     $(ports_get "$name" opencode_wss)"
  say "  kimi_wss:         $(ports_get "$name" kimi_wss)"
  say "  weechat_adapter:  $(ports_get "$name" weechat_adapter)"
  say ""

  ensure_container_runtime
  local container_name="lunarwing-pg-$name"
  if _ctr "$name" inspect -f '{{.State.Running}}' "$container_name" 2>/dev/null | grep -q true; then
    say "PostgreSQL: running ($container_name)"
  else
    say "PostgreSQL: stopped ($container_name)"
  fi

  local nanocode_container="lunarwing-nanocode-$name"
  if _ctr "$name" inspect -f '{{.State.Running}}' "$nanocode_container" 2>/dev/null | grep -q true; then
    say "Nanocode (retired): WARNING still running ($nanocode_container)"
  elif _ctr "$name" inspect "$nanocode_container" &>/dev/null; then
    say "Nanocode (retired): stopped container preserved ($nanocode_container)"
  else
    say "Nanocode (retired): no container in active runtime store"
  fi

  local pebble_container="lunarwing-pebble-$name"
  if _ctr "$name" inspect -f '{{.State.Running}}' "$pebble_container" 2>/dev/null | grep -q true; then
    say "Pebble worker: running ($pebble_container, WSS port $(ports_get "$name" pebble_wss))"
  elif _ctr "$name" inspect "$pebble_container" &>/dev/null; then
    say "Pebble worker: stopped ($pebble_container)"
  else
    say "Pebble worker: not created"
  fi

  local opencode_container="lunarwing-opencode-$name"
  if _ctr "$name" inspect -f '{{.State.Running}}' "$opencode_container" 2>/dev/null | grep -q true; then
    say "OpenCode worker: running ($opencode_container, WSS port $(ports_get "$name" opencode_wss))"
  elif _ctr "$name" inspect "$opencode_container" &>/dev/null; then
    say "OpenCode worker: stopped ($opencode_container)"
  else
    say "OpenCode worker: not created"
  fi

  local kimi_container="lunarwing-kimi-$name"
  if _ctr "$name" inspect -f '{{.State.Running}}' "$kimi_container" 2>/dev/null | grep -q true; then
    say "Kimi worker: running ($kimi_container, WSS port $(ports_get "$name" kimi_wss))"
  elif _ctr "$name" inspect "$kimi_container" &>/dev/null; then
    say "Kimi worker: stopped ($kimi_container)"
  else
    say "Kimi worker: not created"
  fi

  ensure_init_system
  say ""
  say "Services ($INIT_SYSTEM):"
  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    local svcs=("lunarwing-${name}" "xmpp-bridge-${name}" "lunarwing-proxy-${name}" \
                "lunarwing-weechat-${name}" "lunarwing-weechat-adapter-${name}")
    if tenant_darkirc_enabled "$name"; then
      svcs+=("lunarwing-darkirc-${name}" "lunarwing-darkirc-adapter-${name}")
    fi
    # pg + workers are Quadlet units only on rootless podman; on rootful docker
    # they run as plain containers (shown above), not systemd units.
    if [[ "$MT_ROOTLESS" == "true" ]] && podman_supports_quadlet; then
      svcs+=("lunarwing-pg-${name}" "lunarwing-pebble-${name}" "lunarwing-opencode-${name}" "lunarwing-kimi-${name}")
    fi
    local svc state
    for svc in "${svcs[@]}"; do
      state="$(_systemctl_user "$name" is-active "${svc}.service" 2>/dev/null || echo "inactive")"
      say "  ${svc}.service: $state"
    done
  else
    local rc_svcs=("lunarwing-pg-${name}" "lunarwing-pg-${name}-sup" "lunarwing-${name}" "xmpp-bridge-${name}" "lunarwing-proxy-${name}" \
                   "lunarwing-weechat-${name}" "lunarwing-weechat-adapter-${name}")
    if tenant_darkirc_enabled "$name"; then
      rc_svcs+=("lunarwing-darkirc-${name}" "lunarwing-darkirc-adapter-${name}")
    fi
    rc_svcs+=("lunarwing-pebble-${name}" "lunarwing-pebble-${name}-sup" \
              "lunarwing-opencode-${name}" "lunarwing-opencode-${name}-sup" \
              "lunarwing-kimi-${name}" "lunarwing-kimi-${name}-sup")
    local svc state
    for svc in "${rc_svcs[@]}"; do
      state="$(rc-service "$svc" status 2>/dev/null | grep -oE 'started|stopped|crashed' || echo "unknown")"
      say "  $svc: $state"
    done
  fi
}

list_tenants() {
  ports_registry_init
  say "=== Registered tenants ==="
  say ""

  local names
  names="$(all_tenant_names)"
  if [[ -z "$names" ]]; then
    say "no tenants registered"
    return 0
  fi

  printf '%-15s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s\n' \
    "TENANT" "GATEWAY" "HTTP" "BRIDGE" "PG" "PROXY" "WEECHAT" "WS_ADPT" "ORCH" "NC_RSVD" "PEBBLE" "OPENCODE" "KIMI"
  printf '%-15s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s\n' \
    "------" "-------" "----" "------" "--" "-----" "-------" "-------" "----" "--------" "------" "--------" "----"

  while IFS= read -r name; do
    printf '%-15s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s %-8s\n' \
      "$name" \
      "$(ports_get "$name" gateway)" \
      "$(ports_get "$name" http)" \
      "$(ports_get "$name" bridge)" \
      "$(ports_get "$name" postgres)" \
      "$(ports_get "$name" proxy)" \
      "$(ports_get "$name" weechat)" \
      "$(ports_get "$name" weechat_adapter)" \
      "$(ports_get "$name" orchestrator)" \
      "$(ports_get "$name" nanocode_wss)" \
      "$(ports_get "$name" pebble_wss)" \
      "$(ports_get "$name" opencode_wss)" \
      "$(ports_get "$name" kimi_wss)"
  done <<< "$names"
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

doctor() {
  say "=== Multi-tenant doctor ==="
  say ""

  local pass=0 fail=0
  _check() {
    local label="$1"; shift
    if "$@" >/dev/null 2>&1; then
      printf '[PASS] %s\n' "$label"
      pass=$((pass + 1))
    else
      printf '[FAIL] %s\n' "$label"
      fail=$((fail + 1))
    fi
  }

  _check "running as root" test "${EUID}" -eq 0
  _check "jq installed" command -v jq
  _check "git installed" command -v git
  _check "python3 installed" command -v python3
  _check "cargo installed" command -v cargo
  _check "rustup installed" command -v rustup

  if command -v docker >/dev/null 2>&1; then
    _check "docker available" docker info
  fi
  if command -v podman >/dev/null 2>&1; then
    _check "podman available" podman info
  fi

  # Informational: which runtime commands will use, and why. Single call,
  # protected by the if: detect_container_runtime dies (exits) on a box with
  # neither runtime, and only a $( ) subshell can contain that exit. The
  # source label is re-derived here with the same precedence the function
  # uses (env > saved file > auto-detect).
  local _rt_resolved _rt_source
  if _rt_resolved="$(detect_container_runtime 2>/dev/null)"; then
    if [[ -n "${LUNARWING_CONTAINER_RUNTIME:-}" ]]; then
      _rt_source="env"
    elif [[ "$(_load_saved_container_runtime 2>/dev/null)" == "$_rt_resolved" ]]; then
      _rt_source="saved — $RUNTIME_STATE_FILE"
    else
      _rt_source="auto-detected"
    fi
    printf '[info] container runtime: %s (%s)\n' "$_rt_resolved" "$_rt_source"
  else
    printf '[info] container runtime: unresolved (install docker or podman, or set LUNARWING_CONTAINER_RUNTIME)\n'
  fi

  _check "sshd listening on 127.0.0.1:22 (needed for loopback SSH tenants)" \
    bash -c 'timeout 2 bash -c "exec 3<>/dev/tcp/127.0.0.1/22"'

  ensure_container_runtime
  if [[ "$MT_ROOTLESS" == "true" ]]; then
    _check "rootless: newuidmap setuid" bash -c '[ -u "$(command -v newuidmap 2>/dev/null)" ]'
    _check "rootless: newgidmap setuid" bash -c '[ -u "$(command -v newgidmap 2>/dev/null)" ]'
    _check "rootless: /etc/subuid populated" test -s /etc/subuid
    _check "rootless: /etc/subgid populated" test -s /etc/subgid
    # Every per-tenant container publishes 127.0.0.1:<port>:…, which under rootless
    # needs a userspace port-forwarder (pasta or slirp4netns).
    _check "rootless: pasta or slirp4netns (port-forward)" \
      bash -c 'command -v pasta >/dev/null 2>&1 || command -v slirp4netns >/dev/null 2>&1'
  fi

  ensure_init_system
  _check "init system detected ($INIT_SYSTEM)" true

  if [[ "$INIT_SYSTEM" == "systemd" ]]; then
    _check "loginctl available" command -v loginctl
    if [[ "$MT_ROOTLESS" == "true" ]]; then
      _check "podman >= 4.6 (Quadlet supervision)" podman_supports_quadlet
    fi
    # Per-tenant linger keeps /run/user/<uid> + the systemd --user manager alive
    # across reboot — boot-persistent Quadlet/user units depend on it. (Highest-
    # value rootless-on-systemd check.)
    local _dt _du _duid
    while IFS=$'\t' read -r _dt _du; do
      [[ -n "$_du" ]] || continue
      _duid="$(id -u "$_du" 2>/dev/null || echo "")"
      [[ -n "$_duid" ]] || continue
      _check "tenant $_dt: linger enabled" \
        bash -c "loginctl show-user '$_du' -p Linger --value 2>/dev/null | grep -qx yes"
      _check "tenant $_dt: /run/user/$_duid present" test -d "/run/user/$_duid"
    done < <(jq -r '.tenants // {} | to_entries[] | "\(.key)\t\(.value.user)"' "$PORTS_REGISTRY" 2>/dev/null || true)
  else
    _check "rc-service available" command -v rc-service
    _check "rc-update available" command -v rc-update
  fi

  _check "port registry exists" test -f "$PORTS_REGISTRY"
  _check "source repo exists" test -d "$SOURCE_REPO/ic"
  _check "proxy script exists" test -f "$SOURCE_REPO/tensorzero-proxy-configurations/lunarwing-proxy.py"
  _check "health-check orchestrator present" test -x "$HEALTH_SRC_DIR/infrastructure-health-check.sh"
  _check "self-heal script present" test -x "$HEALTH_SRC_DIR/lunarwing-self-heal.sh"
  _check "curl installed (self-heal/gotify)" command -v curl
  _check "flock installed (self-heal lock)" command -v flock
  if [[ "$DEFAULT_HEALTH_ENABLED" == "true" ]]; then
    _check "health pipeline scheduled" bash -c 'systemctl is-enabled lunarwing-mt-health.timer >/dev/null 2>&1 || crontab -l 2>/dev/null | grep -q lunarwing-mt-health || { command -v fcrontab >/dev/null 2>&1 && fcrontab -l 2>/dev/null | grep -q lunarwing-mt-health; }'
  fi
  _check "pebble worker dir exists" test -d "$LUNARWING_ROOT/pebble4lunarwing"
  _check "pebble worker Dockerfile exists" test -f "$LUNARWING_ROOT/pebble4lunarwing/Dockerfile"
  _check "opencode worker dir exists" test -d "$LUNARWING_ROOT/opencode4lunarwing"
  _check "opencode worker Dockerfile exists" test -f "$LUNARWING_ROOT/opencode4lunarwing/Dockerfile"
  _check "kimi worker dir exists" test -d "$LUNARWING_ROOT/kimicode4lunarwing"
  _check "kimi worker Dockerfile exists" test -f "$LUNARWING_ROOT/kimicode4lunarwing/Dockerfile"

  # Check if worker images are built
  if command -v docker >/dev/null 2>&1; then
    _check "pebble worker image exists" docker image inspect lunarwing-worker-pebble:latest
    _check "opencode worker image exists" docker image inspect lunarwing-worker-opencode:latest
    _check "kimi worker image exists" docker image inspect lunarwing-worker-kimi:latest
    _check "vision sidecar image exists" docker image inspect "$VISION_SIDECAR_IMAGE"
  elif command -v podman >/dev/null 2>&1; then
    _check "pebble worker image exists" podman image inspect lunarwing-worker-pebble:latest
    _check "opencode worker image exists" podman image inspect lunarwing-worker-opencode:latest
    _check "kimi worker image exists" podman image inspect lunarwing-worker-kimi:latest
    _check "vision sidecar image exists" podman image inspect "$VISION_SIDECAR_IMAGE"
  fi

  say ""
  say "passed: $pass, failed: $fail"
  [[ $fail -eq 0 ]]
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

    build-nanocode-worker)
      die "Nanocode worker is retired; build-nanocode-worker is unavailable"
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

    stop-tenant-for-upgrade)
      require_root
      [[ $# -eq 1 ]] || die "usage: stop-tenant-for-upgrade <name>"
      ports_registry_init
      _lifecycle_name="$(sanitize_name "$1")"
      with_tenant_lifecycle_lock "$_lifecycle_name" stop_tenant_for_upgrade "$_lifecycle_name"
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
