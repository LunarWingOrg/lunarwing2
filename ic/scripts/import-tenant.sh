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
#        [--with-nanocode] [--with-pebble] [--with-opencode] [--with-kimi] [--with-toolchains]
#        [--with-vision] [--docker-group]
#        [--owner-scope <old_scope>] [--dry-run] [--yes] [--force]
# Encrypted bundles read their passphrase from KAWARIMI_PASS_FD, KAWARIMI_PASS,
# KAWARIMI_PASS_FILE, or an interactive prompt, in that order. The passphrase is
# fed to 7z over stdin and never placed in command argv.
set -euo pipefail

IMPORT_LUNARWING_KEYS=(
  SECRETS_MASTER_KEY XMPP_JID XMPP_PASSWORD XMPP_DM_POLICY XMPP_ALLOW_FROM
  XMPP_ALLOW_ROOMS XMPP_ENCRYPTED_ROOMS XMPP_ALLOW_PLAINTEXT_FALLBACK
  XMPP_OMEMO_DEVICE_ID LLM_API_KEY LLM_MODEL LLM_BASE_URL NANOCODE_MODEL
  NANOCODE_BASE_URL OPENCODE_MODEL OPENCODE_BASE_URL KIMI_MODEL KIMI_BASE_URL
  KIMI_API_KEY GOTIFY_URL GATEWAY_HOST HTTP_HOST
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
WITH_KIMI=false
WITH_TOOLCHAINS=false
WITH_VISION=false
OWNER_SCOPE=""
DRY_RUN=false
AUTO_YES=false
FORCE=false
MAX_BUNDLE_BYTES="${KAWARIMI_MAX_BUNDLE_BYTES:-107374182400}"
MAX_STATE_BYTES="${KAWARIMI_MAX_STATE_BYTES:-107374182400}"
MAX_STATE_ENTRIES="${KAWARIMI_MAX_STATE_ENTRIES:-1000000}"
TENANT_LIFECYCLE_LOCK_DIR="${LUNARWING_MT_LIFECYCLE_LOCK_DIR:-/run/lock/lunarwing-tenants}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --name)          NAME_OVERRIDE="$2"; shift 2 ;;
    --start)         DO_START=true; shift ;;
    --old-stopped)   OLD_STOPPED=true; shift ;;
    --docker-group)  WITH_DOCKER_GROUP=true; shift ;;
    --with-nanocode) WITH_NANOCODE=true; shift ;;
    --with-pebble)   WITH_PEBBLE=true; shift ;;
    --with-opencode)   WITH_OPENCODE=true; shift ;;
    --with-kimi)       WITH_KIMI=true; shift ;;
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
    meta.txt|db.dump|manifest-lunarwing.env|manifest-bridge.env|manifest-vision.env|state.tar.gz|darkirc-contacts-v1.json) ;;
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
  local file="$1" kind="$2" line key allowed_name allowed
  case "$kind" in
    lunarwing) allowed_name=IMPORT_LUNARWING_KEYS ;;
    bridge) allowed_name=IMPORT_BRIDGE_KEYS ;;
    vision) allowed_name=IMPORT_VISION_KEYS ;;
    *) die "unknown manifest kind '$kind'" ;;
  esac
  local -n allowed_keys="$allowed_name"
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
    for allowed in "${allowed_keys[@]}"; do
      [[ "$key" == "$allowed" ]] && continue 2
    done
    die "$kind manifest contains unsupported key '$key'"
  done <"$file"
}

path_components_no_symlink() {
  local path="$1" rest component current="/"
  [[ "$path" == /* ]] || return 1
  rest="${path#/}"
  while [[ -n "$rest" ]]; do
    component="${rest%%/*}"
    if [[ "$rest" == */* ]]; then rest="${rest#*/}"; else rest=""; fi
    [[ -n "$component" && "$component" != . && "$component" != .. ]] || return 1
    current="${current%/}/$component"
    [[ ! -L "$current" ]] || return 1
    [[ -z "$rest" || -d "$current" ]] || return 1
  done
}

