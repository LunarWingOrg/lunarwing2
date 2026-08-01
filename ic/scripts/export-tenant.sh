#!/usr/bin/env bash
#
# export-tenant.sh — package one multi-tenant tenant into a portable bundle for
# MACHINE MIGRATION to a fresh LunarWing host (see docs/ops/MT-MACHINE-MIGRATION.md).
#
# Runs on the OLD (source) host. Self-contained: it does NOT need a v1.1.4-class
# mt-admin (the source may be v1.1.0). It only needs the container runtime, jq, tar.
#
# IMPORTANT — this is the START OF CUTOVER, not a read-only snapshot. To capture a
# CONSISTENT and FINAL picture (so nothing is written after the snapshot and the
# OMEMO double-ratchet store isn't torn mid-write), it STOPS the tenant's daemon +
# xmpp-bridge first (PostgreSQL stays up only for pg_dump). Plan a maintenance window.
#
# Bundle (a single 0600, AES-256 encrypted 7z archive):
#   meta.txt                 tenant, source version/runtime, pg role/db, timestamp
#   db.dump                  pg_dump -Fc of the tenant DB
#   manifest-lunarwing.env   carry-over keys for lunarwing.env (0600) — see CARRY below
#   manifest-bridge.env      carry-over keys for xmpp-bridge.env (0600)
#   manifest-vision.env      optional carry-over keys for vision.env (0600)
#   state.tar.gz             state dir (OMEMO store + workspace + tool storage),
#                            EXCLUDING sockets, host-specific config.toml, and *.wasm
#
# CARRY policy: only values that MUST match the source are carried — SECRETS_MASTER_KEY
# (the AES-256-GCM vault key, without which the DB's encrypted secret rows are
# unrecoverable), the XMPP identity (JID + password), and operator config (XMPP
# rooms/allowlist/OMEMO + LLM model/key). Intra-host tokens (gateway/bridge/webhook/
# relay) are intentionally NOT carried — the new host mints fresh, self-consistent
# ones (gateway UI / external webhook senders re-auth after cutover). Host-specific
# LLM/worker base URLs are carried ONLY when they point at non-loopback custom
# endpoints; obsolete host-local proxy URLs are discarded. Vision sidecar ports and
# its host-local auth token are regenerated; only custom VL_URL/VL_MODEL values move.
#
# SECURITY: the bundle contains SECRETS_MASTER_KEY + the XMPP password. It is 0600,
# root-owned, in a 0700 dir. Transfer over ssh; delete from both hosts after verify.
#
# Usage (run as root on the source host):
#   sudo ic/scripts/export-tenant.sh <tenant> [--out-dir DIR] [--no-quiesce] [--dry-run]
#     --out-dir DIR   where to write the bundle (default /var/lib/lunarwing-migrate)
#     --no-quiesce    do NOT stop the daemon/bridge (you stopped them already);
#                     the script still verifies the daemon is not running
#     --dry-run       show what would happen; make no changes
#     --no-encrypt    testing only: write a sensitive legacy plaintext tar
#
# Non-interactive passphrase sources, in precedence order: KAWARIMI_PASS_FD,
# KAWARIMI_PASS, KAWARIMI_PASS_FILE. Interactive use prompts and confirms before
# any tenant service is stopped. The passphrase is fed to 7z over stdin.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="${LUNARWING_MT_ADMIN:-$SCRIPT_DIR/lunarwing-mt-admin.sh}"
OUT_DIR="${LUNARWING_MIGRATE_DIR:-/var/lib/lunarwing-migrate}"
NO_QUIESCE=false
DRY_RUN=false
NO_ENCRYPT=false
TENANT=""
PORTS_REGISTRY="${LUNARWING_PORTS_REGISTRY:-/etc/lunarwing/ports.json}"
DARKIRC_WRITER_LOCK_DIR="${LUNARWING_MT_DARKIRC_WRITER_LOCK_DIR:-/run/lock/lunarwing-darkirc}"
TENANT_LIFECYCLE_LOCK_DIR="${LUNARWING_MT_LIFECYCLE_LOCK_DIR:-/run/lock/lunarwing-tenants}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out-dir)    OUT_DIR="$2"; shift 2 ;;
    --no-quiesce) NO_QUIESCE=true; shift ;;
    --dry-run)    DRY_RUN=true; shift ;;
    --no-encrypt) NO_ENCRYPT=true; shift ;;
    -*)           printf 'unknown arg: %s\n' "$1" >&2; exit 2 ;;
    *)            TENANT="$1"; shift ;;
  esac
done

say()    { printf '%s\n' "$*"; }
die()    { printf 'error: %s\n' "$*" >&2; exit 1; }
banner() { printf '\n========== %s ==========\n' "$*"; }
note()   { printf '  · %s\n' "$*"; }
run()    { if $DRY_RUN; then printf '  [dry-run] %s\n' "$*"; return 0; fi; printf '  + %s\n' "$*"; "$@"; }

