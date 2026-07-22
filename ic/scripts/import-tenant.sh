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

normalize_bundle_member() {
  BUNDLE_MEMBER="$1"
  while [[ "$BUNDLE_MEMBER" == ./* ]]; do BUNDLE_MEMBER="${BUNDLE_MEMBER#./}"; done
  [[ -n "$BUNDLE_MEMBER" && "$BUNDLE_MEMBER" != /* ]] \
    || die "bundle contains an absolute or empty path"
  case "/$BUNDLE_MEMBER/" in
    */../*) die "bundle contains parent-directory traversal: $1" ;;
  esac
}

validate_bundle_member_name() {
  normalize_bundle_member "$1"
  case "$BUNDLE_MEMBER" in
    meta.txt|db.dump|manifest-lunarwing.env|manifest-bridge.env|manifest-vision.env|state.tar.gz) ;;
    *) die "bundle contains unexpected top-level entry '$BUNDLE_MEMBER'" ;;
  esac
}

validate_7z_archive() {
  local listing in_entries="false" line path="" size attrs mode count=0 total=0
  declare -A seen=()
  listing="$({ printf '%s\n' "$KAWARIMI_ARCHIVE_PASS"; } | 7z l -slt "$BUNDLE" 2>/dev/null)" \
    || die "failed to inspect encrypted bundle (wrong passphrase or corrupted archive?)"
  while IFS= read -r line; do
    if [[ "$line" == "----------" ]]; then in_entries="true"; continue; fi
    [[ "$in_entries" == "true" ]] || continue
    case "$line" in
      "Path = "*)
        path="${line#Path = }"
        validate_bundle_member_name "$path"
        [[ -z "${seen[$BUNDLE_MEMBER]+x}" ]] || die "bundle contains duplicate entry '$BUNDLE_MEMBER'"
        seen["$BUNDLE_MEMBER"]=1
        count=$((count + 1))
        (( count <= 16 )) || die "bundle contains too many top-level entries"
        ;;
      "Size = "*)
        size="${line#Size = }"
        [[ "$size" =~ ^[0-9]+$ ]] || die "bundle contains an invalid member size"
        total=$((total + size))
        (( total <= MAX_BUNDLE_BYTES )) || die "bundle expands beyond KAWARIMI_MAX_BUNDLE_BYTES"
        ;;
      "Attributes = "*)
        attrs="${line#Attributes = }"
        mode="${attrs##* }"
        [[ "$mode" == -* ]] || die "bundle entry '$path' is not a regular file"
        ;;
      "Symbolic Link = "*|"Hard Link = "*)
        die "bundle entry '$path' is a link"
        ;;
    esac
  done <<<"$listing"
  [[ -n "${seen[meta.txt]+x}" && -n "${seen[db.dump]+x}" && -n "${seen[manifest-lunarwing.env]+x}" ]] \
    || die "bundle is missing required files"
}

validate_tar_archive() {
  local names verbose line type owner size count=0 total=0
  declare -A seen=()
  names="$(tar tf "$BUNDLE")" || die "failed to inspect legacy tar bundle"
  while IFS= read -r line; do
    [[ "$line" == "." || "$line" == "./" ]] && continue
    validate_bundle_member_name "$line"
    [[ -z "${seen[$BUNDLE_MEMBER]+x}" ]] || die "bundle contains duplicate entry '$BUNDLE_MEMBER'"
    seen["$BUNDLE_MEMBER"]=1
    count=$((count + 1))
    (( count <= 16 )) || die "bundle contains too many top-level entries"
  done <<<"$names"
  [[ -n "${seen[meta.txt]+x}" && -n "${seen[db.dump]+x}" && -n "${seen[manifest-lunarwing.env]+x}" ]] \
    || die "bundle is missing required files"

  verbose="$(tar --numeric-owner -tvf "$BUNDLE")" || die "failed to inspect legacy tar metadata"
  while IFS= read -r line; do
    type="${line:0:1}"
    if [[ "$type" == "d" && "$line" == *" ./" ]]; then continue; fi
    [[ "$type" == "-" ]] || die "legacy bundle contains a link or special file"
    read -r _ owner size _ <<<"$line"
    [[ "$owner" == */* && "$size" =~ ^[0-9]+$ ]] || die "legacy bundle contains invalid metadata"
    total=$((total + size))
    (( total <= MAX_BUNDLE_BYTES )) || die "bundle expands beyond KAWARIMI_MAX_BUNDLE_BYTES"
  done <<<"$verbose"
}