validate_mt_admin_override() {
  local path="$1" dir rest component current="" owner mode links
  [[ "$path" == /* && -f "$path" && ! -L "$path" && -x "$path" ]] || return 1
  owner="$(/usr/bin/stat -c '%u' "$path" 2>/dev/null || true)"
  mode="$(/usr/bin/stat -c '%a' "$path" 2>/dev/null || true)"
  links="$(/usr/bin/stat -c '%h' "$path" 2>/dev/null || true)"
  [[ "$owner" == 0 && "$links" == 1 && "$mode" =~ ^[0-7]+$ ]] || return 1
  (( (8#$mode & 022) == 0 )) || return 1
  dir="${path%/*}"
  [[ -n "$dir" ]] || dir=/
  path_components_no_symlink "$dir" || return 1
  rest="${dir#/}"
  while [[ -n "$rest" ]]; do
    component="${rest%%/*}"
    current="$current/$component"
    owner="$(/usr/bin/stat -c '%u' "$current" 2>/dev/null || true)"
    mode="$(/usr/bin/stat -c '%a' "$current" 2>/dev/null || true)"
    [[ "$owner" == 0 && "$mode" =~ ^[0-7]+$ ]] || return 1
    (( (8#$mode & 022) == 0 )) || return 1
    if [[ "$rest" == */* ]]; then rest="${rest#*/}"; else rest=""; fi
  done
}

