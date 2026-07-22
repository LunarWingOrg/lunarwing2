#!/usr/bin/env bash
#
# import-tenant.sh — recreate a tenant on a FRESH LunarWing host from a bundle made
# by export-tenant.sh (see docs/ops/MT-MACHINE-MIGRATION.md). MACHINE MIGRATION.
#
# Runs on the NEW (target) host with a v1.1.4-class mt-admin. Init-system agnostic:
# all init/runtime-specific work is delegated to lunarwing-mt-admin.sh, which
# auto-detects systemd vs OpenRC and rootless-podman vs rootful-docker. The new
# host's init system need not match the old host's.
#
# Flow:
#   add-tenant --no-health (fresh: clone, ports, THROWAWAY secrets, empty PG, units;
#     NO daemon) -> build-tenant + workers -> INJECT carried secrets/config (incl.
#     SECRETS_MASTER_KEY — without it the restored DB's encrypted secrets are dead;
#     verified verbatim before restore) -> restore-tenant (DB) -> verify/rekey
#     owner scope -> restore state dir (OMEMO/workspace) -> install-wasm
#     (fresh v1.1.4 artifacts) -> [--start] start.
#
# Intra-host tokens (gateway/bridge/webhook/relay) are NOT carried — add-tenant minted
# fresh, self-consistent ones (so config.toml's worker auth matches the gateway token).
# Gateway-UI / external-webhook clients re-authenticate after cutover.
#
# CUTOVER: the new daemon uses the SAME XMPP JID as the old one; two simultaneous
# logins conflict. Import STAGES without starting by default. Starting requires you
# to confirm the old side is stopped (export already stops it) — this confirmation is
# NOT satisfied by --yes alone; pass --old-stopped for an unattended start.
#
# Usage (run as root on the new host):
#   sudo ic/scripts/import-tenant.sh <bundle.7z|bundle.tar> [--name <t>] [--start] [--old-stopped]
#        [--with-nanocode] [--with-pebble] [--with-opencode] [--with-toolchains]
#        [--with-vision] [--docker-group]
#        [--owner-scope <old_scope>] [--dry-run] [--yes] [--force]
# Encrypted bundles read their passphrase from KAWARIMI_PASS_FD, KAWARIMI_PASS,
# KAWARIMI_PASS_FILE, or an interactive prompt, in that order. The passphrase is
# fed to 7z over stdin and never placed in command argv.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
DARKIRC_MANIFEST_NAME="darkirc-contacts-v1.json"
DARKIRC_SCOPE_RE='^[0-9a-f]{32}$'
BUNDLE_HAS_DARKIRC_MANIFEST=false
BUNDLE_DARKIRC_MANIFEST_MEMBER=""

# These are the only values the exporter is allowed to carry into a live
# tenant environment.  Import treats the bundle as untrusted input: an
# arbitrary KEY=value line must never become a service setting.
IMPORT_LUNARWING_KEYS=(
  SECRETS_MASTER_KEY XMPP_JID XMPP_PASSWORD XMPP_DM_POLICY XMPP_ALLOW_FROM
  XMPP_ALLOW_ROOMS XMPP_ENCRYPTED_ROOMS XMPP_ALLOW_PLAINTEXT_FALLBACK
  XMPP_OMEMO_DEVICE_ID LLM_API_KEY LLM_MODEL OPENCODE_MODEL GOTIFY_URL
  GATEWAY_HOST HTTP_HOST LLM_BASE_URL OPENCODE_BASE_URL
)
IMPORT_BRIDGE_KEYS=(
  XMPP_JID XMPP_PASSWORD XMPP_DM_POLICY XMPP_ALLOW_FROM_JSON
  XMPP_ALLOW_ROOMS_JSON XMPP_ENCRYPTED_ROOMS_JSON XMPP_DEVICE_ID
  XMPP_ALLOW_PLAINTEXT_FALLBACK
)
IMPORT_VISION_KEYS=(VL_URL VL_MODEL LUNARWING_AUTH_TOKEN)
INJECTED_MASTER_KEY_VERIFIED=false

BUNDLE=""
NAME_OVERRIDE=""
DO_START=false
OLD_STOPPED=false
WITH_DOCKER_GROUP=false
WITH_NANOCODE=false
WITH_PEBBLE=false
WITH_OPENCODE=false
WITH_TOOLCHAINS=false
WITH_VISION=false
OWNER_SCOPE=""
DRY_RUN=false
AUTO_YES=false
FORCE=false
MAX_BUNDLE_BYTES="${KAWARIMI_MAX_BUNDLE_BYTES:-107374182400}"
MAX_STATE_BYTES="${KAWARIMI_MAX_STATE_BYTES:-107374182400}"
MAX_STATE_ENTRIES="${KAWARIMI_MAX_STATE_ENTRIES:-1000000}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --name)          NAME_OVERRIDE="$2"; shift 2 ;;
    --start)         DO_START=true; shift ;;
    --old-stopped)   OLD_STOPPED=true; shift ;;
    --docker-group)  WITH_DOCKER_GROUP=true; shift ;;
    --with-nanocode) WITH_NANOCODE=true; shift ;;
    --with-pebble)   WITH_PEBBLE=true; shift ;;
    --with-opencode)   WITH_OPENCODE=true; shift ;;
    --with-toolchains) WITH_TOOLCHAINS=true; shift ;;
    --with-vision)     WITH_VISION=true; shift ;;
    --owner-scope)      OWNER_SCOPE="$2"; shift 2 ;;
    --dry-run)       DRY_RUN=true; shift ;;
    --yes|-y)        AUTO_YES=true; shift ;;
    --force)         FORCE=true; shift ;;
    -*)              printf 'unknown arg: %s\n' "$1" >&2; exit 2 ;;
    *)               BUNDLE="$1"; shift ;;
  esac