validate_manifest() {
  local file="$1" kind="$2" line key
  declare -A seen=()
  [[ -f "$file" && ! -L "$file" ]] || die "bundle contains an unsafe $kind manifest"
  while IFS= read -r line || [[ -n "$line" ]]; do
    line="${line%$'\r'}"
    [[ -z "$line" || "$line" == \#* ]] && continue
    [[ "$line" == *=* ]] || die "$kind manifest contains a malformed line"
    key="${line%%=*}"
    [[ "$key" =~ ^[A-Z][A-Z0-9_]*$ ]] || die "$kind manifest contains an invalid key name"
    [[ -z "${seen[$key]+x}" ]] || die "$kind manifest contains duplicate key '$key'"
    seen["$key"]=1
    case "$kind:$key" in
      lunarwing:SECRETS_MASTER_KEY|lunarwing:XMPP_JID|lunarwing:XMPP_PASSWORD|lunarwing:XMPP_DM_POLICY|lunarwing:XMPP_ALLOW_FROM|lunarwing:XMPP_ALLOW_ROOMS|lunarwing:XMPP_ENCRYPTED_ROOMS|lunarwing:XMPP_ALLOW_PLAINTEXT_FALLBACK|lunarwing:XMPP_OMEMO_DEVICE_ID|lunarwing:LLM_API_KEY|lunarwing:LLM_MODEL|lunarwing:LLM_BASE_URL|lunarwing:NANOCODE_MODEL|lunarwing:NANOCODE_BASE_URL|lunarwing:OPENCODE_MODEL|lunarwing:OPENCODE_BASE_URL|lunarwing:GOTIFY_URL|lunarwing:GATEWAY_HOST|lunarwing:HTTP_HOST) ;;
      bridge:XMPP_JID|bridge:XMPP_PASSWORD|bridge:XMPP_DM_POLICY|bridge:XMPP_ALLOW_FROM_JSON|bridge:XMPP_ALLOW_ROOMS_JSON|bridge:XMPP_ENCRYPTED_ROOMS_JSON|bridge:XMPP_DEVICE_ID|bridge:XMPP_ALLOW_PLAINTEXT_FALLBACK) ;;
      vision:VL_URL|vision:VL_MODEL|vision:LUNARWING_AUTH_TOKEN) ;;
      *) die "$kind manifest contains unsupported key '$key'" ;;
    esac
  done <"$file"
}

validate_bundle_layout() {
  local entry name unsafe_link
  unsafe_link="$(find "$WORK" -type l -print -quit 2>/dev/null || true)"
  [[ -z "$unsafe_link" ]] || die "bundle contains symbolic links: $unsafe_link"
  shopt -s nullglob dotglob
  for entry in "$WORK"/*; do
    name="${entry##*/}"
    case "$name" in
      meta.txt|db.dump|manifest-lunarwing.env|manifest-bridge.env|manifest-vision.env|state.tar.gz) ;;
      *) die "bundle contains unexpected top-level entry '$name'" ;;
    esac
    [[ -f "$entry" ]] || die "bundle entry '$name' is not a regular file"
  done
  [[ -f "$WORK/meta.txt" && -f "$WORK/db.dump" && -f "$WORK/manifest-lunarwing.env" ]] \
    || die "bundle is missing required regular files"
}

validate_state_archive_paths() {
  local names verbose member normalized line type owner size count=0 total=0
  names="$(tar tzf "$WORK/state.tar.gz")" || die "failed to inspect state archive"
  while IFS= read -r member; do
    normalized="${member#./}"
    [[ -n "$normalized" && "$normalized" != /* ]] \
      || die "state archive contains an absolute or empty path"
    case "/$normalized/" in
      */../*) die "state archive contains parent-directory traversal: $member" ;;
    esac
    [[ "$normalized" == "state" || "$normalized" == state/* ]] \
      || die "state archive contains an entry outside state/: $member"
    count=$((count + 1))
    (( count <= MAX_STATE_ENTRIES )) || die "state archive contains too many entries"
  done <<<"$names"

  verbose="$(tar --numeric-owner -tvzf "$WORK/state.tar.gz")" || die "failed to inspect state archive metadata"
  while IFS= read -r line; do
    type="${line:0:1}"
    [[ "$type" == "-" || "$type" == "d" ]] \
      || die "state archive contains a link or special file"
    read -r _ owner size _ <<<"$line"
    [[ "$owner" == */* && "$size" =~ ^[0-9]+$ ]] || die "state archive contains invalid metadata"
    total=$((total + size))
    (( total <= MAX_STATE_BYTES )) || die "state archive expands beyond KAWARIMI_MAX_STATE_BYTES"
  done <<<"$verbose"
}