validate_secure_output_directory() {
  local path="$1" current="" component owner mode
  [[ "$path" == /* && -d "$path" && ! -L "$path" ]] || return 1
  while IFS= read -r component; do
    [[ -n "$component" ]] || continue
    current="$current/$component"
    [[ -d "$current" && ! -L "$current" ]] || return 1
    owner="$(stat -c '%u' "$current" 2>/dev/null || true)"
    mode="$(stat -c '%a' "$current" 2>/dev/null || true)"
    [[ "$owner" == 0 && "$mode" =~ ^[0-7]+$ ]] || return 1
    (( (8#$mode & 0022) == 0 )) || return 1
  done < <(tr '/' '\n' <<<"${path#/}")
}

validate_passphrase() {
  [[ -n "$1" ]] || die "encryption passphrase must not be empty"
  [[ "${#1}" -ge 12 ]] || die "encryption passphrase must be at least 12 characters"
  [[ "${#1}" -le 1024 ]] || die "encryption passphrase must be at most 1024 characters"
  [[ "$1" != *$'\n'* && "$1" != *$'\r'* ]] || die "encryption passphrase must not contain line breaks"
}

load_passphrase_file() {
  local path="$1" mode owner
  [[ -f "$path" && ! -L "$path" ]] || die "passphrase file must be a regular, non-symlink file: $path"
  owner="$(stat -c '%u' "$path")" || die "cannot inspect passphrase file owner: $path"
  [[ "$owner" == "$(id -u)" ]] || die "passphrase file must be owned by the current user: $path"
  mode="$(stat -c '%a' "$path")" || die "cannot inspect passphrase file mode: $path"
  (( (8#$mode & 077) == 0 )) || die "passphrase file must not be accessible by group or others: $path"
  KAWARIMI_ARCHIVE_PASS="$(<"$path")"
}

acquire_export_passphrase() {
  local fd confirm=""
  if [[ -n "${KAWARIMI_PASS_FD:-}" ]]; then
    [[ "$KAWARIMI_PASS_FD" =~ ^[0-9]+$ ]] || die "KAWARIMI_PASS_FD must be a file descriptor number"
    fd="$KAWARIMI_PASS_FD"
    if ! IFS= read -r KAWARIMI_ARCHIVE_PASS <&"$fd"; then
      [[ -n "$KAWARIMI_ARCHIVE_PASS" ]] || die "failed to read passphrase from KAWARIMI_PASS_FD"
    fi
    eval "exec ${fd}<&-"
  elif [[ -n "${KAWARIMI_PASS:-}" ]]; then
    KAWARIMI_ARCHIVE_PASS="$KAWARIMI_PASS"
  elif [[ -n "${KAWARIMI_PASS_FILE:-}" ]]; then
    load_passphrase_file "$KAWARIMI_PASS_FILE"
  else
    [[ -t 0 ]] || die "encryption passphrase required via KAWARIMI_PASS_FD, KAWARIMI_PASS_FILE, KAWARIMI_PASS, or an interactive terminal"
    read -r -s -p "Enter encryption passphrase: " KAWARIMI_ARCHIVE_PASS
    echo
    read -r -s -p "Confirm passphrase: " confirm
    echo
    [[ "$KAWARIMI_ARCHIVE_PASS" == "$confirm" ]] || die "passphrases do not match"
  fi
  unset KAWARIMI_PASS KAWARIMI_PASS_FILE KAWARIMI_PASS_FD
  validate_passphrase "$KAWARIMI_ARCHIVE_PASS"
}

read_env_value() {
  local file="$1" key="$2" value
  value="$(sed -n "s/^${key}=//p" "$file" 2>/dev/null | head -1)"
  value="${value%$'\r'}"
  if [[ "$value" =~ ^\"(.*)\"$ ]]; then
    value="${BASH_REMATCH[1]}"
    value="${value//\\\"/\"}"
    value="${value//\\\\/\\}"
  elif [[ "$value" =~ ^\'(.*)\'$ ]]; then
    value="${BASH_REMATCH[1]}"
    value="${value//\\\'/\'}"
    value="${value//\\\\/\\}"
  fi
  printf '%s' "$value"
}

is_loopback_url() {
  local lower="${1,,}"
  [[ "$lower" =~ ^https?://(localhost|127\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}|\[::1\]|\[0:0:0:0:0:0:0:1\])(:[0-9]+)?(/|$) ]]
}

[[ -n "$TENANT" ]] || die "usage: $0 <tenant> [--out-dir DIR] [--no-quiesce] [--dry-run] [--no-encrypt]"
[[ "$TENANT" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]] || die "invalid tenant name"
[[ "$(id -u)" -eq 0 ]] || die "run as root (sudo): stops the tenant services, reads its home/state, execs its DB container"
command -v jq  >/dev/null 2>&1 || die "jq required"
command -v tar >/dev/null 2>&1 || die "tar required"
command -v flock >/dev/null 2>&1 || die "flock required"
if ! $NO_ENCRYPT; then
  command -v 7z >/dev/null 2>&1 || die "7z (p7zip) required for encrypted bundles"
fi

id "$TENANT" >/dev/null 2>&1 || die "OS user '$TENANT' not found"
UID_T="$(id -u "$TENANT")"
HOME_T="$(getent passwd "$TENANT" | cut -d: -f6)"
if ! $DRY_RUN; then
  [[ ! -L "$TENANT_LIFECYCLE_LOCK_DIR" ]]
  install -d -o root -g root -m 0700 "$TENANT_LIFECYCLE_LOCK_DIR"
  [[ "$(stat -c '%u:%a' "$TENANT_LIFECYCLE_LOCK_DIR" 2>/dev/null)" == "0:700" ]] \
    || die "unsafe tenant lifecycle lock directory"
  lifecycle_lock_file="$TENANT_LIFECYCLE_LOCK_DIR/$TENANT.lock"
  [[ ! -L "$lifecycle_lock_file" && ( ! -e "$lifecycle_lock_file" || -f "$lifecycle_lock_file" ) ]] \
    || die "unsafe tenant lifecycle lock file"
  if [[ ! -e "$lifecycle_lock_file" ]]; then (umask 077; : >"$lifecycle_lock_file"); fi
  chmod 0600 "$lifecycle_lock_file"
  exec {EXPORT_LIFECYCLE_LOCK_FD}>"$lifecycle_lock_file"
  flock -x "$EXPORT_LIFECYCLE_LOCK_FD"
  export LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_FD="$EXPORT_LIFECYCLE_LOCK_FD"
  export LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_TENANT="$TENANT"
fi
LWROOT="$HOME_T/lunarwing"
ENVF="$LWROOT/env/lunarwing.env"
BRIDGE_ENVF="$LWROOT/env/xmpp-bridge.env"
VISION_ENVF="$LWROOT/env/vision.env"
STATE_DIR="$LWROOT/state"
[[ -f "$ENVF" ]] || die "tenant env not found: $ENVF (is '$TENANT' a LunarWing tenant on this host?)"
MANAGED_DARKIRC=false
if [[ -e "$STATE_DIR/darkirc/key-exchange" || -L "$STATE_DIR/darkirc/key-exchange" ]]; then
  MANAGED_DARKIRC=true
fi

# Read scope under registry lock, release it, then acquire locks in mt-admin's
# writer-before-registry order. Revalidation prevents scope replacement races.
if ! $MANAGED_DARKIRC && [[ -f "$PORTS_REGISTRY" && ! -L "$PORTS_REGISTRY" ]]; then
  registry_dir="$(dirname "$PORTS_REGISTRY")"
  exec {DARKIRC_PORTS_LOCK_FD}<"$registry_dir"
  flock -x "$DARKIRC_PORTS_LOCK_FD"
  darkirc_scope="$(jq -r --arg tenant "$TENANT" '.tenants[$tenant].darkirc_scope_id // empty' "$PORTS_REGISTRY" 2>/dev/null || true)"
  flock -u "$DARKIRC_PORTS_LOCK_FD"
  exec {DARKIRC_PORTS_LOCK_FD}>&-
  if [[ "$darkirc_scope" =~ ^[0-9a-f]{32}$ && "$darkirc_scope" != "00000000000000000000000000000000" ]]; then
    [[ ! -L "$DARKIRC_WRITER_LOCK_DIR" ]]
    install -d -o root -g root -m 0700 "$DARKIRC_WRITER_LOCK_DIR"
    [[ "$(stat -c '%u:%a' "$DARKIRC_WRITER_LOCK_DIR" 2>/dev/null)" == "0:700" ]] \
      || die "unsafe DarkIRC writer lock directory"
    darkirc_lock_file="$DARKIRC_WRITER_LOCK_DIR/$darkirc_scope.lock"
    [[ ! -L "$darkirc_lock_file" && ( ! -e "$darkirc_lock_file" || -f "$darkirc_lock_file" ) ]] \
      || die "unsafe DarkIRC writer lock file"
    if [[ ! -e "$darkirc_lock_file" ]]; then (umask 077; : >"$darkirc_lock_file"); fi
    chmod 0600 "$darkirc_lock_file"
    exec {DARKIRC_WRITER_LOCK_FD}>"$darkirc_lock_file"
    flock -x "$DARKIRC_WRITER_LOCK_FD"
    exec {DARKIRC_PORTS_LOCK_FD}<"$registry_dir"
    flock -x "$DARKIRC_PORTS_LOCK_FD"
    verified_darkirc_scope="$(jq -r --arg tenant "$TENANT" '.tenants[$tenant].darkirc_scope_id // empty' "$PORTS_REGISTRY" 2>/dev/null || true)"
    [[ "$verified_darkirc_scope" == "$darkirc_scope" ]] \
      || die "DarkIRC scope changed while export lock was acquired"
    flock -u "$DARKIRC_PORTS_LOCK_FD"
    exec {DARKIRC_PORTS_LOCK_FD}>&-
  fi
fi

# Validate critical source secrets before prompting or stopping tenant services.
SOURCE_MASTER_KEY="$(read_env_value "$ENVF" SECRETS_MASTER_KEY)"
[[ "$SOURCE_MASTER_KEY" =~ ^[0-9a-fA-F]{64}$ ]] \
  || die "SECRETS_MASTER_KEY is missing or invalid in $ENVF — refusing export before quiescence"
DAEMON_XMPP_PASSWORD="$(read_env_value "$ENVF" XMPP_PASSWORD)"
BRIDGE_XMPP_PASSWORD="$(read_env_value "$BRIDGE_ENVF" XMPP_PASSWORD)"
if [[ -n "$DAEMON_XMPP_PASSWORD" || -n "$BRIDGE_XMPP_PASSWORD" ]]; then
  [[ -n "$DAEMON_XMPP_PASSWORD" && "$DAEMON_XMPP_PASSWORD" == "$BRIDGE_XMPP_PASSWORD" ]] \
    || die "XMPP_PASSWORD differs between lunarwing.env and xmpp-bridge.env — reconcile the source tenant before export"
fi

DB_BACKEND="$(sed -n 's/^DATABASE_BACKEND=//p' "$ENVF" | head -1)"; DB_BACKEND="${DB_BACKEND:-postgres}"
[[ "$DB_BACKEND" == "postgres" ]] || die "DATABASE_BACKEND=$DB_BACKEND: machine migration supports postgres only (libSQL would need a manual DB-file copy + a libSQL target). Aborting rather than risk a silent empty import."

# Parse role + db from DATABASE_URL (handles a pre-rename source named ironclaw).
# In-process parameter expansion — the password never reaches argv/logs.
DBURL="$(sed -n 's/^DATABASE_URL=//p' "$ENVF" | head -1)"
PG_ROLE="lunarwing"; PG_DB="lunarwing"
if [[ "$DBURL" == postgres://* ]]; then
  _u="${DBURL#postgres://}"; PG_ROLE="${_u%%:*}"; PG_ROLE="${PG_ROLE:-lunarwing}"
  _d="${DBURL##*/}"; _d="${_d%%\?*}"; PG_DB="${_d:-lunarwing}"
fi

# Find which runtime actually owns the container (don't assume podman-first).
# For rootless podman, containers live in the tenant user's storage — root
# can't see them, so we must probe as the tenant user (matching mt-admin's
# _ctr pattern).
PG="lunarwing-pg-$TENANT"

# Run a container command as the tenant user when rootless podman is in use.
_ctr() {
  local name="$TENANT"
  local uid home
  uid="$(id -u "$name")" || die "cannot resolve uid for tenant '$name'"
  home="$(getent passwd "$name" | cut -d: -f6)"
  ( cd / && exec sudo -u "$name" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" "$@" )
}

pick_runtime() {
  if [[ -n "${LUNARWING_CONTAINER_RUNTIME:-}" ]]; then echo "$LUNARWING_CONTAINER_RUNTIME"; return 0; fi
  local rt
  # First try root-visible containers (rootful docker/podman)
  for rt in podman docker; do
    command -v "$rt" >/dev/null 2>&1 || continue
    "$rt" inspect "$PG" >/dev/null 2>&1 && { echo "$rt"; return 0; }
  done
  # Fall back to rootless podman as the tenant user
  if command -v podman >/dev/null 2>&1; then
    _ctr podman inspect "$PG" >/dev/null 2>&1 && { echo "podman"; return 0; }
  fi
  return 1
}
RUNTIME="$(pick_runtime || true)"
[[ -n "$RUNTIME" ]] || die "could not find container '$PG' in podman or docker — is the tenant's PostgreSQL container present on this host? (set LUNARWING_CONTAINER_RUNTIME to force)"

# Refuse a missing or malformed passphrase before stopping any tenant service.
if ! $DRY_RUN && ! $NO_ENCRYPT; then
  acquire_export_passphrase
fi
if $MANAGED_DARKIRC; then
  [[ -x "$MT_ADMIN" && ! -L "$MT_ADMIN" ]] \
    || die "managed DarkIRC state requires trusted mt-admin: $MT_ADMIN"
  if $DRY_RUN; then
    run "$MT_ADMIN" darkirc-contact export-migration "$TENANT"
  else
    DARKIRC_SCOPE_ID="$(jq -r --arg tenant "$TENANT" '.tenants[$tenant].darkirc_scope_id // empty' "$PORTS_REGISTRY" 2>/dev/null || true)"
    [[ "$DARKIRC_SCOPE_ID" =~ ^[0-9a-f]{32}$ && "$DARKIRC_SCOPE_ID" != "00000000000000000000000000000000" ]] \
      || die "managed DarkIRC tenant has no valid stable scope ID"
    [[ ! -L "$DARKIRC_WRITER_LOCK_DIR" ]]
    install -d -o root -g root -m 0700 "$DARKIRC_WRITER_LOCK_DIR"
    [[ "$(stat -c '%u:%a' "$DARKIRC_WRITER_LOCK_DIR" 2>/dev/null)" == "0:700" ]] \
      || die "unsafe DarkIRC writer lock directory"
    darkirc_lock_file="$DARKIRC_WRITER_LOCK_DIR/$DARKIRC_SCOPE_ID.lock"
    [[ ! -L "$darkirc_lock_file" && ( ! -e "$darkirc_lock_file" || -f "$darkirc_lock_file" ) ]] \
      || die "unsafe DarkIRC writer lock file"
    if [[ ! -e "$darkirc_lock_file" ]]; then (umask 077; : >"$darkirc_lock_file"); fi
    chmod 0600 "$darkirc_lock_file"
    exec {DARKIRC_WRITER_LOCK_FD}>"$darkirc_lock_file"
    flock -x -w 10 "$DARKIRC_WRITER_LOCK_FD" \
      || die "DarkIRC config update is busy for tenant '$TENANT'"
    registry_dir="$(dirname "$PORTS_REGISTRY")"
    exec {DARKIRC_PORTS_LOCK_FD}<"$registry_dir"
    flock -x "$DARKIRC_PORTS_LOCK_FD"
    verified_darkirc_scope="$(jq -r --arg tenant "$TENANT" '.tenants[$tenant].darkirc_scope_id // empty' "$PORTS_REGISTRY" 2>/dev/null || true)"
    [[ "$verified_darkirc_scope" == "$DARKIRC_SCOPE_ID" ]] \
      || die "DarkIRC scope changed while export lock was acquired"
    flock -u "$DARKIRC_PORTS_LOCK_FD"
    exec {DARKIRC_PORTS_LOCK_FD}>&-
    LUNARWING_MT_INHERITED_DARKIRC_LOCK_FD="$DARKIRC_WRITER_LOCK_FD" \
    LUNARWING_MT_INHERITED_DARKIRC_LOCK_SCOPE="$DARKIRC_SCOPE_ID" \
      "$MT_ADMIN" darkirc-contact export-migration "$TENANT"
    DARKIRC_MIGRATION_FILE="$STATE_DIR/darkirc/key-exchange/darkirc-contacts-v1.json"
    [[ -f "$DARKIRC_MIGRATION_FILE" && ! -L "$DARKIRC_MIGRATION_FILE" ]] \
      || die "DarkIRC structured migration manifest was not produced"
    DARKIRC_INNER_LOCK="$STATE_DIR/darkirc/key-exchange/update.lock"
    [[ -f "$DARKIRC_INNER_LOCK" && ! -L "$DARKIRC_INNER_LOCK" ]] \
      || die "unsafe DarkIRC helper lock path"
    exec {DARKIRC_INNER_LOCK_FD}<>"$DARKIRC_INNER_LOCK"
    inner_owner="$(stat -c '%u' "/proc/self/fd/$DARKIRC_INNER_LOCK_FD")"
    inner_mode="$(stat -c '%a' "/proc/self/fd/$DARKIRC_INNER_LOCK_FD")"
    inner_links="$(stat -c '%h' "/proc/self/fd/$DARKIRC_INNER_LOCK_FD")"
    inner_identity="$(stat -c '%d:%i' "/proc/self/fd/$DARKIRC_INNER_LOCK_FD")"
    path_identity="$(stat -c '%d:%i' "$DARKIRC_INNER_LOCK" 2>/dev/null || true)"
    [[ "$inner_owner" == "$UID_T" && "$inner_mode" == 600 && "$inner_links" == 1 \
      && "$inner_identity" == "$path_identity" ]] \
      || die "unsafe DarkIRC helper lock file"
    flock -x -w 10 "$DARKIRC_INNER_LOCK_FD" \
      || die "DarkIRC helper state is busy for tenant '$TENANT'"
  fi
fi

# Determine if the tenant's containers are rootless (need _ctr wrapper).
_is_rootless() {
  "$RUNTIME" inspect "$PG" >/dev/null 2>&1 && return 1 || return 0
}

SRC_VER="$(sudo -u "$TENANT" git -C "$LWROOT" describe --tags --always 2>/dev/null || echo unknown)"
STAMP="$(date +%Y%m%d-%H%M%S)"

banner "Export tenant '$TENANT' (source $SRC_VER, runtime $RUNTIME, db $PG_DB/$PG_ROLE) — CUTOVER"
$DRY_RUN && say "*** DRY RUN — no changes will be made ***"

WORK="$(mktemp -d)"; SEAL_DIR=""
trap 'rm -rf "$WORK"; [[ -z "${SEAL_DIR:-}" ]] || rm -rf "$SEAL_DIR"' EXIT
chmod 0700 "$WORK"

# ---- 1. quiesce the writers (consistent + final snapshot) --------------------
banner "1/5  Quiesce daemon + bridge"
tenant_writers_running() {
  local unit state status service found=false
  if command -v systemctl >/dev/null 2>&1 \
    && sudo -u "$TENANT" XDG_RUNTIME_DIR="/run/user/$UID_T" \
      systemctl --user list-unit-files >/dev/null 2>&1; then
    for unit in "lunarwing-$TENANT.service" "xmpp-bridge-$TENANT.service"; do
      status=0
      state="$(sudo -u "$TENANT" XDG_RUNTIME_DIR="/run/user/$UID_T" \
        systemctl --user is-active "$unit" 2>/dev/null)" || status=$?
      case "$state" in
        active|activating|reloading|deactivating) return 0 ;;
        inactive|failed|unknown) ;;
        *) [[ "$status" -eq 3 || "$status" -eq 4 ]] || return 2 ;;
      esac
    done
    return 1
  fi
  if command -v rc-service >/dev/null 2>&1; then
    for service in "lunarwing-$TENANT" "xmpp-bridge-$TENANT"; do
      [[ -x "/etc/init.d/$service" ]] || continue
      found=true
      status=0
      rc-service "$service" status >/dev/null 2>&1 || status=$?
      [[ "$status" -ne 0 ]] || return 0
      [[ "$status" -eq 3 ]] || return 2
    done
    [[ "$found" == true ]] && return 1
  fi
  return 2
}
if $NO_QUIESCE; then
  if ! $DRY_RUN; then
    writer_status=0
    tenant_writers_running || writer_status=$?
    case "$writer_status" in
      0) die "--no-quiesce given but the '$TENANT' daemon or bridge is still running — stop both first for a consistent snapshot" ;;
      1) ;;
      *) die "could not verify that the '$TENANT' daemon and bridge are stopped" ;;
    esac
  fi
  note "--no-quiesce: assuming daemon + bridge are already stopped"