done

say()    { printf '%s\n' "$*"; }
die()    { printf 'error: %s\n' "$*" >&2; exit 1; }
banner() { printf '\n========== %s ==========\n' "$*"; }
note()   { printf '  · %s\n' "$*"; }
confirm() { $AUTO_YES && return 0; local a; read -r -p "$1 [y/N] " a; [[ "$a" == y || "$a" == Y ]]; }
run()    { if $DRY_RUN; then printf '  [dry-run] %s\n' "$*"; return 0; fi; printf '  + %s\n' "$*"; "$@"; }

run_bundle_manifest_stdin() {  # <command> [args...]
  if $DRY_RUN; then
    printf '  [dry-run] %s < %s\n' "$*" "$DARKIRC_MANIFEST_NAME"
    return 0
  fi
  printf '  + %s < %s\n' "$*" "$DARKIRC_MANIFEST_NAME"
  tar -xOf "$BUNDLE" "$BUNDLE_DARKIRC_MANIFEST_MEMBER" | "$@"
}

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

validate_dotenv_manifest() {  # <manifest> <allowed-key> ...
  local manifest="$1" manifest_name line key
  shift
  manifest_name="$(basename -- "$manifest")"
  local -A allowed=() seen=()
  for key in "$@"; do
    allowed["$key"]=1
  done
  [[ -f "$manifest" && ! -L "$manifest" ]] \
    || die "manifest is missing or not a regular file: $manifest"
  while IFS= read -r line || [[ -n "$line" ]]; do
    line="${line%$'\r'}"
    case "$line" in
      ''|'#'*) continue ;;
    esac
    [[ "$line" =~ ^([A-Za-z_][A-Za-z0-9_]*)=.*$ ]] \
      || die "malformed dotenv line in $manifest_name"
    key="${BASH_REMATCH[1]}"
    [[ -n "${allowed[$key]+present}" ]] \
      || die "unsupported dotenv key '$key' in $manifest_name"
    [[ -z "${seen[$key]+present}" ]] \
      || die "duplicate dotenv key '$key' in $manifest_name"
    seen["$key"]=1
  done <"$manifest"
}