validate_passphrase() {
  [[ -n "$1" ]] || die "bundle passphrase must not be empty"
  [[ "${#1}" -le 1024 ]] || die "bundle passphrase must be at most 1024 characters"
  [[ "$1" != *$'\n'* && "$1" != *$'\r'* ]] || die "bundle passphrase must not contain line breaks"
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

acquire_import_passphrase() {
  local fd
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
    [[ -t 0 ]] || die "bundle passphrase required via KAWARIMI_PASS_FD, KAWARIMI_PASS_FILE, KAWARIMI_PASS, or an interactive terminal"
    read -r -s -p "Enter bundle passphrase: " KAWARIMI_ARCHIVE_PASS
    echo
  fi
  unset KAWARIMI_PASS KAWARIMI_PASS_FILE KAWARIMI_PASS_FD
  validate_passphrase "$KAWARIMI_ARCHIVE_PASS"
}

# Inject KEY=value lines from a manifest into a live env file, backslash-safe (awk
# ENVIRON, not -v) and CR-tolerant; preserves the live file's inode/owner/mode. The
# read loop's `|| [[ -n "$line" ]]` keeps a final line without a trailing newline.
inject_keys() {  # <manifest> <live_env>
  local man="$1" live="$2" line key tmp
  [[ -f "$man" && -f "$live" ]] || return 0
  while IFS= read -r line || [[ -n "$line" ]]; do
    line="${line%$'\r'}"
    [[ "$line" == *=* ]] || continue
    key="${line%%=*}"
    grep -qxF "$line" "$live" 2>/dev/null && continue
    tmp="$(mktemp)"
    if grep -q "^${key}=" "$live"; then
      _ik_repl="$line" awk -v k="${key}=" 'index($0,k)==1{print ENVIRON["_ik_repl"];next}{print}' "$live" >"$tmp"
    else
      cp "$live" "$tmp"; printf '%s\n' "$line" >>"$tmp"
    fi
    cat "$tmp" >"$live"; rm -f "$tmp"
    note "injected $key"
  done < "$man"
}

non_target_owner_scopes() {  # <scope-summary> <target-scope>
  local summary="$1" target="$2" scope
  while IFS=$' \t' read -r scope _; do
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
  banner "5/7  Owner scope"

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
[[ "$MAX_BUNDLE_BYTES" =~ ^[1-9][0-9]*$ && "$MAX_STATE_BYTES" =~ ^[1-9][0-9]*$ && "$MAX_STATE_ENTRIES" =~ ^[1-9][0-9]*$ ]] \
  || die "Kawarimi archive limits must be positive integers"
[[ "$(stat -c '%s' "$BUNDLE")" -le "$MAX_BUNDLE_BYTES" ]] \
  || die "bundle exceeds KAWARIMI_MAX_BUNDLE_BYTES before extraction"
ORIGINAL_BUNDLE="$BUNDLE"
[[ "$(id -u)" -eq 0 ]] || die "run as root (sudo) — mt-admin needs root"
command -v jq  >/dev/null 2>&1 || die "jq required"

# Check for 7z if importing an encrypted bundle
if [[ "$BUNDLE" == *.7z ]]; then
  command -v 7z >/dev/null 2>&1 || die "7z (p7zip) required to unpack encrypted bundle"
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
MT="$SCRIPT_DIR/lunarwing-mt-admin.sh"
WEECHAT_PREFLIGHT="$SCRIPT_DIR/lunarwing-weechat-preflight.sh"
PORTS_REGISTRY="${LUNARWING_PORTS_REGISTRY:-/etc/lunarwing/ports.json}"
[[ -x "$MT" ]] || die "mt-admin not found/executable at $MT"
[[ -x "$WEECHAT_PREFLIGHT" ]] || die "WeeChat preflight not found/executable at $WEECHAT_PREFLIGHT"
grep -qE '^\s*restore-tenant)' "$MT" || die "mt-admin at $MT predates restore-tenant (need a v1.1.4-class host)"
grep -qE '^\s*owner-scopes)' "$MT" || die "mt-admin at $MT predates owner-scopes (need current Kawarimi owner-scope checks)"

WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
chmod 0700 "$WORK"

# Pin the archive in a private directory so validation and extraction consume
# the same inode even when the operator supplied a path in a shared directory.
case "$ORIGINAL_BUNDLE" in
  *.7z) BUNDLE="$WORK/.kawarimi-input.7z" ;;
  *.tar) BUNDLE="$WORK/.kawarimi-input.tar" ;;
  *) die "unknown bundle format: $ORIGINAL_BUNDLE (expected .7z or .tar)" ;;