else
  if command -v systemctl >/dev/null 2>&1 \
     && sudo -u "$TENANT" XDG_RUNTIME_DIR="/run/user/$UID_T" systemctl --user list-unit-files "lunarwing-$TENANT.service" >/dev/null 2>&1; then
    run sudo -u "$TENANT" env XDG_RUNTIME_DIR="/run/user/$UID_T" systemctl --user stop "lunarwing-$TENANT.service" "xmpp-bridge-$TENANT.service"
  elif command -v rc-service >/dev/null 2>&1; then
    run rc-service "lunarwing-$TENANT" stop
    run rc-service "xmpp-bridge-$TENANT" stop
  else
    die "could not detect the init system to stop the writers; stop the '$TENANT' daemon + xmpp-bridge manually and re-run with --no-quiesce"
  fi
  note "daemon + bridge stopped (PostgreSQL left running for the dump)"
fi

# ---- 2. database (PG container must still be up) -----------------------------
banner "2/5  Database (pg_dump -Fc)"
if $DRY_RUN; then
  note "[dry-run] would: $RUNTIME exec $PG pg_dump -U $PG_ROLE -Fc $PG_DB > db.dump (then verify PGDMP)"
else
  ROOTLESS="$(_is_rootless && echo true || echo false)"
  if [[ "$ROOTLESS" == "true" ]]; then
    _ctr podman inspect -f '{{.State.Running}}' "$PG" 2>/dev/null | grep -q true \
      || die "DB container $PG is not running — start just the PG container so pg_dump can run, then re-export"
    ( umask 077; _ctr podman exec "$PG" pg_dump -U "$PG_ROLE" -Fc "$PG_DB" > "$WORK/db.dump" ) \
      || die "pg_dump failed (role=$PG_ROLE db=$PG_DB) — check the names parsed from DATABASE_URL"
  else
    "$RUNTIME" inspect -f '{{.State.Running}}' "$PG" 2>/dev/null | grep -q true \
      || die "DB container $PG is not running — start just the PG container so pg_dump can run, then re-export"
    ( umask 077; "$RUNTIME" exec "$PG" pg_dump -U "$PG_ROLE" -Fc "$PG_DB" > "$WORK/db.dump" ) \
      || die "pg_dump failed (role=$PG_ROLE db=$PG_DB) — check the names parsed from DATABASE_URL"
  fi
  [[ "$(head -c5 "$WORK/db.dump")" == "PGDMP" ]] || die "pg_dump output is not a valid PGDMP archive"
  [[ "$(stat -c%s "$WORK/db.dump")" -gt 0 ]] || die "pg_dump produced an empty file"
  say "  db.dump: $(du -h "$WORK/db.dump" | cut -f1) (verified PGDMP)"