validate_import_target_roots() {
  local uid path owner mode links
  uid="$(id -u "$TENANT" 2>/dev/null || true)"
  [[ "$uid" =~ ^[0-9]+$ ]] || die "could not resolve target tenant UID"
  for path in "$HOME_T" "$LWROOT" "$LWROOT/env" "$LWROOT/state"; do
    path_components_no_symlink "$path" \
      || die "unsafe target path component (symlink or missing directory): $path"
    [[ -d "$path" && ! -L "$path" ]] || die "target path is not a regular directory: $path"
    owner="$(stat -c '%u' "$path" 2>/dev/null || true)"
    mode="$(stat -c '%a' "$path" 2>/dev/null || true)"
    [[ "$owner" == "$uid" && "$mode" =~ ^[0-7]+$ ]] \
      || die "target directory has unexpected ownership: $path"
    (( (8#$mode & 022) == 0 )) || die "target directory is group/world writable: $path"
  done
  for path in "$ENVF" "$BRIDGE_ENVF"; do
    [[ -f "$path" && ! -L "$path" ]] || die "target environment file is missing or unsafe: $path"
    path_components_no_symlink "$path" || die "unsafe target environment path: $path"
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

validate_bundle_layout() {
  local entry name unsafe_link
  unsafe_link="$(find "$WORK" -type l -print -quit 2>/dev/null || true)"
  [[ -z "$unsafe_link" ]] || die "bundle contains symbolic links: $unsafe_link"
  shopt -s nullglob dotglob
  for entry in "$WORK"/*; do
    name="${entry##*/}"
    case "$name" in
      meta.txt|db.dump|manifest-lunarwing.env|manifest-bridge.env|manifest-vision.env|state.tar.gz|darkirc-contacts-v1.json) ;;
      *) die "bundle contains unexpected top-level entry '$name'" ;;
    esac
    [[ -f "$entry" ]] || die "bundle entry '$name' is not a regular file"
  done
  [[ -f "$WORK/meta.txt" && -f "$WORK/db.dump" && -f "$WORK/manifest-lunarwing.env" ]] \
    || die "bundle is missing required regular files"
}

validate_state_archive_paths() {
  local names verbose member normalized line type owner size count=0 total=0 component
  local -a components
  declare -A normalized_seen=()
  names="$(tar tzf "$WORK/state.tar.gz")" || die "failed to inspect state archive"
  while IFS= read -r member; do
    [[ -n "$member" && "$member" != /* ]] \
      || die "state archive contains an absolute or empty path"
    normalized=""
    IFS='/' read -r -a components <<<"$member"
    for component in "${components[@]}"; do
      [[ -z "$component" || "$component" == . ]] && continue
      [[ "$component" != .. ]] || die "state archive contains parent-directory traversal: $member"
      [[ "$component" != *$'\n'* && "$component" != *$'\r'* ]] \
        || die "state archive contains control characters"
      normalized="${normalized:+$normalized/}$component"
    done
    [[ -n "$normalized" ]] || die "state archive contains an empty path"
    [[ "$normalized" == "state" || "$normalized" == state/* ]] \
      || die "state archive contains an entry outside state/: $member"
    [[ -z "${normalized_seen[$normalized]+x}" ]] \
      || die "state archive contains duplicate normalized entry: $normalized"
    normalized_seen["$normalized"]=1
    case "$normalized" in
      state/darkirc/key-exchange|state/darkirc/key-exchange/*)
        die "managed DarkIRC state requires structured contact import; generic import refused"
        ;;
    esac
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

# Merge one allowlisted manifest through one tenant-side atomic replacement.
# Secret values stay on stdin and same-directory temporary files, never argv or
# environment. Master-key equality is verified before the rename commits.
inject_keys() {  # <manifest> <live_env> <kind> [skip_key]
  local man="$1" live="$2" kind="$3" skip_key="${4:-}" verify_key=""
  validate_manifest "$man" "$kind"
  path_components_no_symlink "$live" || die "unsafe target environment path: $live"
  [[ -f "$live" && ! -L "$live" ]] || die "target environment file is missing or unsafe: $live"
  [[ "$kind" == lunarwing ]] && verify_key="SECRETS_MASTER_KEY"

  cat "$man" | sudo -u "$TENANT" env -i PATH="/usr/bin:/bin" \
    TARGET_PATH="$live" TARGET_VERIFY_KEY="$verify_key" TARGET_SKIP_KEY="$skip_key" bash -c '
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
      [[ -f "$target" && ! -L "$target" ]] || exit 73
      owner="$(stat -c %u "$target" 2>/dev/null || printf -- -1)"
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
            if (key == ENVIRON["TARGET_SKIP_KEY"]) continue
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
          for (i = 1; i <= count; i++) {
            key = order[i]
            if (!(key in seen)) print replacement[key]
          }
        }
      '\'' "$target" >"$tmp"; then
        exit 74
      fi
      chmod "$mode" "$tmp"
      if [[ -n "${TARGET_VERIFY_KEY:-}" ]]; then
        awk -v key="${TARGET_VERIFY_KEY}=" '\''
          FNR == NR {
            if (index($0, key) == 1) expected = $0
            next
          }
          { if (index($0, key) == 1) actual = $0 }
          END { if (expected == "" || actual != expected) exit 74 }
        '\'' "$patch" "$tmp" || exit 74
      fi
      mv -T -f -- "$tmp" "$target"
      rm -f -- "$patch"
      trap - EXIT
    ' >/dev/null || die "could not safely inject $kind manifest into $live"
  [[ "$kind" == lunarwing ]] && INJECTED_MASTER_KEY_VERIFIED=true
  note "validated and atomically injected $kind manifest into $live"
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

[[ -n "$BUNDLE" ]] || die "usage: $0 <bundle.7z|bundle.tar> [--name <t>] [--start] [--old-stopped] [--with-nanocode] [--with-pebble] [--with-opencode] [--with-kimi] [--with-toolchains] [--with-vision] [--docker-group] [--owner-scope <old_scope>] [--dry-run] [--yes] [--force]"
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
MT="${LUNARWING_MT_ADMIN:-$SCRIPT_DIR/lunarwing-mt-admin.sh}"
WEECHAT_PREFLIGHT="$SCRIPT_DIR/lunarwing-weechat-preflight.sh"
PORTS_REGISTRY="${LUNARWING_PORTS_REGISTRY:-/etc/lunarwing/ports.json}"
[[ -x "$MT" ]] || die "mt-admin not found/executable at $MT"
if [[ -n "${LUNARWING_MT_ADMIN:-}" ]]; then
  validate_mt_admin_override "$MT" \
    || die "LUNARWING_MT_ADMIN must be root-owned, single-link, non-writable, and below trusted directories"
fi
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
if [[ -e "$WORK/darkirc-contacts-v1.json" ]]; then
  [[ -f "$WORK/darkirc-contacts-v1.json" && ! -L "$WORK/darkirc-contacts-v1.json" ]] \
    || die "unsafe DarkIRC structured migration manifest"
  [[ "$(stat -c '%s' "$WORK/darkirc-contacts-v1.json")" -le 1048576 ]] \
    || die "DarkIRC structured migration manifest exceeds size limit"
fi

meta() { sed -n "s/^$1=//p" "$WORK/meta.txt" | head -1; }
manifest_value() {
  local key="$1" file="${2:-$WORK/manifest-lunarwing.env}" value
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
# Sanitize the tenant name the same way mt-admin does ([a-z0-9-]), so our own
# path/getent/chown use exactly the name mt-admin will use internally.
RAW_NAME="${NAME_OVERRIDE:-$(meta tenant)}"
TENANT="$(printf '%s' "$RAW_NAME" | tr '[:upper:]' '[:lower:]' | tr -cd 'a-z0-9-')"
[[ -n "$TENANT" ]] || die "could not determine a valid tenant name (got '$RAW_NAME'; pass --name)"
[[ "$TENANT" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]] \
  || die "tenant name must start and end with a letter or digit"
[[ "$TENANT" == "$RAW_NAME" ]] || note "tenant name sanitized: '$RAW_NAME' -> '$TENANT'"
SRC_VER="$(meta source_version)"; DB_BACKEND="$(meta db_backend)"; DB_BACKEND="${DB_BACKEND:-postgres}"
DARKIRC_SCOPE_ID=""
if [[ -f "$WORK/darkirc-contacts-v1.json" ]]; then
  DARKIRC_SCOPE_ID="$(meta darkirc_scope_id)"
  [[ "$DARKIRC_SCOPE_ID" =~ ^[0-9a-f]{32}$ && "$DARKIRC_SCOPE_ID" != "00000000000000000000000000000000" ]] \
    || die "bundle has invalid or missing DarkIRC stable scope ID"
  grep -qE 'validate-migration' "$MT" \
    || die "target mt-admin lacks typed DarkIRC migration validation"
  if $DRY_RUN; then
    note "[dry-run] would typed-validate darkirc-contacts-v1.json before provisioning"
  else
    manifest_validation="$("$MT" darkirc-contact validate-migration --in - --json \
      <"$WORK/darkirc-contacts-v1.json")" \
      || die "DarkIRC structured migration manifest failed typed validation"
    validated_scope="$(jq -r '.scope_id // empty' <<<"$manifest_validation")"
    [[ "$validated_scope" == "$DARKIRC_SCOPE_ID" ]] \
      || die "DarkIRC manifest scope does not match bundle metadata"
  fi
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
KIMI_MODEL="$(manifest_value KIMI_MODEL)"
KIMI_BASE_URL="$(manifest_value KIMI_BASE_URL)"
KIMI_API_KEY="$(manifest_value KIMI_API_KEY)"
GOTIFY_URL="$(manifest_value GOTIFY_URL)"

confirm "Stage tenant '$TENANT' on THIS host from the bundle?" || die "aborted by user"

if ! $DRY_RUN; then
  command -v flock >/dev/null 2>&1 || die "flock required"
  [[ ! -L "$TENANT_LIFECYCLE_LOCK_DIR" ]]
  install -d -o root -g root -m 0700 "$TENANT_LIFECYCLE_LOCK_DIR"
  [[ "$(stat -c '%u:%a' "$TENANT_LIFECYCLE_LOCK_DIR" 2>/dev/null)" == "0:700" ]] \
    || die "unsafe tenant lifecycle lock directory"
  lifecycle_lock_file="$TENANT_LIFECYCLE_LOCK_DIR/$TENANT.lock"
  [[ ! -L "$lifecycle_lock_file" && ( ! -e "$lifecycle_lock_file" || -f "$lifecycle_lock_file" ) ]] \
    || die "unsafe tenant lifecycle lock file"
  if [[ ! -e "$lifecycle_lock_file" ]]; then (umask 077; : >"$lifecycle_lock_file"); fi
  chmod 0600 "$lifecycle_lock_file"
  exec {IMPORT_LIFECYCLE_LOCK_FD}>"$lifecycle_lock_file"
  flock -x "$IMPORT_LIFECYCLE_LOCK_FD"
  export LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_FD="$IMPORT_LIFECYCLE_LOCK_FD"
  export LUNARWING_MT_INHERITED_LIFECYCLE_LOCK_TENANT="$TENANT"
  if jq -e ".tenants[\"$TENANT\"]" "$PORTS_REGISTRY" >/dev/null 2>&1 && ! $FORCE; then
    die "tenant '$TENANT' was created while import waited for its lifecycle lock"
  fi
fi

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
[[ -n "$KIMI_MODEL" ]] && add_args+=(--kimi-model "$KIMI_MODEL")
[[ -n "$KIMI_BASE_URL" ]] && add_args+=(--kimi-base-url "$KIMI_BASE_URL")
# KIMI_API_KEY is secret-bearing. add-tenant generates placeholder config;
# manifest injection below replaces it without exposing value through argv/logs.
[[ -n "$GOTIFY_URL" ]] && add_args+=(--gotify-url "$GOTIFY_URL")
# Persist the operator's worker selection at add-tenant (not just build-tenant):
# start-tenant now gates on the per-tenant registry flag, so an imported tenant
# started with --start would build its workers but never start them unless the
# selection is recorded here too. Mirrors the --with-* flags passed to
# build-tenant below. See PER_TENANT_WORKER_GATING.md.
$WITH_NANOCODE && add_args+=(--with-nanocode)
$WITH_PEBBLE  && add_args+=(--with-pebble)
$WITH_OPENCODE && add_args+=(--with-opencode)
$WITH_KIMI && add_args+=(--with-kimi)
if [[ -n "$DARKIRC_SCOPE_ID" ]]; then
  add_args+=(--enable-darkirc --darkirc-scope-id "$DARKIRC_SCOPE_ID")
fi
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
$WITH_KIMI && build_args+=(--with-kimi)
$WITH_TOOLCHAINS && build_args+=(--with-toolchains)
run "$MT" "${build_args[@]}"
$WITH_VISION && run "$MT" build-vision-sidecar

# ---- 3. inject carried secrets + config (CRITICAL: SECRETS_MASTER_KEY) -------
banner "3/7  Inject carried secrets + config"
if $DRY_RUN; then
  note "[dry-run] would inject manifest-lunarwing.env -> $ENVF and manifest-bridge.env -> $BRIDGE_ENVF (incl. SECRETS_MASTER_KEY, XMPP password, XMPP/LLM config)"
  [[ -s "$WORK/manifest-vision.env" ]] && note "[dry-run] would inject manifest-vision.env -> $VISION_ENVF (VL_URL and VL_MODEL; target-generated auth token retained)"
else
  validate_import_target_roots
  inject_keys "$WORK/manifest-lunarwing.env" "$ENVF" lunarwing
  [[ -f "$WORK/manifest-bridge.env" ]] \
    && inject_keys "$WORK/manifest-bridge.env" "$BRIDGE_ENVF" bridge
  if [[ -s "$WORK/manifest-vision.env" ]]; then
    [[ -f "$VISION_ENVF" ]] || die "bundle carries vision config, but target mt-admin did not render $VISION_ENVF"
    # Vision auth is host-local. Old bundles may carry it, but add-tenant has
    # already generated a matching target-sidecar/daemon pair; never split it.
    inject_keys "$WORK/manifest-vision.env" "$VISION_ENVF" vision LUNARWING_AUTH_TOKEN
    note "vision.env carried config injected (target-generated auth retained)"
  fi
  [[ "$INJECTED_MASTER_KEY_VERIFIED" == true ]] \
    || die "SECRETS_MASTER_KEY verification did not complete before restore"
  note "SECRETS_MASTER_KEY confirmed before atomic env replacement"
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
    # Root extracts only into root-owned staging. Tenant UID performs destination
    # writes, so hostile pre-existing symlinks cannot become root write gadgets.
    state_stage="$WORK/state-restore"
    mkdir -m 0700 "$state_stage"
    tar xzf "$WORK/state.tar.gz" -C "$state_stage" --no-same-owner --no-same-permissions \
      --exclude='state/config.toml' --exclude='state/tools/*.wasm' --exclude='state/channels/*.wasm'
    tar cf - -C "$state_stage" state | sudo -u "$TENANT" env -i PATH="/usr/bin:/bin" \
      tar xf - -C "$LWROOT" --no-same-owner --no-same-permissions \
        --exclude='state/config.toml' --exclude='state/tools/*.wasm' --exclude='state/channels/*.wasm'
    note "restored state dir$( [[ -d "$LWROOT/state/xmpp" ]] && echo ' (incl. OMEMO store)' )"
  fi
else
  note "bundle has no state.tar.gz — OMEMO/workspace start fresh"
fi
if [[ -f "$WORK/darkirc-contacts-v1.json" ]]; then
  if $DRY_RUN; then
    note "[dry-run] would stage and import structured DarkIRC contacts under scope $DARKIRC_SCOPE_ID"
  else
    "$MT" darkirc-contact restore-migration "$TENANT" <"$WORK/darkirc-contacts-v1.json"
    note "structured DarkIRC contacts imported"
  fi
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