esac
head -c "$((MAX_BUNDLE_BYTES + 1))" -- "$ORIGINAL_BUNDLE" >"$BUNDLE" \
  || die "failed to copy bundle into private staging"
chmod 0600 "$BUNDLE"
[[ "$(stat -c '%s' "$BUNDLE")" -le "$MAX_BUNDLE_BYTES" ]] \
  || die "bundle exceeds KAWARIMI_MAX_BUNDLE_BYTES during private staging"

# Unpack bundle — detect format
if [[ "$BUNDLE" == *.7z ]]; then
  acquire_import_passphrase
  validate_7z_archive
  # Omitting -p makes p7zip/7-Zip read the decryption password from stdin.
  { printf '%s\n' "$KAWARIMI_ARCHIVE_PASS"; } | 7z x -o"$WORK" "$BUNDLE" -y >/dev/null 2>&1 \
    || die "failed to decrypt/unpack bundle (wrong passphrase or corrupted archive?)"
  unset KAWARIMI_ARCHIVE_PASS
elif [[ "$BUNDLE" == *.tar ]]; then
  # Legacy plaintext bundle — backward compat
  echo "WARNING: importing UNENCRYPTED legacy .tar bundle" >&2
  validate_tar_archive
  tar xf "$BUNDLE" -C "$WORK" || die "failed to unpack bundle $BUNDLE"
else
  die "unknown bundle format: $BUNDLE (expected .7z or .tar)"
fi
rm -f "$BUNDLE"

validate_bundle_layout
validate_manifest "$WORK/manifest-lunarwing.env" lunarwing
[[ ! -e "$WORK/manifest-bridge.env" ]] || validate_manifest "$WORK/manifest-bridge.env" bridge
[[ ! -e "$WORK/manifest-vision.env" ]] || validate_manifest "$WORK/manifest-vision.env" vision
[[ ! -e "$WORK/state.tar.gz" ]] || validate_state_archive_paths

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
# Sanitize the tenant name the same way mt-admin does ([a-z0-9-]), so our own
# path/getent/chown use exactly the name mt-admin will use internally.
RAW_NAME="${NAME_OVERRIDE:-$(meta tenant)}"
TENANT="$(printf '%s' "$RAW_NAME" | tr '[:upper:]' '[:lower:]' | tr -cd 'a-z0-9-')"
[[ -n "$TENANT" ]] || die "could not determine a valid tenant name (got '$RAW_NAME'; pass --name)"
[[ "$TENANT" == "$RAW_NAME" ]] || note "tenant name sanitized: '$RAW_NAME' -> '$TENANT'"
SRC_VER="$(meta source_version)"; DB_BACKEND="$(meta db_backend)"; DB_BACKEND="${DB_BACKEND:-postgres}"

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
banner "1/7  Provision (add-tenant --no-health)"
add_args=(add-tenant "$TENANT" --no-health)
$WITH_DOCKER_GROUP && add_args+=(--docker-group)
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
banner "2/7  Build"
build_args=(build-tenant "$TENANT" --with-wasm)
$WITH_NANOCODE && build_args+=(--with-nanocode)
$WITH_PEBBLE  && build_args+=(--with-pebble)
$WITH_OPENCODE && build_args+=(--with-opencode)
$WITH_TOOLCHAINS && build_args+=(--with-toolchains)
run "$MT" "${build_args[@]}"
$WITH_VISION && run "$MT" build-vision-sidecar