fi

# ---- 3. carry-over manifests (secrets + operator config) ---------------------
banner "3/5  Secrets + config manifest"
  LW_KEYS=(SECRETS_MASTER_KEY XMPP_JID XMPP_PASSWORD XMPP_DM_POLICY XMPP_ALLOW_FROM
         XMPP_ALLOW_ROOMS XMPP_ENCRYPTED_ROOMS XMPP_ALLOW_PLAINTEXT_FALLBACK
         XMPP_OMEMO_DEVICE_ID LLM_API_KEY LLM_MODEL NANOCODE_MODEL OPENCODE_MODEL KIMI_MODEL KIMI_API_KEY GOTIFY_URL
         GATEWAY_HOST HTTP_HOST)   # bind ADDRESS is operator config (carried); the
                                   # PORTS are host-specific and regenerated by add-tenant.
                                   # local proxy URLs are handled specially below.
BRIDGE_KEYS=(XMPP_JID XMPP_PASSWORD XMPP_DM_POLICY XMPP_ALLOW_FROM_JSON
             XMPP_ALLOW_ROOMS_JSON XMPP_ENCRYPTED_ROOMS_JSON XMPP_DEVICE_ID
             XMPP_ALLOW_PLAINTEXT_FALLBACK)
VISION_KEYS=(VL_URL VL_MODEL)

