#!/usr/bin/env bash
#
# export-tenant.sh — package one multi-tenant tenant into a portable bundle for
# MACHINE MIGRATION to a fresh LunarWing host (see docs/ops/MT-MACHINE-MIGRATION.md).
#
# Runs on the OLD (source) host. Init-specific lifecycle work is delegated to the
# source mt-admin's `stop-writers`/`writers-active` contract; the script also needs
# the container runtime, jq, and tar.
#
# IMPORTANT — this is the START OF CUTOVER, not a read-only snapshot. To capture a
# CONSISTENT and FINAL picture (so nothing is written after the snapshot and the
# OMEMO double-ratchet store isn't torn mid-write), it STOPS all tenant writers
# first (PostgreSQL stays up only for pg_dump). Plan a maintenance window.
#
# Bundle (a single 0600 tar):
#   meta.txt                 tenant, source version/runtime, pg role/db, timestamp
#   db.dump                  pg_dump -Fc of the tenant DB
#   manifest-lunarwing.env   carry-over keys for lunarwing.env (0600) — see CARRY below
#   manifest-bridge.env      carry-over keys for xmpp-bridge.env (0600)
#   manifest-vision.env      optional carry-over keys for vision.env (0600)
#   state.tar.gz             state dir (OMEMO store + workspace + tool storage),
#                            EXCLUDING sockets, host-specific config.toml, *.wasm,
#                            and the secret-bearing state/darkirc subtree
#   darkirc-contacts-v1.json optional structured contact-only migration manifest
#
# CARRY policy: only values that MUST match the source are carried — SECRETS_MASTER_KEY
# (the AES-256-GCM vault key, without which the DB's encrypted secret rows are
# unrecoverable), the XMPP identity (JID + password), and operator config (XMPP
# rooms/allowlist/OMEMO + LLM model/key). Intra-host tokens (gateway/bridge/webhook/
# relay) are intentionally NOT carried — the new host mints fresh, self-consistent
# ones (gateway UI / external webhook senders re-auth after cutover). Host-specific
# LLM_BASE_URL and OPENCODE_BASE_URL local proxies are carried ONLY when they
# point at non-local custom endpoints. Vision sidecar ports are regenerated, but
# a custom VL_URL/VL_MODEL and sidecar auth token are carried when vision.env exists.
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
set -euo pipefail

OUT_DIR="${LUNARWING_MIGRATE_DIR:-/var/lib/lunarwing-migrate}"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
MT="${LUNARWING_MT_ADMIN:-$SCRIPT_DIR/lunarwing-mt-admin.sh}"
PORTS_REGISTRY="${LUNARWING_PORTS_REGISTRY:-/etc/lunarwing/ports.json}"
NO_QUIESCE=false
DRY_RUN=false
TENANT=""

# The Rust helper owns all DarkIRC secret parsing and structured migration.  The
# shell wrapper only handles the fixed, owner-only manifest path and metadata.
DARKIRC_MANIFEST_NAME="darkirc-contacts-v1.json"
DARKIRC_MANIFEST_REL="key-exchange/$DARKIRC_MANIFEST_NAME"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out-dir)    OUT_DIR="$2"; shift 2 ;;
    --no-quiesce) NO_QUIESCE=true; shift ;;
    --dry-run)    DRY_RUN=true; shift ;;
    -*)           printf 'unknown arg: %s\n' "$1" >&2; exit 2 ;;
    *)            TENANT="$1"; shift ;;
  esac
done

say()    { printf '%s\n' "$*"; }
die()    { printf 'error: %s\n' "$*" >&2; exit 1; }
banner() { printf '\n========== %s ==========\n' "$*"; }
note()   { printf '  · %s\n' "$*"; }
run()    { if $DRY_RUN; then printf '  [dry-run] %s\n' "$*"; return 0; fi; printf '  + %s\n' "$*"; "$@"; }