# ---- 3. inject carried secrets + config (CRITICAL: SECRETS_MASTER_KEY) -------
banner "3/7  Inject carried secrets + config"
if $DRY_RUN; then
  note "[dry-run] would inject manifest-lunarwing.env -> $ENVF and manifest-bridge.env -> $BRIDGE_ENVF (incl. SECRETS_MASTER_KEY, XMPP password, XMPP/LLM config)"
  [[ -s "$WORK/manifest-vision.env" ]] && note "[dry-run] would inject manifest-vision.env -> $VISION_ENVF (VL_URL, VL_MODEL, LUNARWING_AUTH_TOKEN)"
else
  inject_keys "$WORK/manifest-lunarwing.env" "$ENVF"
  [[ -f "$WORK/manifest-bridge.env" ]] && inject_keys "$WORK/manifest-bridge.env" "$BRIDGE_ENVF"
  if [[ -s "$WORK/manifest-vision.env" ]]; then
    [[ -f "$VISION_ENVF" ]] || die "bundle carries vision config, but target mt-admin did not render $VISION_ENVF"
    inject_keys "$WORK/manifest-vision.env" "$VISION_ENVF"
    chown "$TENANT:$TENANT" "$VISION_ENVF" 2>/dev/null || true
    note "vision.env carried config injected"
  fi
  chown "$TENANT:$TENANT" "$ENVF" "$BRIDGE_ENVF" 2>/dev/null || true
  # Verify the master key landed VERBATIM — without putting the value on argv
  # (command substitution keeps it out of /proc/<pid>/cmdline) and without the
  # grep -F "" empty-pattern false-PASS.
  man_key="$(grep -m1 '^SECRETS_MASTER_KEY=' "$WORK/manifest-lunarwing.env" || true)"
  live_key="$(grep -m1 '^SECRETS_MASTER_KEY=' "$ENVF" || true)"
  [[ -n "$man_key" && "$live_key" == "$man_key" ]] \
    || die "SECRETS_MASTER_KEY did not land in $ENVF after injection — abort before restore (the DB's secrets would be undecryptable)"
  note "SECRETS_MASTER_KEY confirmed in place (verbatim)"
fi

# ---- 4. restore the database (PG up from step 1, daemon not started) ---------
banner "4/7  Restore database"
if $DRY_RUN; then note "[dry-run] would: $MT restore-tenant $TENANT <bundle db.dump> --yes"
else "$MT" restore-tenant "$TENANT" "$WORK/db.dump" --yes; fi

reconcile_owner_scope

# ---- 6. restore on-disk state (OMEMO/workspace), then fresh WASM -------------
banner "6/7  Restore state + install WASM"
if [[ -f "$WORK/state.tar.gz" ]]; then
  if $DRY_RUN; then note "[dry-run] would: tar xzf state.tar.gz into $LWROOT (OMEMO + workspace), chown to $TENANT"
  else
    # Defensive excludes (export already strips these): never let a stale config.toml
    # or old *.wasm overwrite the fresh host-specific ones.
    tar xzf "$WORK/state.tar.gz" -C "$LWROOT" --no-same-owner --no-same-permissions \
      --exclude='state/config.toml' --exclude='state/tools/*.wasm' --exclude='state/channels/*.wasm'
    chown -R "$TENANT:$TENANT" "$LWROOT/state"
    note "restored state dir$( [[ -d "$LWROOT/state/xmpp" ]] && echo ' (incl. OMEMO store)' )"
  fi
else
  note "bundle has no state.tar.gz — OMEMO/workspace start fresh"
fi
run "$MT" install-wasm "$TENANT"   # lay down current v1.1.4 .wasm artifacts

banner "WeeChat migration preflight"
run "$WEECHAT_PREFLIGHT" "$TENANT"

# ---- 7. cutover ---------------------------------------------------------------
banner "7/7  Cutover"
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
  run "$MT" status "$TENANT"
  note "Smoke-test: a message round-trips, history present, routines + channels load, OMEMO decrypts."
fi
say ""
say "Rollback: the OLD host is intact — stop '$TENANT' here and restart it there."