# Append KEY=line from <src> to <dest> if present (CR-stripped). The `if` form (not
# `&& printf`) keeps a missing last key from making the function return non-zero
# under set -e.
copy_key() {  # <src> <dest> <key>
  local line; line="$(grep -m1 "^${3}=" "$1" 2>/dev/null || true)"
  if [[ -n "$line" ]]; then printf '%s\n' "${line%$'\r'}" >> "$2"; fi
}

if $DRY_RUN; then
  note "[dry-run] would extract ${#LW_KEYS[@]}+ keys from lunarwing.env (incl. SECRETS_MASTER_KEY) and ${#BRIDGE_KEYS[@]} from xmpp-bridge.env; intra-host tokens are NOT carried"
  if [[ -f "$VISION_ENVF" ]]; then
    note "[dry-run] would extract ${#VISION_KEYS[@]} keys from vision.env (VL_URL, VL_MODEL; auth token regenerated on target)"
  else
    note "[dry-run] no vision.env at $VISION_ENVF — no vision manifest would be written"
  fi
else
  ( umask 077; : > "$WORK/manifest-lunarwing.env"; : > "$WORK/manifest-bridge.env" )
  for k in "${LW_KEYS[@]}"; do copy_key "$ENVF" "$WORK/manifest-lunarwing.env" "$k"; done
  # LLM_BASE_URL: carry ONLY a non-local custom endpoint; a local proxy URL is
  # host-specific (its port differs on the new host, which sets its own).
  llm_url="$(read_env_value "$ENVF" LLM_BASE_URL)"
  if [[ -n "$llm_url" ]] && ! is_loopback_url "$llm_url"; then
    printf 'LLM_BASE_URL=%s\n' "$llm_url" >> "$WORK/manifest-lunarwing.env"
    note "carrying custom LLM_BASE_URL"
  else
    note "LLM_BASE_URL is host-local — NOT carried"
  fi
  opencode_url="$(read_env_value "$ENVF" OPENCODE_BASE_URL)"
  if [[ -n "$opencode_url" ]] && ! is_loopback_url "$opencode_url"; then
    printf 'OPENCODE_BASE_URL=%s\n' "$opencode_url" >> "$WORK/manifest-lunarwing.env"
    note "carrying custom OPENCODE_BASE_URL"
  elif [[ -n "$opencode_url" ]]; then
    note "OPENCODE_BASE_URL is host-local — NOT carried"
  fi
  kimi_url="$(read_env_value "$ENVF" KIMI_BASE_URL)"
  if [[ -n "$kimi_url" ]] && ! is_loopback_url "$kimi_url"; then
    printf 'KIMI_BASE_URL=%s\n' "$kimi_url" >> "$WORK/manifest-lunarwing.env"
    note "carrying custom KIMI_BASE_URL"
  elif [[ -n "$kimi_url" ]]; then
    note "KIMI_BASE_URL is host-local — NOT carried"
  fi
  nanocode_url="$(read_env_value "$ENVF" NANOCODE_BASE_URL)"
  if [[ -n "$nanocode_url" ]] && ! is_loopback_url "$nanocode_url"; then
    printf 'NANOCODE_BASE_URL=%s\n' "$nanocode_url" >> "$WORK/manifest-lunarwing.env"
    note "carrying custom NANOCODE_BASE_URL"
  elif [[ -n "$nanocode_url" ]]; then
    note "NANOCODE_BASE_URL is host-local — NOT carried"
  fi
  for k in "${BRIDGE_KEYS[@]}"; do copy_key "$BRIDGE_ENVF" "$WORK/manifest-bridge.env" "$k"; done
  if [[ -f "$VISION_ENVF" ]]; then
    ( umask 077; : > "$WORK/manifest-vision.env" )
    for k in "${VISION_KEYS[@]}"; do copy_key "$VISION_ENVF" "$WORK/manifest-vision.env" "$k"; done
    if [[ -s "$WORK/manifest-vision.env" ]]; then
      say "  manifest-vision.env:    $(grep -c '=' "$WORK/manifest-vision.env" 2>/dev/null || true) keys"
    else
      rm -f "$WORK/manifest-vision.env"
      note "vision.env had no carried keys — no vision manifest written"
    fi
  else
    note "no vision.env at $VISION_ENVF — vision sidecar config not carried"
  fi
  grep -q '^SECRETS_MASTER_KEY=' "$WORK/manifest-lunarwing.env" \
    || die "SECRETS_MASTER_KEY not found in $ENVF — refusing to export a bundle that can't decrypt the DB. Locate the key first."
  say "  manifest-lunarwing.env: $(grep -c '=' "$WORK/manifest-lunarwing.env") keys (incl. SECRETS_MASTER_KEY)"
  say "  manifest-bridge.env:    $(grep -c '=' "$WORK/manifest-bridge.env" 2>/dev/null || true) keys"