path_components_no_symlink() {
  local path="$1" rest component current="/"
  [[ "$path" == /* ]] || return 1
  rest="${path#/}"
  while [[ -n "$rest" ]]; do
    component="${rest%%/*}"
    if [[ "$rest" == */* ]]; then
      rest="${rest#*/}"
    else
      rest=""
    fi
    [[ -n "$component" && "$component" != . && "$component" != .. ]] || return 1
    current="${current%/}/$component"
    [[ ! -L "$current" ]] || return 1
    if [[ -n "$rest" ]]; then
      [[ -d "$current" ]] || return 1
    fi
  done
}

validate_export_output_dir() {
  local parent owner mode parent_owner parent_mode current rest component component_owner component_mode
  [[ "$OUT_DIR" == /* && "$OUT_DIR" != / ]] \
    || die "export output directory must be an absolute path"
  path_components_no_symlink "$OUT_DIR" \
    || die "unsafe export output path: $OUT_DIR"
  if [[ "$EUID" -eq 0 ]]; then
    # Every existing ancestor must be root-owned.  Otherwise its owner could
    # rename the validated directory between this check and bundle creation.
    current=/
    rest="${OUT_DIR#/}"
    while [[ -n "$rest" ]]; do
      component="${rest%%/*}"
      if [[ "$rest" == */* ]]; then
        rest="${rest#*/}"
      else
        rest=""
      fi
      current="${current%/}/$component"
      [[ -e "$current" ]] || continue
      component_owner="$(stat -c '%u' "$current" 2>/dev/null || true)"
      component_mode="$(stat -c '%a' "$current" 2>/dev/null || true)"
      [[ "$component_owner" == 0 && "$component_mode" =~ ^[0-7]+$ ]] \
        || die "root export path has an untrusted owner: $current"
      (( (8#$component_mode & 07022) == 0 )) \
        || die "root export path has unsafe permissions: $current"
    done
  fi
  if [[ -e "$OUT_DIR" || -L "$OUT_DIR" ]]; then
    [[ -d "$OUT_DIR" && ! -L "$OUT_DIR" ]] \
      || die "export output path is not a regular directory: $OUT_DIR"
    owner="$(stat -c '%u' "$OUT_DIR" 2>/dev/null || true)"
    mode="$(stat -c '%a' "$OUT_DIR" 2>/dev/null || true)"
    [[ "$owner" =~ ^[0-9]+$ && "$mode" =~ ^[0-7]+$ ]] \
      || die "could not validate export output directory: $OUT_DIR"
    (( (8#$mode & 0022) == 0 )) \
      || die "export output directory is group/world writable: $OUT_DIR"
    if [[ "$EUID" -eq 0 && "$owner" != 0 ]]; then
      die "root export output directory must be root-owned: $OUT_DIR"
    fi
  else
    parent="${OUT_DIR%/*}"
    [[ -n "$parent" ]] || parent=/
    path_components_no_symlink "$parent" \
      || die "unsafe export output parent: $parent"
    [[ -d "$parent" && ! -L "$parent" ]] \
      || die "export output parent is not a regular directory: $parent"
    parent_owner="$(stat -c '%u' "$parent" 2>/dev/null || true)"
    parent_mode="$(stat -c '%a' "$parent" 2>/dev/null || true)"
    [[ "$parent_owner" =~ ^[0-9]+$ && "$parent_mode" =~ ^[0-7]+$ ]] \
      || die "could not validate export output parent: $parent"
    if [[ "$EUID" -eq 0 && "$parent_owner" != 0 ]]; then
      die "root export output parent must be root-owned: $parent"
    fi
    (( (8#$parent_mode & 0022) == 0 )) \
      || die "export output parent is group/world writable: $parent"
    mkdir -- "$OUT_DIR" \
      || die "could not create export output directory: $OUT_DIR"
  fi
  chmod 0700 -- "$OUT_DIR" \
    || die "could not secure export output directory: $OUT_DIR"
  owner="$(stat -c '%u' "$OUT_DIR" 2>/dev/null || true)"
  mode="$(stat -c '%a' "$OUT_DIR" 2>/dev/null || true)"
  [[ "$mode" == 700 ]] || die "export output directory must be mode 0700: $OUT_DIR"
  if [[ "$EUID" -eq 0 && "$owner" != 0 ]]; then
    die "root export output directory must remain root-owned: $OUT_DIR"
  fi
}

validate_tenant_source_paths() {
  local uid path owner mode links
  uid="$(id -u "$TENANT" 2>/dev/null || true)"
  [[ "$uid" =~ ^[0-9]+$ ]] || die "could not resolve tenant UID"
  for path in "$HOME_T" "$LWROOT" "$LWROOT/env" "$STATE_DIR"; do
    path_components_no_symlink "$path" \
      || die "unsafe tenant path component: $path"
    [[ -d "$path" && ! -L "$path" ]] \
      || die "tenant path is not a regular directory: $path"
    owner="$(stat -c '%u' "$path" 2>/dev/null || true)"
    mode="$(stat -c '%a' "$path" 2>/dev/null || true)"
    [[ "$owner" == "$uid" && "$mode" =~ ^[0-7]+$ ]] \
      || die "tenant directory has unexpected ownership: $path"
    (( (8#$mode & 07022) == 0 )) \
      || die "tenant directory has unsafe permissions: $path"
  done

  for path in "$ENVF" "$BRIDGE_ENVF" "$VISION_ENVF"; do
    [[ "$path" == "$ENVF" || -e "$path" || -L "$path" ]] || continue
    path_components_no_symlink "$path" \
      || die "unsafe tenant environment path: $path"
    [[ -f "$path" && ! -L "$path" ]] \
      || die "tenant environment file is missing or unsafe: $path"
    owner="$(stat -c '%u' "$path" 2>/dev/null || true)"
    mode="$(stat -c '%a' "$path" 2>/dev/null || true)"
    links="$(stat -c '%h' "$path" 2>/dev/null || true)"
    [[ "$owner" == "$uid" && "$mode" == 600 && "$links" == 1 ]] \
      || die "tenant environment file must be owner-controlled, single-link mode 0600: $path"
  done

  if [[ -e "$DARKIRC_DIR" || -L "$DARKIRC_DIR" ]]; then
    path_components_no_symlink "$DARKIRC_DIR" \
      || die "unsafe DarkIRC state path: $DARKIRC_DIR"
    [[ -d "$DARKIRC_DIR" && ! -L "$DARKIRC_DIR" ]] \
      || die "unsafe DarkIRC state path: $DARKIRC_DIR"
    owner="$(stat -c '%u' "$DARKIRC_DIR" 2>/dev/null || true)"
    mode="$(stat -c '%a' "$DARKIRC_DIR" 2>/dev/null || true)"
    [[ "$owner" == "$uid" && "$mode" == 700 ]] \
      || die "DarkIRC state directory must be tenant-owned mode 0700"
  fi
}

[[ -n "$TENANT" ]] || die "usage: $0 <tenant> [--out-dir DIR] [--no-quiesce] [--dry-run]"
[[ "$TENANT" =~ ^[a-z0-9][a-z0-9-]{0,63}$ ]] \
  || die "unsafe tenant name '$TENANT' (use lowercase letters, digits, and hyphens)"
[[ "$(id -u)" -eq 0 ]] || die "run as root (sudo): stops the tenant services, reads its home/state, execs its DB container"
command -v jq  >/dev/null 2>&1 || die "jq required"
command -v tar >/dev/null 2>&1 || die "tar required"

[[ -x "$MT" ]] || die "mt-admin not found/executable at $MT"
grep -qE '^\s*stop-writers\)' "$MT" \
  || die "mt-admin at $MT lacks stop-writers (required for migration quiesce)"
grep -qE '^\s*writers-active\)' "$MT" \
  || die "mt-admin at $MT lacks writers-active (required for migration safety checks)"

id "$TENANT" >/dev/null 2>&1 || die "OS user '$TENANT' not found"
HOME_T="$(getent passwd "$TENANT" | cut -d: -f6)"
LWROOT="$HOME_T/lunarwing"
ENVF="$LWROOT/env/lunarwing.env"
BRIDGE_ENVF="$LWROOT/env/xmpp-bridge.env"
VISION_ENVF="$LWROOT/env/vision.env"
STATE_DIR="$LWROOT/state"
DARKIRC_DIR="$STATE_DIR/darkirc"
REGISTRY_TENANT_JSON=""
DARKIRC_ENABLED=false
DARKIRC_SCOPE_ID=""
validate_tenant_source_paths

if [[ -f "$PORTS_REGISTRY" ]]; then
  REGISTRY_TENANT_JSON="$(jq -c --arg tenant "$TENANT" '.tenants[$tenant] // empty' "$PORTS_REGISTRY" 2>/dev/null || true)"
  if [[ -n "$REGISTRY_TENANT_JSON" ]]; then
    DARKIRC_ENABLED="$(jq -r '.enable_darkirc // false' <<<"$REGISTRY_TENANT_JSON" 2>/dev/null || printf 'false')"
    DARKIRC_SCOPE_ID="$(jq -r '.darkirc_scope_id // empty' <<<"$REGISTRY_TENANT_JSON" 2>/dev/null || true)"
  fi
fi

# A DarkIRC directory outside the registry is still migration-relevant.  It may
# contain manually-managed contacts from an older registry, so never silently
# discard it merely because enable_darkirc is false/missing.
if [[ -e "$DARKIRC_DIR" ]]; then
  [[ -d "$DARKIRC_DIR" && ! -L "$DARKIRC_DIR" ]] \
    || die "unsafe DarkIRC state path: $DARKIRC_DIR"
fi

[[ "$DARKIRC_ENABLED" == true || "$DARKIRC_ENABLED" == false ]] \
  || die "invalid enable_darkirc value for '$TENANT'"
if $DARKIRC_ENABLED; then
  [[ "$DARKIRC_SCOPE_ID" =~ ^[0-9a-f]{32}$ && ! "$DARKIRC_SCOPE_ID" =~ ^0+$ ]] \
    || die "DarkIRC tenant '$TENANT' has no valid darkirc_scope_id"
fi

darkirc_contacts_present() {
  local config="$DARKIRC_DIR/darkirc_config.toml"
  [[ -f "$config" && ! -L "$config" ]] || return 1
  # The shell deliberately treats any active config as migration-relevant. It
  # must not guess whether dotted/quoted TOML keys contain contacts and then
  # silently discard a manually managed contact from the generic archive.
}

darkirc_transactional_state_present() {
  local rel path
  [[ -d "$DARKIRC_DIR" ]] || return 1
  for rel in key-exchange/pending key-exchange/candidates key-exchange/transactions key-exchange/rollback; do
    path="$DARKIRC_DIR/$rel"
    if [[ -e "$path" || -L "$path" ]] && [[ ! -d "$path" || -L "$path" ]]; then
      return 0
    fi
    [[ -d "$path" ]] || continue
    if find "$path" -mindepth 1 -print -quit 2>/dev/null | grep -q .; then
      return 0
    fi
  done
  for rel in key-exchange/darkirc-contacts-v1.json.next \
             key-exchange/darkirc-contacts-v1.staged.json \
             key-exchange/darkirc-contacts-v1.staged.json.next; do
    path="$DARKIRC_DIR/$rel"
    if [[ -e "$path" || -L "$path" ]]; then
      return 0
    fi
  done
  # A ledger is metadata-only, but its presence means contacts require the
  # structured migration path rather than generic state.tar.gz.
  return 1
}

darkirc_ledger_unresolved() {
  local ledger="$DARKIRC_DIR/key-exchange/ledger.json"
  if [[ -L "$ledger" ]]; then
    return 0
  fi
  [[ -f "$ledger" ]] || return 1
  # The ledger contains fingerprints and state only; jq validates it without
  # emitting contact keys. Unknown states fail closed because they may represent
  # an unfinished verification/activation decision.
  jq -e '.schema == "lunarwing.darkirc-ledger/v1" and (.contacts | type == "object")' \
    "$ledger" >/dev/null 2>&1 \
    || return 0
  jq -e '
    [.contacts[]?.state]
    | any(.[];
        . as $state
        | ["legacy-active", "legacy-noncompliant", "PeerVerified", "Revoked", "Expired", "Cancelled"]
        | index($state) | not)
  ' "$ledger" >/dev/null 2>&1
}

darkirc_manifest_required() {
  $DARKIRC_ENABLED && return 0
  darkirc_contacts_present && return 0
  [[ -L "$DARKIRC_DIR/$DARKIRC_MANIFEST_REL" ]] && return 0
  [[ -f "$DARKIRC_DIR/$DARKIRC_MANIFEST_REL" ]] && return 0
  [[ -L "$DARKIRC_DIR/key-exchange/ledger.json" ]] && return 0
  [[ -f "$DARKIRC_DIR/key-exchange/ledger.json" ]] && return 0
  return 1
}

mt_has_darkirc_migration() {
  grep -qE 'export-migration' "$MT"
}

if darkirc_transactional_state_present; then
  die "DarkIRC has pending/candidate/transaction/rollback state; resolve it before export"
fi
if darkirc_ledger_unresolved; then
  die "DarkIRC ledger has an unresolved or invalid state; complete/cancel it before export"
fi
if darkirc_manifest_required; then
  mt_has_darkirc_migration \
    || die "mt-admin lacks darkirc-contact export-migration; refusing to export contact secrets generically"
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

# Determine if the tenant's containers are rootless (need _ctr wrapper).
_is_rootless() {
  "$RUNTIME" inspect "$PG" >/dev/null 2>&1 && return 1 || return 0
}

SRC_VER="$(sudo -u "$TENANT" git -C "$LWROOT" describe --tags --always 2>/dev/null || echo unknown)"
STAMP="$(date +%Y%m%d-%H%M%S)"

banner "Export tenant '$TENANT' (source $SRC_VER, runtime $RUNTIME, db $PG_DB/$PG_ROLE) — CUTOVER"
$DRY_RUN && say "*** DRY RUN — no changes will be made ***"

WORK="$(mktemp -d)"
DARKIRC_PIN_DIR=""
trap 'rm -rf "$WORK" "${DARKIRC_PIN_DIR:-}"' EXIT
chmod 0700 "$WORK"
if ! $DRY_RUN; then
  validate_export_output_dir
fi

# ---- 1. quiesce the writers (consistent + final snapshot) --------------------
banner "1/6  Quiesce tenant writers"
writers_active() {
  local rc=0
  "$MT" writers-active "$TENANT" >/dev/null 2>&1 || rc=$?
  case "$rc" in
    0) return 0 ;;
    1) return 1 ;;
    *) die "mt-admin writers-active failed for '$TENANT' (rc=$rc)" ;;
  esac
}
if $NO_QUIESCE; then
  if ! $DRY_RUN && writers_active; then die "--no-quiesce given but '$TENANT' still has active writers — stop them first for a consistent snapshot"; fi
  note "--no-quiesce: verified tenant writers are already stopped"
else
  run "$MT" stop-writers "$TENANT"
  if ! $DRY_RUN && writers_active; then
    die "mt-admin stop-writers returned but '$TENANT' writers are still active; refusing snapshot"
  fi
  note "tenant writers stopped (PostgreSQL left running for the dump)"
fi

# ---- 2. database (PG container must still be up) -----------------------------
banner "2/6  Database (pg_dump -Fc)"
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
banner "3/6  Secrets + config manifest"
LW_KEYS=(SECRETS_MASTER_KEY XMPP_JID XMPP_PASSWORD XMPP_DM_POLICY XMPP_ALLOW_FROM
         XMPP_ALLOW_ROOMS XMPP_ENCRYPTED_ROOMS XMPP_ALLOW_PLAINTEXT_FALLBACK
         XMPP_OMEMO_DEVICE_ID LLM_API_KEY LLM_MODEL OPENCODE_MODEL GOTIFY_URL
         GATEWAY_HOST HTTP_HOST)   # bind ADDRESS is operator config (carried); the
                                   # PORTS are host-specific and regenerated by add-tenant.
                                   # local proxy URLs are handled specially below.
BRIDGE_KEYS=(XMPP_JID XMPP_PASSWORD XMPP_DM_POLICY XMPP_ALLOW_FROM_JSON
             XMPP_ALLOW_ROOMS_JSON XMPP_ENCRYPTED_ROOMS_JSON XMPP_DEVICE_ID
             XMPP_ALLOW_PLAINTEXT_FALLBACK)
VISION_KEYS=(VL_URL VL_MODEL LUNARWING_AUTH_TOKEN)

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
    note "[dry-run] would extract ${#VISION_KEYS[@]} keys from vision.env (VL_URL, VL_MODEL, LUNARWING_AUTH_TOKEN)"
  else
    note "[dry-run] no vision.env at $VISION_ENVF — no vision manifest would be written"
  fi
else
  ( umask 077; : > "$WORK/manifest-lunarwing.env"; : > "$WORK/manifest-bridge.env" )
  for k in "${LW_KEYS[@]}"; do copy_key "$ENVF" "$WORK/manifest-lunarwing.env" "$k"; done
  # LLM_BASE_URL: carry ONLY a non-local custom endpoint; a local proxy URL is
  # host-specific (its port differs on the new host, which sets its own).
  llm_url="$(sed -n 's/^LLM_BASE_URL=//p' "$ENVF" | head -1)"; llm_url="${llm_url%$'\r'}"
  if [[ -n "$llm_url" && "$llm_url" != http://127.0.0.1:* && "$llm_url" != http://localhost:* ]]; then
    printf 'LLM_BASE_URL=%s\n' "$llm_url" >> "$WORK/manifest-lunarwing.env"
    note "carrying custom LLM_BASE_URL"
  else
    note "LLM_BASE_URL is the local proxy ($llm_url) — NOT carried (new host sets its own)"
  fi
  opencode_url="$(sed -n 's/^OPENCODE_BASE_URL=//p' "$ENVF" | head -1)"; opencode_url="${opencode_url%$'\r'}"
  if [[ -n "$opencode_url" && "$opencode_url" != http://127.0.0.1:* && "$opencode_url" != http://localhost:* ]]; then
    printf 'OPENCODE_BASE_URL=%s\n' "$opencode_url" >> "$WORK/manifest-lunarwing.env"
    note "carrying custom OPENCODE_BASE_URL"
  elif [[ -n "$opencode_url" ]]; then
    note "OPENCODE_BASE_URL is local ($opencode_url) — NOT carried (new host sets its own)"
  fi
  for k in "${BRIDGE_KEYS[@]}"; do copy_key "$BRIDGE_ENVF" "$WORK/manifest-bridge.env" "$k"; done
  if [[ -f "$VISION_ENVF" ]]; then
    ( umask 077; : > "$WORK/manifest-vision.env" )
    for k in "${VISION_KEYS[@]}"; do copy_key "$VISION_ENVF" "$WORK/manifest-vision.env" "$k"; done
    if [[ -s "$WORK/manifest-vision.env" ]]; then
      say "  manifest-vision.env:    $(grep -c '=' "$WORK/manifest-vision.env" 2>/dev/null || echo 0) keys"
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
  say "  manifest-bridge.env:    $(grep -c '=' "$WORK/manifest-bridge.env" 2>/dev/null || echo 0) keys"
fi

# ---- 4. on-disk state (OMEMO store + workspace) ------------------------------
banner "4/6  State directory"
if [[ -d "$STATE_DIR" ]]; then
  if $DRY_RUN; then
    note "[dry-run] would: tar czf state.tar.gz -C $LWROOT state (excluding *.sock, state/config.toml, *.wasm, state/darkirc)"
    if [[ -d "$STATE_DIR/xmpp" ]]; then
      note "  includes OMEMO store state/xmpp"
    else
      note "  (no OMEMO store present yet)"
    fi
  else
    # Exclude: sockets; the host-specific config.toml (the new host's add-tenant
    # writes the correct external-worker ports); *.wasm (rebuilt by install-wasm
    # — carrying stale v1.1.0 artifacts would linger past the never-cleaning overlay);
    # and the complete DarkIRC subtree. DarkIRC contacts/private keys and its
    # transactional state have a separate structured migration contract.
    tar czf "$WORK/state.tar.gz" -C "$LWROOT" \
      --exclude='*.sock' --exclude='state/config.toml' \
      --exclude='state/tools/*.wasm' --exclude='state/channels/*.wasm' \
      --exclude='state/darkirc' --exclude='state/darkirc/**' state
    say "  state.tar.gz: $(du -h "$WORK/state.tar.gz" | cut -f1)$( [[ -d "$STATE_DIR/xmpp" ]] && echo ' (incl. OMEMO store)' )"
  fi
else
  note "no state dir at $STATE_DIR — nothing to bundle (OMEMO/workspace start fresh on import)"
fi

# ---- 5. structured DarkIRC contact migration -------------------------------
DARKIRC_MIGRATION_INCLUDED=false
if darkirc_manifest_required; then
  banner "5/6  DarkIRC contact-only manifest"
  if $DRY_RUN; then
    note "[dry-run] would: $MT darkirc-contact export-migration $TENANT"
  else
    "$MT" darkirc-contact export-migration "$TENANT" >/dev/null \
      || die "DarkIRC contact-only migration export failed; no generic archive was sealed"
    # A legacy tenant may have a DarkIRC state tree but no enablement flag yet.
    # Once its settled contacts are exported through the structured path, carry
    # the explicit enablement/scope metadata so the destination provisions the
    # same protected contact contract instead of treating it as generic state.
    DARKIRC_ENABLED=true
    DARKIRC_SCOPE_ID="$(jq -r ".tenants[\"$TENANT\"].darkirc_scope_id // empty" "$PORTS_REGISTRY" 2>/dev/null || true)"
    [[ "$DARKIRC_SCOPE_ID" =~ ^[0-9a-f]{32}$ && ! "$DARKIRC_SCOPE_ID" =~ ^0+$ ]] \
      || die "DarkIRC migration did not produce a valid tenant scope ID"
    DARKIRC_MANIFEST_PATH="$DARKIRC_DIR/$DARKIRC_MANIFEST_REL"
    [[ -f "$DARKIRC_MANIFEST_PATH" && ! -L "$DARKIRC_MANIFEST_PATH" ]] \
      || die "DarkIRC migration helper did not create $DARKIRC_MANIFEST_PATH"
    manifest_owner="$(stat -c '%u' "$DARKIRC_MANIFEST_PATH" 2>/dev/null || true)"
    manifest_mode="$(stat -c '%a' "$DARKIRC_MANIFEST_PATH" 2>/dev/null || true)"
    manifest_links="$(stat -c '%h' "$DARKIRC_MANIFEST_PATH" 2>/dev/null || true)"
    [[ "$manifest_owner" == "$(id -u "$TENANT")" \
       && "$manifest_mode" == 600 && "$manifest_links" == 1 ]] \
      || die "DarkIRC migration manifest must be tenant-owned, single-link mode 0600"
    DARKIRC_PIN_DIR="$(mktemp -d "$OUT_DIR/.${TENANT}-darkirc-manifest.XXXXXX")"
    chmod 0700 "$DARKIRC_PIN_DIR"
    DARKIRC_PINNED_MANIFEST="$DARKIRC_PIN_DIR/$DARKIRC_MANIFEST_NAME"
    ( umask 077; cp --no-dereference -- "$DARKIRC_MANIFEST_PATH" "$DARKIRC_PINNED_MANIFEST" ) \
      || die "failed to pin DarkIRC migration manifest"
    [[ -f "$DARKIRC_PINNED_MANIFEST" && ! -L "$DARKIRC_PINNED_MANIFEST" ]] \
      || die "DarkIRC migration manifest changed type while being pinned"
    chmod 0600 "$DARKIRC_PINNED_MANIFEST"
    pinned_links="$(stat -c '%h' "$DARKIRC_PINNED_MANIFEST" 2>/dev/null || true)"
    [[ "$pinned_links" == 1 ]] \
      || die "pinned DarkIRC migration manifest must have one link"
    "$MT" darkirc-contact validate-migration --in - --json \
      <"$DARKIRC_PINNED_MANIFEST" >/dev/null \
      || die "pinned DarkIRC migration manifest failed typed validation"
    DARKIRC_MIGRATION_INCLUDED=true
    say "  $DARKIRC_MANIFEST_NAME: structured contact-only migration (0600)"
  fi
fi

# ---- 6. seal the bundle ------------------------------------------------------
banner "6/6  Seal bundle"
BUNDLE="$OUT_DIR/${TENANT}-migrate-${STAMP}.tar"
if $DRY_RUN; then
  note "[dry-run] would write meta.txt and seal -> $BUNDLE (0600)"
  say ""; say "DRY RUN complete — no bundle written, services NOT stopped."
  exit 0
fi
validate_export_output_dir
{
  printf 'tenant=%s\n' "$TENANT"
  printf 'source_version=%s\n' "$SRC_VER"
  printf 'source_runtime=%s\n' "$RUNTIME"
  printf 'pg_role=%s\n' "$PG_ROLE"
  printf 'pg_db=%s\n' "$PG_DB"
  printf 'db_backend=%s\n' "$DB_BACKEND"
  printf 'darkirc_enabled=%s\n' "$DARKIRC_ENABLED"
  printf 'darkirc_scope_id=%s\n' "$DARKIRC_SCOPE_ID"
  printf 'darkirc_migration=%s\n' "$DARKIRC_MIGRATION_INCLUDED"
  printf 'source_quiesced=true\n'
  printf 'created=%s\n' "$STAMP"
} > "$WORK/meta.txt"
( umask 077; tar cf "$BUNDLE" -C "$WORK" . )
if $DARKIRC_MIGRATION_INCLUDED; then
  # Append from the root-owned pinned directory. The private contact manifest
  # never passes through /tmp or the generic work archive, and the pinned copy
  # was revalidated after leaving the tenant-controlled path.
  tar --append --file "$BUNDLE" \
    -C "$DARKIRC_PIN_DIR" "$DARKIRC_MANIFEST_NAME" \
    || die "failed to append DarkIRC contact manifest to bundle"
  rm -rf "$DARKIRC_PIN_DIR"
  DARKIRC_PIN_DIR=""
fi
chmod 0600 "$BUNDLE"

banner "Done — bundle written (tenant is STOPPED — cutover in progress)"
say "  $BUNDLE ($(du -h "$BUNDLE" | cut -f1))"
say ""
say "The '$TENANT' writers are now STOPPED on this host (PG still up for the dump;"
say "you may stop it too). The bundle contains SECRETS_MASTER_KEY + the XMPP"
say "password — transfer it over ssh (0600), and delete it from both hosts after verify."
if $DARKIRC_MIGRATION_INCLUDED; then
  say "DarkIRC contacts are carried only in the structured $DARKIRC_MANIFEST_NAME manifest."
fi
say ""
say "Next, on the NEW host:  sudo ic/scripts/import-tenant.sh $(basename "$BUNDLE") --start"
say "Rollback: this host is intact — restart '$TENANT' here to abort the migration."
