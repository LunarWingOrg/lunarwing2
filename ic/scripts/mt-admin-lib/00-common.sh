#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# 00-common.sh — part of the lunarwing-mt-admin library.
#
# Shared helpers: diagnostics, port-registry locking, token/name
# generation, tenant path primitives, container config-hash tracking,
# per-tenant feature-flag + PG password readers, template rendering,
# and the root guard.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced, so this file only defines functions.

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

generate_token() {
  if command -v od >/dev/null 2>&1; then
    dd if=/dev/urandom bs=32 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n'
  else
    printf 'replace-with-random-token-%s' "$(date +%s)"
  fi
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

# Compute and store the hash of a config file (e.g. a .container quadlet).
# Usage: _store_container_hash <tenant> <container-name> <config-file>
_store_container_hash() {
  local name="$1" container="$2" config_file="$3" hdir hash
  hdir="$(_hash_dir "$name")"
  mkdir -p "$hdir" 2>/dev/null || true
  hash="$(sha256sum "$config_file" 2>/dev/null | cut -d' ' -f1 || true)"
  [[ -n "$hash" ]] && printf '%s\n' "$hash" >"$hdir/${container}.hash"
}

# Check whether the config file's hash matches the stored hash.
# Returns 0 (match / first-run) or 1 (mismatch / needs recreate).
# Usage: _container_config_changed <tenant> <container-name> <config-file>
_container_config_changed() {
  local name="$1" container="$2" config_file="$3" hdir stored current
  hdir="$(_hash_dir "$name")"
  [[ -f "$config_file" ]] || return 0  # no config file = no opinion
  current="$(sha256sum "$config_file" 2>/dev/null | cut -d' ' -f1 || true)"
  [[ -n "$current" ]] || return 0      # can't hash = don't force recreate
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

# Whether a given external worker (nanocode|pebble|opencode) is selected for this
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

  [[ "$template" = /* ]] || template="$TEMPLATES_DIR/$template"
  [[ -f "$template" ]] || die "template not found: $template"

  local content
  content="$(<"$template")"

  local pair var val
  for pair; do
    var="${pair%%=*}"
    val="${pair#*=}"
    content="${content//__${var}__/${val}}"
  done

  printf '%s\n' "$content" > "$out"
}
# ── Root check ────────────────────────────────────────────────────────────────

require_root() {
  [[ "${EUID}" -eq 0 ]] || die "must run as root (use sudo)"
}