fi

# ---- 4. on-disk state (OMEMO store + workspace) ------------------------------
banner "4/5  State directory"
if [[ -d "$STATE_DIR" ]]; then
  if $DRY_RUN; then
    note "[dry-run] would: tar czf state.tar.gz -C $LWROOT state (excluding *.sock, state/config.toml, *.wasm)"
    if [[ -d "$STATE_DIR/xmpp" ]]; then
      note "  includes OMEMO store state/xmpp"
    else
      note "  (no OMEMO store present yet)"
    fi
  else
    state_excludes=(
      --exclude='*.sock' --exclude='state/config.toml'
      --exclude='state/tools/*.wasm' --exclude='state/channels/*.wasm'
      --exclude='state/darkirc/key-exchange'
    )
    if $MANAGED_DARKIRC; then
      state_excludes+=(--exclude='state/darkirc')
      exec {DARKIRC_MANIFEST_FD}<"$DARKIRC_MIGRATION_FILE"
      manifest_owner="$(stat -c '%u' "/proc/self/fd/$DARKIRC_MANIFEST_FD")"
      manifest_mode="$(stat -c '%a' "/proc/self/fd/$DARKIRC_MANIFEST_FD")"
      manifest_links="$(stat -c '%h' "/proc/self/fd/$DARKIRC_MANIFEST_FD")"
      [[ "$manifest_owner" == "$UID_T" && "$manifest_mode" == 600 && "$manifest_links" == 1 ]] \
        || die "unsafe DarkIRC structured migration manifest"
      (umask 077; dd status=none <&"$DARKIRC_MANIFEST_FD" >"$WORK/darkirc-contacts-v1.json")
      exec {DARKIRC_MANIFEST_FD}>&-
    fi
    # Exclude: sockets; the host-specific config.toml (the new host's add-tenant
    # writes the correct external-worker ports); and *.wasm (rebuilt by install-wasm
    # — carrying stale v1.1.0 artifacts would linger past the never-cleaning overlay).
    tar czf "$WORK/state.tar.gz" -C "$LWROOT" "${state_excludes[@]}" state
    if ! $MANAGED_DARKIRC; then
      [[ ! -e "$STATE_DIR/darkirc/key-exchange" && ! -L "$STATE_DIR/darkirc/key-exchange" ]] \
        || die "managed DarkIRC state appeared during export; retry structured export"
    fi
    say "  state.tar.gz: $(du -h "$WORK/state.tar.gz" | cut -f1)$( [[ -d "$STATE_DIR/xmpp" ]] && echo ' (incl. OMEMO store)' )"
  fi
