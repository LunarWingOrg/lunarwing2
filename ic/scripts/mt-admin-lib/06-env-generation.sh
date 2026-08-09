#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# 06-env-generation.sh — part of the lunarwing-mt-admin library.
#
# Tenant environment-file generation: lunarwing/bridge/proxy/darkirc env
# writers, WeeChat relay auto-bootstrap, study-notes path validation, atomic
# file writes, and DarkIRC configuration generation. No init-system,
# container, or registry awareness.
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# composition root defines all constants and `set -euo pipefail` before any
# library is sourced, so this file only defines functions.
#
# Extracted from master (v2.0.6.0 era) as Phase 2 of the mt-admin refactor.
# Architecture follows docs/proposals/MT-ADMIN-DECOMPOSITION.md and the prior
# attempt on origin/mtadminrefactor/meta4/v2.0.3.0, updated for new growth:
# study-notes path validation, voice env writer, and expanded DarkIRC
# compatibility/scope-id handling.
#
# Pure line-range move from the monolith: no function body was rewritten.

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

