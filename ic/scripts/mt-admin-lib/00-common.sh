#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# 00-common.sh — part of the lunarwing-mt-admin library.
#
# Shared helpers: diagnostics, port-registry locking, tenant lifecycle locking,
# token/name/scope-id generation, tenant path primitives, container config-hash
# tracking, per-tenant feature-flag + PG password readers, template rendering,
# and the root guard.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced, so this file only defines functions.
#
# Extracted from master (v2.0.6.0 era) as Phase 0 of the mt-admin refactor.
# Architecture follows docs/proposals/MT-ADMIN-DECOMPOSITION.md and the prior
# attempt on origin/mtadminrefactor/meta4/v2.0.3.0, updated for new growth:
# lifecycle locks, DarkIRC scope IDs, WeeChat enablement, worker desired-runtime,
# launch-contract hashing, and render_template_content.
#
# Pure line-range move from the monolith: no function body was rewritten.

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

# Check identity and held state without converting an unlocked forged descriptor
# into a valid boundary. A separate open-file description must remain blocked.
inherited_lock_fd_is_held() {
  local lock_fd="$1" lock_file="$2" probe_fd
  [[ "$lock_fd" =~ ^[0-9]+$ && -e "/proc/self/fd/$lock_fd" \
    && -f "$lock_file" && ! -L "$lock_file" \
    && "$(stat -Lc '%d:%i' "/proc/self/fd/$lock_fd" 2>/dev/null)" \
      == "$(stat -c '%d:%i' "$lock_file" 2>/dev/null)" ]] || return 1
  exec {probe_fd}>"$lock_file" || return 1
  if flock -x -n "$probe_fd"; then
    flock -u "$probe_fd" || true
    exec {probe_fd}>&-
    return 1
  fi
  exec {probe_fd}>&-
  # Probe proved some open-file description holds this inode. This succeeds
  # without changing lock ownership only when inherited FD is that description;
  # an unlocked same-inode FD remains blocked by holder and must fail.
  flock -x -n "$lock_fd"
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
    [[ "${LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_TENANT:-}" == "$name" ]] \
      && inherited_lock_fd_is_held "$lock_fd" "$TENANT_LIFECYCLE_LOCK_DIR/${name}.lock" \
      || die "invalid active tenant lifecycle lock"
    "$@"
    return
  fi
  if [[ -n "${LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_FD:-}" ]]; then
    lock_fd="$LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_FD"
    [[ "${LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_TENANT:-}" == "$name" ]] \
      && inherited_lock_fd_is_held "$lock_fd" "$TENANT_LIFECYCLE_LOCK_DIR/${name}.lock" \
      || die "invalid inherited tenant lifecycle lock"
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

# OpenRC worker pause state. Existing enabled workers default to running when
# this optional field is absent; stop-worker/kill-worker persist false and
# start-worker restores true. Kept separate from .workers because selection and
# operator-requested runtime state are different concerns.
tenant_worker_desired_running() {
  local name="$1" worker="$2" val
  tenant_worker_enabled "$name" "$worker" || return 1
  val="$(jq -r --arg name "$name" --arg worker "$worker" '
    if .tenants[$name].worker_runtime[$worker].desired_running == false
    then "false" else "true" end
  ' "$PORTS_REGISTRY" 2>/dev/null || true)"
  [[ "$val" == "true" ]]
}

# Cleanup may stop live services, so unknown registry state must fail safe. Only
# an explicit unselected worker or desired_running=false authorizes OpenRC stop.
tenant_worker_openrc_stop_requested() {
  local name="$1" worker="$2" val
  val="$(jq -r --arg name "$name" --arg worker "$worker" '
    if .tenants[$name] == null then "unknown"
    elif .tenants[$name].workers[$worker] != true then "stopped"
    elif .tenants[$name].worker_runtime[$worker].desired_running == false then "stopped"
    else "running" end
  ' "$PORTS_REGISTRY" 2>/dev/null)" || return 1
  [[ "$val" == "stopped" ]]
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