validate_import_target_roots() {
  local uid path owner mode links
  uid="$(id -u "$TENANT" 2>/dev/null || true)"
  [[ "$uid" =~ ^[0-9]+$ ]] || die "could not resolve target tenant UID"
  for path in "$HOME_T" "$LWROOT" "$LWROOT/env" "$LWROOT/state"; do
    path_components_no_symlink "$path" \
      || die "unsafe target path component (symlink or missing directory): $path"
    [[ -d "$path" && ! -L "$path" ]] \
      || die "target path is not a regular directory: $path"
    owner="$(stat -c '%u' "$path" 2>/dev/null || true)"
    mode="$(stat -c '%a' "$path" 2>/dev/null || true)"
    [[ "$owner" == "$uid" && "$mode" =~ ^[0-7]+$ ]] \
      || die "target directory has unexpected ownership: $path"
    (( (8#$mode & 022) == 0 )) \
      || die "target directory is group/world writable: $path"
  done
  for path in "$ENVF" "$BRIDGE_ENVF"; do
    [[ -f "$path" && ! -L "$path" ]] \
      || die "target environment file is missing or unsafe: $path"
    path_components_no_symlink "$path" \
      || die "unsafe target environment path: $path"
    owner="$(stat -c '%u' "$path" 2>/dev/null || true)"
    mode="$(stat -c '%a' "$path" 2>/dev/null || true)"
    links="$(stat -c '%h' "$path" 2>/dev/null || true)"
    [[ "$owner" == "$uid" && "$links" == 1 && "$mode" =~ ^[0-7]+$ ]] \
      || die "target environment file has unexpected ownership or link count: $path"
    (( (8#$mode & 077) == 0 && (8#$mode & 0400) != 0 )) \
      || die "target environment file has unsafe permissions: $path"
  done
  if [[ -e "$VISION_ENVF" ]]; then
    [[ -f "$VISION_ENVF" && ! -L "$VISION_ENVF" ]] \
      || die "target vision environment file is unsafe: $VISION_ENVF"
    path_components_no_symlink "$VISION_ENVF" \
      || die "unsafe target vision environment path: $VISION_ENVF"
  fi
}

validate_bundle_members() {
  local list_file="$1" member normalized first
  tar -tf "$BUNDLE" >"$list_file" \
    || die "failed to inspect bundle archive"
  declare -A seen=()
  while IFS= read -r member || [[ -n "$member" ]]; do
    [[ -n "$member" ]] || continue
    normalized="$member"
    while [[ "$normalized" == ./* ]]; do
      normalized="${normalized#./}"
    done
    while [[ "$normalized" == */ ]]; do
      normalized="${normalized%/}"
    done
    [[ -n "$normalized" ]] || continue
    [[ "$normalized" != /* && "$normalized" != ../* && "$normalized" != */../* \
       && "$normalized" != '..' && "$normalized" != *'//'* \
       && "$normalized" != */./* && "$normalized" != */. ]] \
      || die "bundle contains unsafe path '$member'"
    case "$normalized" in
      "$DARKIRC_MANIFEST_NAME")
        BUNDLE_HAS_DARKIRC_MANIFEST=true
        BUNDLE_DARKIRC_MANIFEST_MEMBER="$member"
        ;;
      state/darkirc|state/darkirc/*)
        die "bundle contains secret-bearing state/darkirc; use $DARKIRC_MANIFEST_NAME"
        ;;
    esac
    [[ -z "${seen[$normalized]+present}" ]] \
      || die "bundle contains duplicate path '$member'"
    seen["$normalized"]=1
  done <"$list_file"

  # Generic migration bundles may contain only ordinary files/directories.
  # Links, devices, FIFOs, and sockets are never meaningful migration payloads
  # and can redirect or block privileged extraction.
  while IFS= read -r first; do
    case "$first" in
      -*|d*) ;;
      *) die "bundle contains a non-regular archive entry; refusing extraction" ;;
    esac
  done < <(tar -tvf "$BUNDLE" 2>/dev/null)
}

validate_state_archive() {  # <state.tar.gz> <list-file>
  local archive="$1" list_file="$2" member normalized first
  tar -tzf "$archive" >"$list_file" \
    || die "bundle contains an unreadable state.tar.gz"
  declare -A seen=()
  while IFS= read -r member || [[ -n "$member" ]]; do
    [[ -n "$member" ]] || continue
    normalized="$member"
    while [[ "$normalized" == ./* ]]; do
      normalized="${normalized#./}"
    done
    while [[ "$normalized" == */ ]]; do
      normalized="${normalized%/}"
    done
    [[ -n "$normalized" ]] || continue
    [[ "$normalized" != /* && "$normalized" != ../* && "$normalized" != */../* \
       && "$normalized" != '..' && "$normalized" != *'//'* \
       && "$normalized" != */./* && "$normalized" != */. ]] \
      || die "state archive contains unsafe path '$member'"
    case "$normalized" in
      state|state/*) ;;
      *) die "state archive member is outside state/: '$member'" ;;
    esac
    case "$normalized" in
      state/darkirc|state/darkirc/*)
        die "generic state archive contains secret-bearing state/darkirc"
        ;;
    esac
    [[ -z "${seen[$normalized]+present}" ]] \
      || die "state archive contains duplicate path '$member'"
    seen["$normalized"]=1
  done <"$list_file"
  while IFS= read -r first; do
    case "$first" in
      -*|d*) ;;
      *) die "state archive contains a non-regular entry; refusing extraction" ;;
    esac
  done < <(tar -tvzf "$archive" 2>/dev/null)
}

valid_scope_id() {
  local value="$1"
  [[ "$value" =~ $DARKIRC_SCOPE_RE && ! "$value" =~ ^0+$ ]]
}

# Inject KEY=value lines from a manifest through a tenant-side atomic merge. Values
# stay on stdin/a same-directory temporary file; they are never placed in argv or
# an environment variable. The tenant child preserves the live file's mode/owner.
inject_keys() {  # <manifest> <live_env> <allowed-key> ...
  local man="$1" live="$2" verify_key=""
  shift 2
  validate_dotenv_manifest "$man" "$@"
  path_components_no_symlink "$live" \
    || die "unsafe target environment path: $live"
  [[ -f "$live" && ! -L "$live" ]] \
    || die "target environment file is missing or unsafe: $live"

  # The manifest is secret-bearing, so feed it over stdin to a tenant-owned
  # merge. The child never receives secret values in argv/environment and the
  # root wrapper never reopens the mutable tenant pathname after the merge.
  [[ "$live" == "$ENVF" ]] && verify_key="SECRETS_MASTER_KEY"
  cat "$man" | sudo -u "$TENANT" env \
    TARGET_PATH="$live" TARGET_VERIFY_KEY="$verify_key" bash -c '
    set -euo pipefail
    target="$TARGET_PATH"
    parent="${target%/*}"
    base="${target##*/}"
    uid="$(id -u)"
    [[ -d "$parent" && ! -L "$parent" ]] || exit 73
    parent_owner="$(stat -c %u "$parent" 2>/dev/null || printf -1)"
    parent_mode="$(stat -c %a "$parent" 2>/dev/null || true)"
    [[ "$parent_owner" == "$uid" && "$parent_mode" =~ ^[0-7]+$ ]] || exit 73
    (( (8#$parent_mode & 077) == 0 )) || exit 73
    [[ -f "$target" && ! -L "$target" ]] || exit 73
    owner="$(stat -c %u "$target" 2>/dev/null || printf -1)"
    mode="$(stat -c %a "$target" 2>/dev/null || true)"
    links="$(stat -c %h "$target" 2>/dev/null || printf 0)"
    [[ "$owner" == "$uid" && "$links" == 1 && "$mode" =~ ^[0-7]+$ ]] || exit 73
    (( (8#$mode & 077) == 0 && (8#$mode & 0400) != 0 )) || exit 73

    umask 077
    patch="$(mktemp "$parent/.${base}.import.XXXXXX")"
    tmp="$(mktemp "$parent/.${base}.next.XXXXXX")"
    trap '\''rm -f -- "$patch" "$tmp"'\'' EXIT
    cat >"$patch"
    if ! awk -v patch="$patch" '\''
      BEGIN {
        while ((getline line < patch) > 0) {
          sub(/\r$/, "", line)
          if (line == "" || line ~ /^#/) continue
          if (line !~ /^[A-Za-z_][A-Za-z0-9_]*=.*$/) exit 74
          key = line
          sub(/=.*/, "", key)
          if (key in replacement) exit 74
          replacement[key] = line
          order[++count] = key
        }
        close(patch)
      }
      {
        key = $0
        if (key ~ /^[A-Za-z_][A-Za-z0-9_]*=/) {
          sub(/=.*/, "", key)
          if (key in replacement) {
            if (seen[key]++) exit 74
            print replacement[key]
            next
          }
        }
        print
      }
      END {
        if (count < 0) exit 74
        for (i = 1; i <= count; i++) {
          key = order[i]
          if (!(key in seen)) print replacement[key]
        }
      }
    '\'' "$target" >"$tmp"; then
      rm -f -- "$patch" "$tmp"
      trap - EXIT
      exit 74
    fi
    chmod "$mode" "$tmp" || {
      rm -f -- "$patch" "$tmp"
      trap - EXIT
      exit 74
    }
    if [[ -n "${TARGET_VERIFY_KEY:-}" ]]; then
      if ! awk -v key="${TARGET_VERIFY_KEY}=" '\''
        FNR == NR {
          if (index($0, key) == 1) expected = $0
          next
        }
        {
          if (index($0, key) == 1) actual = $0
        }
        END {
          if (expected == "" || actual != expected) exit 74
        }
      '\'' "$patch" "$tmp"; then
        rm -f -- "$patch" "$tmp"
        trap - EXIT
        exit 74
      fi
    fi
    mv -f -- "$tmp" "$target"
    rm -f -- "$patch"
    trap - EXIT
    exit 0
  ' >/dev/null || die "could not safely inject manifest into $live"
  [[ "$live" == "$ENVF" ]] && INJECTED_MASTER_KEY_VERIFIED=true
  note "validated and atomically injected manifest keys into $live"
}

non_target_owner_scopes() {  # <scope-summary> <target-scope>
  local summary="$1" target="$2" line scope _count
  while IFS=$' \t' read -r scope _count _; do
    [[ -n "$scope" ]] || continue
    [[ "$scope" == "$target" ]] && continue
    printf '%s\n' "$scope"
  done <<< "$summary" | sort -u
}

show_owner_scope_summary() {  # <summary>
  local summary="$1"
  if [[ -z "$summary" ]]; then
    note "no owner-scoped rows detected"
    return 0
  fi
  say "  owner scopes:"
  printf '%s\n' "$summary" | sed 's/^/    /'
}

reconcile_owner_scope() {
  banner "5/8  Owner scope"

  if $DRY_RUN; then
    if [[ -n "$OWNER_SCOPE" ]]; then
      note "[dry-run] would: $MT migrate-owner-scope $TENANT --from $OWNER_SCOPE"
    else
      note "[dry-run] would inspect restored owner scopes and rekey default -> $TENANT if needed"
    fi
    return 0
  fi

  local summary
  summary="$("$MT" owner-scopes "$TENANT")" \
    || die "failed to inspect owner scopes after restore"
  show_owner_scope_summary "$summary"
  [[ -n "$summary" ]] || return 0

  local -a non_targets
  mapfile -t non_targets < <(non_target_owner_scopes "$summary" "$TENANT")
  if [[ "${#non_targets[@]}" -eq 0 ]]; then
    note "owner-scoped rows already use '$TENANT'"
    return 0
  fi
  if [[ "${#non_targets[@]}" -gt 1 ]]; then
    die "multiple non-target owner scopes detected: ${non_targets[*]} — inspect the DB and re-run with a clean bundle"
  fi

  local source_scope="${non_targets[0]}"
  if [[ -n "$OWNER_SCOPE" && "$OWNER_SCOPE" != "$source_scope" ]]; then
    die "--owner-scope '$OWNER_SCOPE' was requested, but restored data has '$source_scope'"
  fi
  if [[ "$source_scope" != "default" && -z "$OWNER_SCOPE" ]]; then
    die "non-target owner scope '$source_scope' detected; re-run with --owner-scope '$source_scope' to rekey it explicitly"
  fi

  run "$MT" migrate-owner-scope "$TENANT" --from "$source_scope"

  local verify_summary
  verify_summary="$("$MT" owner-scopes "$TENANT")" \
    || die "failed to verify owner scopes after rekey"
  local -a remaining
  mapfile -t remaining < <(non_target_owner_scopes "$verify_summary" "$TENANT")
  [[ "${#remaining[@]}" -eq 0 ]] \
    || die "owner-scope rekey incomplete; remaining non-target scopes: ${remaining[*]}"
  note "owner-scope continuity verified for '$TENANT'"
}

[[ -n "$BUNDLE" ]] || die "usage: $0 <bundle.7z|bundle.tar> [--name <t>] [--start] [--old-stopped] [--with-nanocode] [--with-pebble] [--with-opencode] [--with-toolchains] [--with-vision] [--docker-group] [--owner-scope <old_scope>] [--dry-run] [--yes] [--force]"
[[ -f "$BUNDLE" ]] || die "bundle not found: $BUNDLE"
[[ ! -L "$BUNDLE" ]] || die "bundle path must not be a symlink"
[[ "$(id -u)" -eq 0 ]] || die "run as root (sudo) — mt-admin needs root"
command -v jq  >/dev/null 2>&1 || die "jq required"
command -v tar >/dev/null 2>&1 || die "tar required"

MT="${LUNARWING_MT_ADMIN:-$SCRIPT_DIR/lunarwing-mt-admin.sh}"
WEECHAT_PREFLIGHT="$SCRIPT_DIR/lunarwing-weechat-preflight.sh"
PORTS_REGISTRY="${LUNARWING_PORTS_REGISTRY:-/etc/lunarwing/ports.json}"
[[ -x "$MT" ]] || die "mt-admin not found/executable at $MT"
[[ -x "$WEECHAT_PREFLIGHT" ]] || die "WeeChat preflight not found/executable at $WEECHAT_PREFLIGHT"
grep -qE '^\s*restore-tenant)' "$MT" || die "mt-admin at $MT predates restore-tenant (need a v1.1.4-class host)"
grep -qE '^\s*owner-scopes)' "$MT" || die "mt-admin at $MT predates owner-scopes (need current Kawarimi owner-scope checks)"

WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
chmod 0700 "$WORK"
# Pin the caller-supplied credential bundle before any validation.  Validation
# and every later extraction then operate on the same private copy, so a
# writable source directory cannot swap archive contents between the checks.
BUNDLE_SOURCE="$BUNDLE"
bundle_links="$(stat -c '%h' "$BUNDLE_SOURCE" 2>/dev/null || true)"
[[ "$bundle_links" == 1 ]] || die "bundle must be a single-link regular file"
BUNDLE="$WORK/input.bundle"
( umask 077; cp --no-dereference -- "$BUNDLE_SOURCE" "$BUNDLE" ) \
  || die "could not pin bundle contents for validation"
[[ -f "$BUNDLE" && ! -L "$BUNDLE" ]] \
  || die "bundle source changed to a non-regular file while being pinned"
chmod 0600 "$BUNDLE"
validate_bundle_members "$WORK/archive.list"
tar xf "$BUNDLE" -C "$WORK" --no-same-owner --no-same-permissions \
  --exclude="$DARKIRC_MANIFEST_NAME" --exclude="./$DARKIRC_MANIFEST_NAME" \
  || die "failed to unpack bundle $BUNDLE"
rm -f "$WORK/archive.list"
[[ -f "$WORK/meta.txt" ]] || die "bundle missing meta.txt — not an export-tenant.sh bundle?"
[[ ! -L "$WORK/meta.txt" ]] || die "bundle meta.txt is a symlink"
validate_dotenv_manifest "$WORK/manifest-lunarwing.env" "${IMPORT_LUNARWING_KEYS[@]}"
if [[ -f "$WORK/manifest-bridge.env" ]]; then
  validate_dotenv_manifest "$WORK/manifest-bridge.env" "${IMPORT_BRIDGE_KEYS[@]}"
fi
if [[ -f "$WORK/manifest-vision.env" ]]; then
  validate_dotenv_manifest "$WORK/manifest-vision.env" "${IMPORT_VISION_KEYS[@]}"
fi
if [[ -f "$WORK/state.tar.gz" ]]; then
  [[ ! -L "$WORK/state.tar.gz" ]] || die "state.tar.gz is a symlink"
  validate_state_archive "$WORK/state.tar.gz" "$WORK/state-archive.list"
  rm -f "$WORK/state-archive.list"
fi

meta() { sed -n "s/^$1=//p" "$WORK/meta.txt" | head -1; }
manifest_value() {
  local key="$1" file="${2:-$WORK/manifest-lunarwing.env}" value
  value="$(sed -n "s/^${key}=//p" "$file" 2>/dev/null | head -1)"
  value="${value%$'\r'}"
  if [[ ${#value} -ge 2 && ( "$value" == \"*\" || "$value" == \'*\' ) ]]; then
    value="${value:1:${#value}-2}"
  fi
  printf '%s' "$value"
}
SOURCE_TENANT="$(meta tenant)"
[[ "$SOURCE_TENANT" =~ ^[a-z0-9][a-z0-9-]{0,63}$ ]] \
  || die "bundle has an unsafe source tenant name '$SOURCE_TENANT'"
RAW_NAME="${NAME_OVERRIDE:-$SOURCE_TENANT}"
[[ "$RAW_NAME" =~ ^[a-z0-9][a-z0-9-]{0,63}$ ]] \
  || die "unsafe target tenant name '$RAW_NAME' (use lowercase letters, digits, and hyphens)"
TENANT="$RAW_NAME"
SRC_VER="$(meta source_version)"; DB_BACKEND="$(meta db_backend)"; DB_BACKEND="${DB_BACKEND:-postgres}"

DARKIRC_ENABLED_META="$(meta darkirc_enabled)"
DARKIRC_ENABLED="$DARKIRC_ENABLED_META"
DARKIRC_SCOPE_ID="$(meta darkirc_scope_id)"
DARKIRC_MIGRATION_META="$(meta darkirc_migration)"
DARKIRC_MIGRATION="$DARKIRC_MIGRATION_META"
SOURCE_QUIESCED="$(meta source_quiesced)"
SOURCE_QUIESCED="${SOURCE_QUIESCED:-false}"
HAS_DARKIRC_MANIFEST="$BUNDLE_HAS_DARKIRC_MANIFEST"

# Older structured bundles may predate the explicit metadata lines. Presence of
# the fixed manifest is enough to opt into the strict DarkIRC path; explicit
# false metadata remains an integrity error below.
if $HAS_DARKIRC_MANIFEST; then
  [[ -n "$DARKIRC_ENABLED" ]] || DARKIRC_ENABLED=true
  [[ -n "$DARKIRC_MIGRATION" ]] || DARKIRC_MIGRATION=true
fi
DARKIRC_ENABLED="${DARKIRC_ENABLED:-false}"
DARKIRC_MIGRATION="${DARKIRC_MIGRATION:-false}"

[[ "$DARKIRC_ENABLED" == true || "$DARKIRC_ENABLED" == false ]] \
  || die "bundle has invalid darkirc_enabled metadata"
[[ "$DARKIRC_MIGRATION" == true || "$DARKIRC_MIGRATION" == false ]] \
  || die "bundle has invalid darkirc_migration metadata"
if $DARKIRC_MIGRATION && ! $HAS_DARKIRC_MANIFEST; then
  die "bundle declares DarkIRC contact migration but lacks $DARKIRC_MANIFEST_NAME"
fi
if $HAS_DARKIRC_MANIFEST; then
  $DARKIRC_ENABLED \
    || die "$DARKIRC_MANIFEST_NAME is present but DarkIRC is not enabled in bundle metadata"
  $DARKIRC_MIGRATION \
    || die "$DARKIRC_MANIFEST_NAME is present without darkirc_migration=true metadata"
  manifest_listing="$(tar -tvf "$BUNDLE" 2>/dev/null | awk -v name="$BUNDLE_DARKIRC_MANIFEST_MEMBER" '$NF == name { print; exit }')"
  [[ "$manifest_listing" == -rw-------* ]] \
    || die "$DARKIRC_MANIFEST_NAME must be a mode-0600 regular file"
fi
if $DARKIRC_ENABLED; then
  valid_scope_id "$DARKIRC_SCOPE_ID" \
    || die "bundle has no valid DarkIRC scope ID"
  [[ "$TENANT" == "$SOURCE_TENANT" ]] \
    || die "refusing to clone DarkIRC keys from '$SOURCE_TENANT' into '$TENANT'; verified rotation is required"
  [[ "$SOURCE_QUIESCED" == true ]] \
    || die "DarkIRC scope may be preserved only from a quiesced source export"
  $OLD_STOPPED \
    || die "DarkIRC scope preservation requires --old-stopped before target provisioning"
  $FORCE && die "--force is not allowed for a DarkIRC migration; use a fresh target"

  grep -qE 'darkirc-scope-id' "$MT" \
    || die "target mt-admin lacks --darkirc-scope-id; refusing a late scope mismatch"
  if $HAS_DARKIRC_MANIFEST; then
    grep -qE 'stage-migration' "$MT" \
      || die "target mt-admin lacks structured DarkIRC migration support"
    grep -qE 'import-migration' "$MT" \
      || die "target mt-admin lacks structured DarkIRC import support"
    # The destination binary is not installed until after tenant provisioning;
    # this preflight validates the manifest's frozen profile/key codec and
    # checksums without accepting it into tenant state.  mt-admin revalidates
    # the measured target binary immediately before stage/import.
    if $DRY_RUN; then
      note "[dry-run] would typed-validate $DARKIRC_MANIFEST_NAME without staging it"
    else
      manifest_validation="$(tar -xOf "$BUNDLE" "$BUNDLE_DARKIRC_MANIFEST_MEMBER" | \
        "$MT" darkirc-contact validate-migration --in - --json --unbound)" \
        || die "$DARKIRC_MANIFEST_NAME failed typed validation"
      validated_scope="$(jq -r '.scope_id // empty' <<<"$manifest_validation")"
      [[ "$validated_scope" == "$DARKIRC_SCOPE_ID" ]] \
        || die "$DARKIRC_MANIFEST_NAME scope does not match bundle metadata"
    fi
  fi

  if [[ -f "$PORTS_REGISTRY" ]]; then
    jq -e '.tenants | type == "object"' "$PORTS_REGISTRY" >/dev/null 2>&1 \
      || die "ports registry '$PORTS_REGISTRY' is invalid; refusing DarkIRC scope import"
    SCOPE_OWNER="$(jq -r --arg scope "$DARKIRC_SCOPE_ID" \
      '.tenants // {} | to_entries[] | select(.value.darkirc_scope_id == $scope) | .key' \
      "$PORTS_REGISTRY" 2>/dev/null | head -1)"
    [[ -z "$SCOPE_OWNER" ]] \
      || die "DarkIRC scope ID is already registered to '$SCOPE_OWNER'; refusing clone/import"
  else
    # A pristine destination has no registry yet. add-tenant will initialize it
    # and atomically reserve the validated source scope before any contact state
    # is staged.
    SCOPE_OWNER=""
  fi
fi

if [[ -n "$OWNER_SCOPE" ]]; then
  [[ "$OWNER_SCOPE" =~ ^[A-Za-z0-9_.:@-]+$ ]] \
    || die "unsafe --owner-scope value '$OWNER_SCOPE'"
  [[ "$OWNER_SCOPE" != "$TENANT" ]] \
    || die "--owner-scope must not equal target tenant '$TENANT'"
fi

banner "Import tenant '$TENANT' (from source $SRC_VER, db $DB_BACKEND)"
$DRY_RUN && say "*** DRY RUN — no changes will be made ***"

# ---- preconditions ----
[[ "$DB_BACKEND" == "postgres" ]] || die "bundle db_backend=$DB_BACKEND: only postgres is supported"
[[ -f "$WORK/db.dump" && ! -L "$WORK/db.dump" ]] || die "bundle missing safe db.dump"
SOURCE_MASTER_KEY="$(manifest_value SECRETS_MASTER_KEY)"
[[ "$SOURCE_MASTER_KEY" =~ ^[0-9a-fA-F]{64}$ ]] \
  || die "bundle has a missing or invalid SECRETS_MASTER_KEY — the restored DB's encrypted secrets would be unrecoverable; abort"
if jq -e ".tenants[\"$TENANT\"]" "$PORTS_REGISTRY" >/dev/null 2>&1; then
  $FORCE || die "tenant '$TENANT' already exists in $PORTS_REGISTRY — refusing (use --force only to re-import over it)"
  say "WARNING: tenant '$TENANT' already exists — proceeding due to --force"
fi
XMPP_JID="$(sed -n 's/^XMPP_JID=//p' "$WORK/manifest-lunarwing.env" | head -1)"; XMPP_JID="${XMPP_JID%$'\r'}"
XMPP_ALLOW_FROM="$(manifest_value XMPP_ALLOW_FROM)"
GATEWAY_HOST="$(manifest_value GATEWAY_HOST)"
[[ -n "$GATEWAY_HOST" ]] || GATEWAY_HOST="$(manifest_value HTTP_HOST)"
LLM_MODEL="$(manifest_value LLM_MODEL)"
LLM_BASE_URL="$(manifest_value LLM_BASE_URL)"
NANOCODE_MODEL="$(manifest_value NANOCODE_MODEL)"
NANOCODE_BASE_URL="$(manifest_value NANOCODE_BASE_URL)"
OPENCODE_MODEL="$(manifest_value OPENCODE_MODEL)"
OPENCODE_BASE_URL="$(manifest_value OPENCODE_BASE_URL)"
GOTIFY_URL="$(manifest_value GOTIFY_URL)"

confirm "Stage tenant '$TENANT' on THIS host from the bundle?" || die "aborted by user"

# ---- 1. provision fresh (no daemon, no health) -------------------------------
banner "1/8  Provision (add-tenant --no-health)"
add_args=(add-tenant "$TENANT" --no-health)
$WITH_DOCKER_GROUP && add_args+=(--docker-group)
$DARKIRC_ENABLED && add_args+=(--enable-darkirc --darkirc-scope-id "$DARKIRC_SCOPE_ID")
[[ -n "$XMPP_JID" ]] && add_args+=(--xmpp-jid "$XMPP_JID")
[[ -n "$XMPP_ALLOW_FROM" ]] && add_args+=(--xmpp-allow-from "$XMPP_ALLOW_FROM")
[[ -n "$GATEWAY_HOST" ]] && add_args+=(--gateway-host "$GATEWAY_HOST")
[[ -n "$LLM_MODEL" ]] && add_args+=(--llm-model "$LLM_MODEL")
[[ -n "$LLM_BASE_URL" ]] && add_args+=(--llm-base-url "$LLM_BASE_URL")
[[ -n "$NANOCODE_MODEL" ]] && add_args+=(--nanocode-model "$NANOCODE_MODEL")
[[ -n "$NANOCODE_BASE_URL" ]] && add_args+=(--nanocode-base-url "$NANOCODE_BASE_URL")
[[ -n "$OPENCODE_MODEL" ]] && add_args+=(--opencode-model "$OPENCODE_MODEL")
[[ -n "$OPENCODE_BASE_URL" ]] && add_args+=(--opencode-base-url "$OPENCODE_BASE_URL")
[[ -n "$GOTIFY_URL" ]] && add_args+=(--gotify-url "$GOTIFY_URL")
# Persist the operator's worker selection at add-tenant (not just build-tenant):
# start-tenant now gates on the per-tenant registry flag, so an imported tenant
# started with --start would build its workers but never start them unless the
# selection is recorded here too. Mirrors the --with-* flags passed to
# build-tenant below. See PER_TENANT_WORKER_GATING.md.
$WITH_NANOCODE && add_args+=(--with-nanocode)
$WITH_PEBBLE  && add_args+=(--with-pebble)
$WITH_OPENCODE && add_args+=(--with-opencode)
run "$MT" "${add_args[@]}"

HOME_T="$(getent passwd "$TENANT" | cut -d: -f6 2>/dev/null || echo "/home/$TENANT")"
LWROOT="$HOME_T/lunarwing"
ENVF="$LWROOT/env/lunarwing.env"
BRIDGE_ENVF="$LWROOT/env/xmpp-bridge.env"
VISION_ENVF="$LWROOT/env/vision.env"

# ---- 2. build daemon + workers -----------------------------------------------
banner "2/8  Build"
build_args=(build-tenant "$TENANT" --with-wasm)
$WITH_NANOCODE && build_args+=(--with-nanocode)
$WITH_PEBBLE  && build_args+=(--with-pebble)
$WITH_OPENCODE && build_args+=(--with-opencode)
$WITH_TOOLCHAINS && build_args+=(--with-toolchains)
run "$MT" "${build_args[@]}"
$WITH_VISION && run "$MT" build-vision-sidecar

# ---- 3. inject carried secrets + config (CRITICAL: SECRETS_MASTER_KEY) -------
banner "3/8  Inject carried secrets + config"
if $DRY_RUN; then
  note "[dry-run] would inject manifest-lunarwing.env -> $ENVF and manifest-bridge.env -> $BRIDGE_ENVF (incl. SECRETS_MASTER_KEY, XMPP password, XMPP/LLM config)"
  [[ -s "$WORK/manifest-vision.env" ]] && note "[dry-run] would inject manifest-vision.env -> $VISION_ENVF (VL_URL, VL_MODEL, LUNARWING_AUTH_TOKEN)"
else
  validate_import_target_roots
  inject_keys "$WORK/manifest-lunarwing.env" "$ENVF" "${IMPORT_LUNARWING_KEYS[@]}"
  [[ -f "$WORK/manifest-bridge.env" ]] && \
    inject_keys "$WORK/manifest-bridge.env" "$BRIDGE_ENVF" "${IMPORT_BRIDGE_KEYS[@]}"
  if [[ -s "$WORK/manifest-vision.env" ]]; then
    [[ -f "$VISION_ENVF" ]] || die "bundle carries vision config, but target mt-admin did not render $VISION_ENVF"
    inject_keys "$WORK/manifest-vision.env" "$VISION_ENVF" "${IMPORT_VISION_KEYS[@]}"
    note "vision.env carried config injected"
  fi
  # The tenant-side merge compares the master-key line internally and returns
  # only a boolean, so the secret never enters a root shell variable.
  [[ "$INJECTED_MASTER_KEY_VERIFIED" == true ]] \
    || die "SECRETS_MASTER_KEY did not land in $ENVF after injection — abort before restore (the DB's secrets would be undecryptable)"
  note "SECRETS_MASTER_KEY confirmed in place (verbatim)"
fi

# ---- 4. restore the database (PG up from step 1, daemon not started) ---------
banner "4/8  Restore database"
if $DRY_RUN; then note "[dry-run] would: $MT restore-tenant $TENANT <bundle db.dump> --yes"
else "$MT" restore-tenant "$TENANT" "$WORK/db.dump" --yes; fi

reconcile_owner_scope

# ---- 6. structured DarkIRC contacts (never via generic state archive) --------
if $HAS_DARKIRC_MANIFEST; then
  banner "6/8  DarkIRC contacts"
  run_bundle_manifest_stdin \
    "$MT" darkirc-contact stage-migration "$TENANT" --in -
  run "$MT" darkirc-contact import-migration "$TENANT"
  note "DarkIRC contacts imported through the target baseline and scope contract"
fi

# ---- 7. restore on-disk state (OMEMO/workspace), then fresh WASM -------------
banner "7/8  Restore state + install WASM"
if [[ -f "$WORK/state.tar.gz" ]]; then
  if $DRY_RUN; then note "[dry-run] would: tar xzf state.tar.gz into $LWROOT (OMEMO + workspace; excluding state/darkirc), chown to $TENANT"
  else
    # Defensive excludes (export already strips these): never let a stale config.toml
    # or old *.wasm overwrite the fresh host-specific ones. DarkIRC is always
    # structured separately so stale ports or transactional secrets cannot land.
    validate_import_target_roots
    # Root opens the protected bundle member; tar itself runs unprivileged and
    # receives archive bytes only over stdin.
    # shellcheck disable=SC2024
    sudo -u "$TENANT" tar xzf - -C "$LWROOT" \
      --no-same-owner --no-same-permissions \
      --keep-old-files \
      --exclude='state/config.toml' --exclude='state/tools/*.wasm' \
      --exclude='state/channels/*.wasm' --exclude='state/darkirc' \
      --exclude='state/darkirc/**' <"$WORK/state.tar.gz"
    note "restored state dir$( [[ -d "$LWROOT/state/xmpp" ]] && echo ' (incl. OMEMO store)' )"
  fi
else
  note "bundle has no state.tar.gz — OMEMO/workspace start fresh"
fi
run "$MT" install-wasm "$TENANT"   # lay down current v1.1.4 .wasm artifacts

banner "WeeChat migration preflight"
run "$WEECHAT_PREFLIGHT" "$TENANT"

# ---- 8. cutover ---------------------------------------------------------------
banner "8/8  Cutover"
stage_msg() {
  say "Tenant '$TENANT' is STAGED (DB + secrets + state restored, units rendered) but NOT started."
  say ""
  say "To cut over:"
  say "  1. Ensure '$TENANT' is stopped on the OLD host (export already stops it; same"
  say "     XMPP JID '${XMPP_JID:-?}' — avoid a double login)."
  say "  2. sudo $MT start-tenant $TENANT"
  say "  3. Verify, then once ALL agents are migrated enable self-heal fleet-wide:"
  say "     sudo ic/scripts/enable-health-fleet.sh --gotify-url <url> --gotify-token-file <path>"
  say ""
  say "Rollback: the OLD host is intact — restart '$TENANT' there."
}
if ! $DO_START; then stage_msg; exit 0; fi

# --start: the double-login gate is NOT satisfied by --yes alone.
if ! $OLD_STOPPED; then
  if [[ -t 0 ]]; then
    say "⚠  The new daemon will log into XMPP JID '${XMPP_JID:-?}'. Two simultaneous logins on one JID conflict."
    confirm "Confirm the OLD host's '$TENANT' is STOPPED — start it here now?" \
      || { say ""; stage_msg; exit 0; }
  else
    die "refusing to start unattended without --old-stopped (would risk a double login on JID '${XMPP_JID:-?}'). Re-run with --old-stopped once the old host is stopped, or omit --start to stage."
  fi
fi
run "$MT" start-tenant "$TENANT"
if ! $DRY_RUN; then
  if $DARKIRC_ENABLED; then
    "$MT" darkirc-health "$TENANT" --strict --json \
      || { "$MT" stop-tenant "$TENANT" >/dev/null 2>&1 || true; die "DarkIRC strict health failed after import; tenant was stopped"; }
  fi
  run "$MT" status "$TENANT"
  note "Smoke-test: a message round-trips, history present, routines + channels load, OMEMO decrypts."
fi
say ""
say "Rollback: the OLD host is intact — stop '$TENANT' here and restart it there."