else
  note "no state dir at $STATE_DIR — nothing to bundle (OMEMO/workspace start fresh on import)"
fi

# ---- 5. seal the bundle ------------------------------------------------------
banner "5/5  Seal bundle"

if $DRY_RUN; then
  if $NO_ENCRYPT; then
    note "[dry-run] would write meta.txt and seal -> $OUT_DIR/${TENANT}-migrate-${STAMP}.tar (0600, UNENCRYPTED)"
  else
    note "[dry-run] would write meta.txt and seal -> $OUT_DIR/${TENANT}-migrate-${STAMP}.7z (0600, AES-256 encrypted)"
  fi
  say ""; say "DRY RUN complete — no bundle written, services NOT stopped."
  exit 0
fi
[[ "$OUT_DIR" == /* ]] || die "output directory must be absolute"
output_parent="$(dirname "$OUT_DIR")"
validate_secure_output_directory "$output_parent" \
  || die "output directory parent is unsafe"
[[ ! -L "$OUT_DIR" && ( ! -e "$OUT_DIR" || -d "$OUT_DIR" ) ]] \
  || die "output directory is unsafe"
install -d -o root -g root -m 0700 "$OUT_DIR"
validate_secure_output_directory "$OUT_DIR" \
  || die "output directory must be root-owned and non-symlinked"
SEAL_DIR="$(mktemp -d "$OUT_DIR/.seal.XXXXXX")"
chmod 0700 "$SEAL_DIR"
{
  printf 'tenant=%s\n' "$TENANT"
  if $MANAGED_DARKIRC; then printf 'darkirc_scope_id=%s\n' "$DARKIRC_SCOPE_ID"; fi
  printf 'source_version=%s\n' "$SRC_VER"
  printf 'source_runtime=%s\n' "$RUNTIME"
  printf 'pg_role=%s\n' "$PG_ROLE"
  printf 'pg_db=%s\n' "$PG_DB"
  printf 'db_backend=%s\n' "$DB_BACKEND"
  printf 'created=%s\n' "$STAMP"
} > "$WORK/meta.txt"

if $NO_ENCRYPT; then
  # Legacy plaintext tar — for testing only
  BUNDLE="$OUT_DIR/${TENANT}-migrate-${STAMP}.tar"
  BUNDLE_CANDIDATE="$SEAL_DIR/bundle.tar"
  ( umask 077; tar cf "$BUNDLE_CANDIDATE" -C "$WORK" . )
  say "  WARNING: bundle is UNENCRYPTED (--no-encrypt)" >&2
else
  # Encrypted 7z with header encryption
  BUNDLE="$OUT_DIR/${TENANT}-migrate-${STAMP}.7z"
  BUNDLE_CANDIDATE="$SEAL_DIR/bundle.7z"

  # Bare -p reads the password twice from stdin and keeps it out of process argv.
  { printf '%s\n%s\n' "$KAWARIMI_ARCHIVE_PASS" "$KAWARIMI_ARCHIVE_PASS"; } | 7z a -t7z \
    -mhe=on -p \
    -mx=5 \
    "$BUNDLE_CANDIDATE" "$WORK"/* \
    >/dev/null 2>&1 || die "7z encryption failed"
  unset KAWARIMI_ARCHIVE_PASS
fi
chmod 0600 "$BUNDLE_CANDIDATE"
[[ ! -e "$BUNDLE" && ! -L "$BUNDLE" ]] || die "output bundle already exists"
ln "$BUNDLE_CANDIDATE" "$BUNDLE" || die "failed to publish output bundle"
rm -f "$BUNDLE_CANDIDATE"
rmdir "$SEAL_DIR"
SEAL_DIR=""

banner "Done — bundle written (tenant is STOPPED — cutover in progress)"
say "  $BUNDLE ($(du -h "$BUNDLE" | cut -f1))"
say ""
if $NO_ENCRYPT; then
say "WARNING: bundle is UNENCRYPTED. Contains SECRETS_MASTER_KEY + XMPP password."
say "         Transfer over ssh (0600), delete from both hosts after verify."
else
say "The '$TENANT' daemon + bridge are now STOPPED on this host (PG still up for the"
say "dump; you may stop it too). The 7z bundle is AES-256 encrypted — the passphrase"
say "is required to even list contents. Transfer over ssh, and delete from both hosts after verify."
fi
say ""
say "Next, on the NEW host:  sudo ic/scripts/import-tenant.sh $(basename "$BUNDLE") --start"
say "Rollback: this host is intact — restart '$TENANT' here to abort the migration."
