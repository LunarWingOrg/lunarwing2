#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2154  # constants and helpers are defined by the composition root
# shellcheck disable=SC2317  # functions here are only reached via the dispatcher in the composition root
#
# 16-tui-access.sh — part of the lunarwing-mt-admin library.
#
# `tui-access`: gives a desktop user access to one tenant gateway for Wingtip,
# the multimodal TUI (tui/), without that user ever reading tenant files. Root
# writes, AS the desktop user,
#   ~<user>/.config/wingtip/tenants/<tenant>.json   (0600, 0700 dirs)
# with the gateway URL, the tenant uid and the bearer token. The default
# client-token mode also registers, AS the tenant, only the SHA-256 of a fresh
# 256-bit token in <state>/gateway-client-tokens.json (0600), which daemons
# with client-token support reload every 2 s. Before minting, the verb reads
# GET /api/chat/capabilities with the master token (same SEC-01 rules as the
# probe) and refuses, writing nothing, unless the running daemon lists the
# `client_tokens` feature: a daemon without the loader would otherwise leave
# a dormant credential that comes alive on a later upgrade. --use-master-token
# copies GATEWAY_AUTH_TOKEN instead: a stopgap for daemons that predate client
# tokens, revocable only by `rotate-gateway-token`.
#
# Init-agnostic by construction: the verb never starts, stops or probes a unit
# (no systemctl, no rc-service). Grants and revocations take effect through
# the daemon's file reload, identically on systemd user units and OpenRC. Its
# only network action is an HTTP probe of the registry `gateway` port; the
# token-bearing request is sent only after /proc/net/tcp and tcp6 show that
# the tenant uid alone holds the port and that one of its listeners covers the
# address the probe connects to (SEC-01).
#
# Secrets: the token is minted in-process and reaches sha256sum, the
# tenant-side writer and curl (-K config on a pipe) through stdin or pipes
# only. It never enters argv, stdout, stderr or a log, and output never
# carries a token or a digest.
#
# The tenant-written registry reaches jq only through a tenant-side reader
# that refuses anything the daemon would refuse to load: a symlink, FIFO,
# device, hard link, foreign owner, group or other access, or more than
# 64 KiB (design 8.3.5).
#
# Sourced by ic/scripts/lunarwing-mt-admin.sh; do not execute directly. The
# grant and revoke bodies run inside with_tenant_lifecycle_lock, which turns
# errexit off for its callback, so every fallible step checks its status.

TUI_ACCESS_MAX_CLIENT_TOKENS=32 # the daemon refuses larger registries
TUI_ACCESS_MAX_REGISTRY_BYTES=65536 # the daemon's MAX_FILE_BYTES (design 8.3.5)
TUI_ACCESS_READ_TIMEOUT=10 # seconds for one tenant-side registry read
TUI_ACCESS_PROC_ANY6=00000000000000000000000000000000 # :: in /proc/net/tcp6
TUI_ACCESS_MAX_EXPIRY_DAYS=3650
TUI_ACCESS_TS_FORMAT='+%Y-%m-%dT%H:%M:%SZ'
TUI_ACCESS_EMPTY_REGISTRY='{"version":1,"tokens":[]}'

# jq: why an existing client-token registry must not be rewritten ("ok" when
# it may). Shared by the upsert and remove plans.
TUI_ACCESS_JQ_REASON='def reason:
  if type != "object" then "not-object"
  elif .version != 1 then "version"
  elif (.tokens | type) != "array" then "tokens"
  elif any(.tokens[]; type != "object") then "entry"
  else "ok" end;'

# jq for --list (SEC-13), run with --slurp. Only validated labels and
# timestamps leave jq; anything else becomes a bare entry number. \A and \z
# anchor the whole string: jq's `$` also matches before a trailing newline.
TUI_ACCESS_JQ_LIST='def ts:
  if . == null then "-"
  elif type == "string"
    and test("\\A[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z\\z") then .
  else null end;
def row($n):
  if type == "object" and (.label | type) == "string"
    and (.label | test("\\A[a-z0-9][a-z0-9:._-]{0,63}\\z"))
    and ((.created_at | ts) != null) and ((.expires_at | ts) != null)
  then "\($n)\t\(.label)\t\(.created_at | ts)\t\(.expires_at | ts)"
  else "\($n)" end;
if length != 1 then "!documents"
else .[0]
  | if type != "object" then "!not-object"
    elif .version != 1 then "!version"
    elif (.tokens | type) != "array" then "!tokens"
    else
      (.tokens | length) as $len
      | (.tokens[:$max] | to_entries[] | (.key + 1) as $n | .value | row($n)),
        (if $len > $max then "+\($len - $max)" else empty end)
    end
end'

# ── Small helpers ────────────────────────────────────────────────────────────

_tui_as_user() {  # <user> <command...>
  local user="$1"
  shift
  if [[ "$(id -un)" == "$user" ]]; then
    "$@"
  else
    sudo -u "$user" "$@"
  fi
}

# UID_MIN from login.defs (default 1000). TUI_LOGIN_DEFS exists for the test.
_tui_uid_min() {
  local defs="${TUI_LOGIN_DEFS:-/etc/login.defs}" value=""
  if [[ -f "$defs" && -r "$defs" ]]; then
    value="$(awk '$1 == "UID_MIN" && $2 ~ /^[0-9]+$/ { v = $2 } END { print v }' "$defs" 2>/dev/null)" || value=""
  fi
  if [[ ! "$value" =~ ^[0-9]{1,9}$ ]] || ((10#$value < 1)); then
    value=1000
  fi
  printf '%s' "$((10#$value))"
}

_tui_timestamp_valid() {  # <timestamp>
  [[ "$1" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]]
}

# The master token lands in JSON and in a curl config line, so its charset
# must exclude quotes and backslashes. Length is checked in bash because some
# regex engines cap {m,n} at 255.
_tui_master_token_valid() {  # <token>
  local token="$1"
  ((${#token} >= 16 && ${#token} <= 512)) && [[ "$token" =~ ^[A-Za-z0-9._~+/=-]+$ ]]
}

# Byte count of a string, whatever the locale.
_tui_byte_length() {  # <string>
  local LC_ALL=C
  printf '%s' "${#1}"
}

# The four decimal bytes of a canonical dotted quad (no leading zeros, every
# part 0-255), or status 1. curl reads a leading zero as octal, so such
# spellings are not decoded here at all.
_tui_ipv4_bytes() {  # <a.b.c.d>
  local part
  local -a parts=() out=()
  [[ "$1" =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ ]] || return 1
  IFS=. read -r -a parts <<<"$1"
  for part in "${parts[@]}"; do
    [[ "$part" =~ ^(0|[1-9][0-9]*)$ ]] && ((10#$part <= 255)) || return 1
    out+=("$((10#$part))")
  done
  printf '%s' "${out[*]}"
}

# The 16-bit groups (decimal) of one side of an IPv6 "::", or status 1. Only
# the last group of the whole address may be a dotted quad (two groups).
_tui_ipv6_groups() {  # <colon-separated groups> <IPv4 tail allowed: true|false>
  local text="$1" v4_tail="$2" i last bytes
  local -a parts=() out=() quad=()
  [[ -n "$text" ]] || return 0
  [[ "$text" != :* && "$text" != *: && "$text" != *::* ]] || return 1
  IFS=: read -r -a parts <<<"$text"
  last=$((${#parts[@]} - 1))
  for i in "${!parts[@]}"; do
    if [[ "${parts[i]}" == *.* ]]; then
      [[ "$v4_tail" == true ]] && ((i == last)) || return 1
      bytes="$(_tui_ipv4_bytes "${parts[i]}")" || return 1
      read -r -a quad <<<"$bytes"
      out+=("$((quad[0] * 256 + quad[1]))" "$((quad[2] * 256 + quad[3]))")
    else
      [[ "${parts[i]}" =~ ^[0-9a-f]{1,4}$ ]] || return 1
      out+=("$((16#${parts[i]}))")
    fi
  done
  printf '%s' "${out[*]}"
}

# The 16 decimal bytes of an IPv6 literal (no brackets, no zone id), or
# status 1.
_tui_ipv6_bytes() {  # <IPv6 literal>
  local text="${1,,}" left right groups g missing
  local -a lead_groups=() trail_groups=() all=() bytes=()
  [[ "$text" =~ ^[0-9a-f:.]{2,45}$ ]] || return 1
  if [[ "$text" == *::* ]]; then
    left="${text%%::*}" right="${text#*::}"
    [[ "$right" != *::* ]] || return 1
    groups="$(_tui_ipv6_groups "$left" false)" || return 1
    read -r -a lead_groups <<<"$groups"
    groups="$(_tui_ipv6_groups "$right" true)" || return 1
    read -r -a trail_groups <<<"$groups"
    missing=$((8 - ${#lead_groups[@]} - ${#trail_groups[@]}))
    ((missing >= 1)) || return 1
    all=("${lead_groups[@]}")
    for ((g = 0; g < missing; g++)); do all+=(0); done
    all+=("${trail_groups[@]}")
  else
    groups="$(_tui_ipv6_groups "$text" true)" || return 1
    read -r -a all <<<"$groups"
    ((${#all[@]} == 8)) || return 1
  fi
  for g in "${all[@]}"; do
    bytes+=("$((g >> 8))" "$((g & 255))")
  done
  printf '%s' "${bytes[*]}"
}

# 127.0.0.0/8 as a canonical dotted quad, or [::1]. Other spellings do not
# count as loopback, so the token never goes to a host that the listener
# check might decode differently from curl.
_tui_host_is_loopback() {  # <url host, IPv6 bracketed>
  local bytes
  [[ "$1" != "[::1]" ]] || return 0
  bytes="$(_tui_ipv4_bytes "$1")" || return 1
  [[ "$bytes" == "127 "* ]]
}

# ── Desktop user ─────────────────────────────────────────────────────────────

# Sets TUI_USER_HOME and TUI_USER_UID. Returns 2 when the account does not
# exist. In revoke mode it returns 3, with TUI_USER_HOME_ERROR set, when the
# home is not an absolute directory, is path-unsafe, is missing or is owned by
# another uid: revocation must not depend on the state of the desktop home
# (design 9.4). Every other refusal dies.
_tui_desktop_user_resolve() {  # <user> [grant|revoke]
  local user="$1" mode="${2:-grant}" entry pw_name pw_uid pw_home uid_min owner problem="" shown
  TUI_USER_HOME="" TUI_USER_UID="" TUI_USER_HOME_ERROR=""
  [[ "$user" =~ ^[a-z_][a-z0-9_-]{0,31}$ ]] \
    || die "invalid --for-user '$(sanitize_display_value "$user" 64)' (want ^[a-z_][a-z0-9_-]{0,31}\$)"
  entry="$(getent passwd "$user" 2>/dev/null)" || return 2
  entry="${entry%%$'\n'*}"
  IFS=: read -r pw_name _ pw_uid _ _ pw_home _ <<<"$entry"
  [[ "$pw_name" == "$user" && "$pw_uid" =~ ^[0-9]{1,10}$ ]] \
    || die "unexpected passwd entry for '$user'"
  pw_uid="$((10#$pw_uid))"
  ((pw_uid != 0)) || die "refusing --for-user '$user': uid 0 never receives tenant access"
  uid_min="$(_tui_uid_min)"
  ((pw_uid >= uid_min)) \
    || die "refusing --for-user '$user': uid $pw_uid is below UID_MIN $uid_min (a system account)"
  if tenant_exists_in_registry "$user"; then
    die "refusing --for-user '$user': it is a registered tenant (tenants reach their own daemon over its socket)"
  fi
  shown="$(sanitize_display_value "$pw_home" 512)"
  if [[ "$pw_home" != /* || "$pw_home" == / ]]; then
    problem="home '$shown' is not an absolute directory"
  elif ! darkirc_path_components_safe "$pw_home"; then
    problem="home $shown has a symlinked, empty or dot component"
  elif [[ ! -d "$pw_home" ]]; then
    problem="home $shown does not exist"
  else
    owner="$(stat -c %u "$pw_home" 2>/dev/null)" || owner=""
    [[ "$owner" == "$pw_uid" ]] || problem="home $shown is not owned by uid $pw_uid"
  fi
  if [[ -n "$problem" ]]; then
    if [[ "$mode" == revoke ]]; then
      TUI_USER_HOME_ERROR="$problem"
      return 3
    fi
    die "refusing --for-user '$user': $problem"
  fi
  TUI_USER_HOME="$pw_home"
  # shellcheck disable=SC2034  # part of the resolver's result contract (design 9.3)
  TUI_USER_UID="$pw_uid"
}

# ── Tenant gateway and env ───────────────────────────────────────────────────

# Maps a GATEWAY_HOST (or --url-host) value to the host part of the URL.
_tui_url_host() {  # <host>
  local host="${1,,}" bare
  case "$host" in
    "" | 0.0.0.0 | 127.0.0.1 | localhost | :: | "[::]")
      printf '127.0.0.1'
      return 0
      ;;
    ::1 | "[::1]")
      printf '[::1]'
      return 0
      ;;
  esac
  bare="$host"
  if [[ "${bare:0:1}" == "[" && "${bare: -1}" == "]" ]]; then
    bare="${bare:1:${#bare}-2}"
  fi
  if [[ "$bare" == *:* ]]; then
    # IPv6 literal, optionally with an embedded IPv4 tail; zone ids refused.
    [[ "$bare" =~ ^[0-9a-f:.]{2,45}$ ]] || return 1
    printf '[%s]' "$bare"
    return 0
  fi
  [[ "$host" =~ ^[a-z0-9.-]{1,253}$ ]] || return 1
  printf '%s' "$host"
}

# Sets TUI_GATEWAY_URL (http://<host>:<port>), TUI_GATEWAY_HOST,
# TUI_GATEWAY_PORT and TUI_GATEWAY_LOOPBACK. The port comes from the ports
# registry, the bind host from the tenant env (read as the tenant) unless
# --url-host overrides it.
_tui_gateway_url() {  # <tenant> <env_path> <override_host>
  local tenant="$1" env_path="$2" override="$3" enabled raw host port url_re
  TUI_GATEWAY_URL="" TUI_GATEWAY_HOST="" TUI_GATEWAY_PORT="" TUI_GATEWAY_LOOPBACK=false
  enabled="$(read_tenant_env_value "$tenant" "$env_path" GATEWAY_ENABLED)" \
    || die "could not read GATEWAY_ENABLED for tenant '$tenant' (tenant-side read of $env_path refused)"
  # The daemon's parse_bool_env vocabulary; unset keeps the default (enabled).
  case "${enabled,,}" in
    "" | true | 1) ;;
    false | 0) die "the gateway of tenant '$tenant' is disabled (GATEWAY_ENABLED=$enabled); the TUI reaches tenants only through the gateway" ;;
    *) die "GATEWAY_ENABLED of tenant '$tenant' is '$(sanitize_display_value "$enabled")', not true or false" ;;
  esac
  port="$(ports_get "$tenant" gateway)" || die "tenant '$tenant' has no gateway port in the ports registry"
  if [[ ! "$port" =~ ^[0-9]{1,5}$ ]] || ((10#$port < 1 || 10#$port > 65535)); then
    die "invalid gateway port for tenant '$tenant' in the ports registry"
  fi
  port="$((10#$port))"
  if [[ -n "$override" ]]; then
    raw="$override"
  else
    raw="$(read_tenant_env_value "$tenant" "$env_path" GATEWAY_HOST)" \
      || die "could not read GATEWAY_HOST for tenant '$tenant' (tenant-side read of $env_path refused)"
  fi
  host="$(_tui_url_host "$raw")" \
    || die "unsupported gateway host '$(sanitize_display_value "$raw")' for tenant '$tenant' (want IPv4, an IPv6 literal or a [A-Za-z0-9.-] hostname; --url-host overrides)"
  url_re='^http://[]a-z0-9.:[-]+:[0-9]{1,5}$'
  [[ "http://$host:$port" =~ $url_re ]] || die "internal error: unexpected gateway URL shape for tenant '$tenant'"
  TUI_GATEWAY_URL="http://$host:$port"
  TUI_GATEWAY_HOST="$host"
  TUI_GATEWAY_PORT="$port"
  if _tui_host_is_loopback "$host"; then
    TUI_GATEWAY_LOOPBACK=true
  fi
}

# SEC-10: client-token mode only ever touches the one file a daemon reads by
# default, inside the sandbox-protected state directory. Sets
# TUI_CLIENT_TOKENS_FILE (always <state>/gateway-client-tokens.json) and
# TUI_REGISTRY_PATH_ERROR. Returns 0 (usable), 3 (GATEWAY_CLIENT_TOKENS_FILE
# is none: client tokens disabled) or 4 (refused; reason in the error).
_tui_client_tokens_path() {  # <tenant> <env_path>
  local tenant="$1" env_path="$2" state base configured
  state="$(tenant_state_dir "$tenant")"
  TUI_CLIENT_TOKENS_FILE="$state/gateway-client-tokens.json"
  TUI_REGISTRY_PATH_ERROR=""
  base="$(read_tenant_env_value "$tenant" "$env_path" LUNARWING_BASE_DIR)" \
    || die "could not read LUNARWING_BASE_DIR for tenant '$tenant' (tenant-side read of $env_path refused)"
  configured="$(read_tenant_env_value "$tenant" "$env_path" GATEWAY_CLIENT_TOKENS_FILE)" \
    || die "could not read GATEWAY_CLIENT_TOKENS_FILE for tenant '$tenant' (tenant-side read of $env_path refused)"
  if [[ "$base" != "$state" ]]; then
    TUI_REGISTRY_PATH_ERROR="LUNARWING_BASE_DIR of tenant '$tenant' is '$(sanitize_display_value "$base")', not $state, so its daemon would look for client tokens elsewhere"
    return 4
  fi
  case "$configured" in
    "" | "$TUI_CLIENT_TOKENS_FILE") return 0 ;;
    none) return 3 ;;
  esac
  TUI_REGISTRY_PATH_ERROR="GATEWAY_CLIENT_TOKENS_FILE must be unset, none, or $TUI_CLIENT_TOKENS_FILE (tenant '$tenant' sets '$(sanitize_display_value "$configured")')"
  return 4
}

# ── Tenant-side client-token registry ────────────────────────────────────────

# The tenant-side half of every read of an existing registry: copies the file
# to stdout only if it is what the daemon would load (design 8.3.5): a regular
# file with a single link, owned by the tenant, without group or other access,
# of at most 64 KiB. A FIFO or device is refused before it is opened, so it
# cannot block the verb under the lifecycle lock; the checks are repeated on
# the opened descriptor, so a path swapped in between cannot pass them; head
# caps the bytes should the file grow after the size check, and timeout ends
# a read that blocks anyway. Exits 73 (not such a file) or 74 (too large).
_tui_registry_read() {  # <tenant> <file>
  local tenant="$1" file="$2"
  local -a drop=(sudo -u "$tenant")
  [[ "$(id -un)" != "$tenant" ]] || drop=()
  "${drop[@]}" env -i PATH="/usr/bin:/bin" TARGET_PATH="$file" MAX_BYTES="$TUI_ACCESS_MAX_REGISTRY_BYTES" \
    timeout -k 2 "$TUI_ACCESS_READ_TIMEOUT" bash --norc --noprofile -c '
      set -euo pipefail
      target="$TARGET_PATH"
      [[ -f "$target" && ! -L "$target" ]] || exit 73
      exec 3<"$target"
      [[ -f /dev/fd/3 ]] || exit 73
      meta="$(stat -L -c "%u %h %a %s" /dev/fd/3 2>/dev/null)" || exit 73
      read -r owner links mode size _ <<<"$meta"
      [[ "$owner" == "$(id -u)" && "$links" == 1 && "$mode" =~ ^[0-7]+$ && "$size" =~ ^[0-9]+$ ]] || exit 73
      (( (8#$mode & 077) == 0 )) || exit 73
      (( 10#$size <= MAX_BYTES )) || exit 74
      exec head -c "$((MAX_BYTES + 1))" <&3
    '
}

# Runs jq as the tenant over the registry: an existing file only through
# _tui_registry_read, an absent one as the empty registry. Sets
# TUI_REGISTRY_OUT, and TUI_REGISTRY_READ_STATUS to ok, not-private,
# too-large, timeout or unreadable. Returns 1 unless ok. Neither process's
# stderr is shown: the file is tenant-written.
_tui_registry_jq() {  # <tenant> <file> <jq arguments...>
  local tenant="$1" file="$2" rc=0
  shift 2
  TUI_REGISTRY_OUT="" TUI_REGISTRY_READ_STATUS=ok
  if [[ -e "$file" || -L "$file" ]]; then
    TUI_REGISTRY_OUT="$(
      set -o pipefail
      _tui_registry_read "$tenant" "$file" 2>/dev/null | tenant_jq "$tenant" "$@" 2>/dev/null
    )" || rc=$?
  else
    TUI_REGISTRY_OUT="$(tenant_jq "$tenant" "$@" <<<"$TUI_ACCESS_EMPTY_REGISTRY" 2>/dev/null)" || rc=$?
  fi
  case "$rc" in
    0) return 0 ;;
    73) TUI_REGISTRY_READ_STATUS=not-private ;;
    74) TUI_REGISTRY_READ_STATUS=too-large ;;
    124 | 137) TUI_REGISTRY_READ_STATUS=timeout ;;
    *) TUI_REGISTRY_READ_STATUS=unreadable ;;
  esac
  TUI_REGISTRY_OUT=""
  return 1
}

# Runs a registry jq body as the tenant over the file, or over the empty
# registry when the file is absent. The body prints a status line and, for
# "ok", the new document as compact JSON. --slurp plus the length guard
# refuses files with zero or several JSON documents, whose per-document
# outputs would otherwise be concatenated into the rewrite. Sets
# TUI_REGISTRY_STATUS and TUI_REGISTRY_DOC.
_tui_registry_eval() {  # <tenant> <file> <jq body> [jq args...]
  local tenant="$1" file="$2" program out
  program="$TUI_ACCESS_JQ_REASON"'
    if length != 1 then "documents" else .[0] | ('"$3"') end'
  shift 3
  TUI_REGISTRY_STATUS="" TUI_REGISTRY_DOC=""
  if ! _tui_registry_jq "$tenant" "$file" -s -r "$@" "$program"; then
    TUI_REGISTRY_STATUS="$TUI_REGISTRY_READ_STATUS"
    return 0
  fi
  out="$TUI_REGISTRY_OUT"
  TUI_REGISTRY_STATUS="${out%%$'\n'*}"
  if [[ "$out" == *$'\n'* ]]; then
    TUI_REGISTRY_DOC="${out#*$'\n'}"
  fi
  if [[ "$TUI_REGISTRY_STATUS" == ok && ( "$TUI_REGISTRY_DOC" == *$'\n'* || "$TUI_REGISTRY_DOC" != '{'* ) ]]; then
    TUI_REGISTRY_STATUS=unreadable TUI_REGISTRY_DOC=""
  fi
}

_tui_registry_reason_text() {  # <status>
  local shape='(want {"version":1,"tokens":[...]})'
  case "$1" in
    documents) printf '%s' "it does not hold exactly one JSON document $shape" ;;
    not-object) printf '%s' "it is not a JSON object $shape" ;;
    version) printf '%s' "its version is not 1 $shape" ;;
    tokens) printf '%s' "its tokens field is not an array $shape" ;;
    entry) printf '%s' "a tokens entry is not an object $shape" ;;
    too-many) printf '%s' "it would hold more than $TUI_ACCESS_MAX_CLIENT_TOKENS entries" ;;
    not-private) printf '%s' "it is not a private regular file of the tenant (a symlink, FIFO, device, hard link, foreign owner or group or other access)" ;;
    too-large) printf '%s' "it is larger than $TUI_ACCESS_MAX_REGISTRY_BYTES bytes, the daemon's limit" ;;
    too-large-result) printf '%s' "the result would be larger than $TUI_ACCESS_MAX_REGISTRY_BYTES bytes, the daemon's limit" ;;
    timeout) printf '%s' "the tenant-side read did not finish within $TUI_ACCESS_READ_TIMEOUT s" ;;
    *) printf '%s' "the tenant cannot read it as JSON $shape" ;;
  esac
}

_tui_registry_refuse() {  # <file> <status>
  die "refusing to rewrite $1: $(_tui_registry_reason_text "$2"); the file was left unchanged"
}

# Plans the replacement of <label>'s entry. Dies (without writing anything)
# unless the result is a valid registry of at most 32 entries that stays
# within the daemon's size limit (the writer adds one newline).
_tui_registry_plan_upsert() {  # <tenant> <file> <label> <sha256> <now> <expires|"">
  local tenant="$1" file="$2"
  # jq 1.6 (Debian 12) reserves `label`, so the jq variable is $want_label.
  _tui_registry_eval "$tenant" "$file" '
    reason as $r
    | if $r != "ok" then $r
      else
        (.tokens |= (map(select(.label != $want_label))
          + [{"label": $want_label, "sha256": $sha, "created_at": $now}
             + (if $exp == "" then {} else {"expires_at": $exp} end)])) as $next
        | if ($next.tokens | length) > $max then "too-many" else "ok", ($next | tojson) end
      end' \
    --arg want_label "$3" --arg sha "$4" --arg now "$5" --arg exp "$6" \
    --argjson max "$TUI_ACCESS_MAX_CLIENT_TOKENS"
  if [[ "$TUI_REGISTRY_STATUS" == ok ]] \
    && (($(_tui_byte_length "$TUI_REGISTRY_DOC") >= TUI_ACCESS_MAX_REGISTRY_BYTES)); then
    TUI_REGISTRY_STATUS=too-large-result
  fi
  if [[ "$TUI_REGISTRY_STATUS" != ok || -z "$TUI_REGISTRY_DOC" ]]; then
    _tui_registry_refuse "$file" "$TUI_REGISTRY_STATUS"
  fi
}

# Replaces the digest for <label> atomically (rotation keeps one entry per
# label). Returns the writer's status when the commit fails.
_tui_registry_upsert() {  # <tenant> <file> <label> <sha256> <now> <expires|"">
  _tui_registry_plan_upsert "$@"
  write_tenant_file_atomic "$1" "$2" 600 owned-parent <<<"$TUI_REGISTRY_DOC"
}

# Removes <label>'s entry. Sets TUI_REGISTRY_RESULT to removed or absent; an
# absent file or label is a no-op and leaves the file untouched. Dies on a
# refused or failed rewrite; callers that must go on run it through
# _tui_registry_drop_label.
_tui_registry_remove() {  # <tenant> <file> <label>
  local tenant="$1" file="$2" label="$3" rc=0
  TUI_REGISTRY_RESULT=absent
  [[ -e "$file" || -L "$file" ]] || return 0
  _tui_registry_eval "$tenant" "$file" '
    reason as $r
    | if $r != "ok" then $r
      elif any(.tokens[]; .label == $want_label)
      then "ok", ((.tokens |= map(select(.label != $want_label))) | tojson)
      else "absent" end' \
    --arg want_label "$label"
  case "$TUI_REGISTRY_STATUS" in
    absent) return 0 ;;
    ok) [[ -n "$TUI_REGISTRY_DOC" ]] || _tui_registry_refuse "$file" unreadable ;;
    *) _tui_registry_refuse "$file" "$TUI_REGISTRY_STATUS" ;;
  esac
  write_tenant_file_atomic "$tenant" "$file" 600 owned-parent <<<"$TUI_REGISTRY_DOC" || rc=$?
  ((rc == 0)) \
    || die "could not rewrite $file as tenant '$tenant' (tenant-side writer exit $rc); '$label' is still registered"
  TUI_REGISTRY_RESULT=removed
}

# The tenant-side step of --revoke and of a master-token grant: removes
# <label> from the registry the daemon reads, also with
# GATEWAY_CLIENT_TOKENS_FILE=none (a stale entry would come back to life once
# client tokens are enabled again). Callers run it in a command substitution,
# so a refusal (die, reported on stderr) ends only this step and the desktop
# side still runs. Prints "<tokens_rc> <removed|absent|skipped>".
_tui_registry_drop_label() {  # <tenant> <env_path> <label>
  local tenant="$1" env_path="$2" label="$3" tokens_rc=0
  _tui_client_tokens_path "$tenant" "$env_path" || tokens_rc=$?
  if ((tokens_rc == 4)); then
    say "WARNING: $TUI_REGISTRY_PATH_ERROR; the tenant-side registry was not touched" >&2
    printf '4 skipped\n'
    return 0
  fi
  _tui_registry_remove "$tenant" "$TUI_CLIENT_TOKENS_FILE" "$label"
  printf '%s %s\n' "$tokens_rc" "$TUI_REGISTRY_RESULT"
}

# Runs _tui_registry_drop_label and sets TUI_CLIENT_TOKENS_FILE,
# TUI_REGISTRY_TOKENS_RC and TUI_REGISTRY_RESULT (removed, absent, skipped,
# or failed when the step died).
_tui_registry_drop_label_step() {  # <tenant> <env_path> <label>
  local step=""
  TUI_CLIENT_TOKENS_FILE="$(tenant_state_dir "$1")/gateway-client-tokens.json"
  TUI_REGISTRY_TOKENS_RC=0 TUI_REGISTRY_RESULT=failed
  step="$(_tui_registry_drop_label "$@")" || step=""
  if [[ "$step" =~ ^([0-9])\ (removed|absent|skipped)$ ]]; then
    TUI_REGISTRY_TOKENS_RC="${BASH_REMATCH[1]}" TUI_REGISTRY_RESULT="${BASH_REMATCH[2]}"
  fi
}

_tui_list_row_valid() {  # <label> <created> <expires>
  [[ "$1" =~ ^[a-z0-9][a-z0-9:._-]{0,63}$ ]] || return 1
  [[ "$2" == - ]] || _tui_timestamp_valid "$2" || return 1
  [[ "$3" == - ]] || _tui_timestamp_valid "$3"
}

# SEC-13: prints labels and timestamps that pass validation (in jq and again
# here); every other entry prints as <invalid entry #n>. Never prints digests.
_tui_registry_list() {  # <tenant>
  local tenant="$1" env_path rc=0 file out line n label created expires extra rows=0
  env_path="$(tenant_env_dir "$tenant")/lunarwing.env"
  _tui_client_tokens_path "$tenant" "$env_path" || rc=$?
  ((rc == 0 || rc == 3)) || die "$TUI_REGISTRY_PATH_ERROR; cannot locate the client-token registry"
  file="$TUI_CLIENT_TOKENS_FILE"
  if [[ ! -e "$file" && ! -L "$file" ]]; then
    say "tui-access: no client tokens are registered for tenant '$tenant'"
  else
    if ! _tui_registry_jq "$tenant" "$file" -s -r --argjson max "$TUI_ACCESS_MAX_CLIENT_TOKENS" "$TUI_ACCESS_JQ_LIST"; then
      [[ "$TUI_REGISTRY_READ_STATUS" != unreadable ]] \
        || die "could not read $file as tenant '$tenant' (unreadable or not JSON)"
      die "refusing to list $file: $(_tui_registry_reason_text "$TUI_REGISTRY_READ_STATUS")"
    fi
    out="$TUI_REGISTRY_OUT"
    if [[ "$out" == '!'* ]]; then
      line="${out%%$'\n'*}"
      die "refusing to list $file: not a version-1 client-token registry (${line#!})"
    fi
    say "tui-access: client tokens registered for tenant '$tenant' in $file"
    printf '  %-34s %-21s %s\n' LABEL CREATED EXPIRES
    while IFS= read -r line; do
      [[ -n "$line" ]] || continue
      rows=$((rows + 1))
      if [[ "$line" =~ ^\+([0-9]+)$ ]]; then
        say "  ... ${BASH_REMATCH[1]} more entries not shown (the daemon refuses registries of more than $TUI_ACCESS_MAX_CLIENT_TOKENS)"
        continue
      fi
      n="" label="" created="" expires="" extra=""
      IFS=$'\t' read -r n label created expires extra <<<"$line"
      [[ "$n" =~ ^[0-9]+$ ]] || n="?"
      if [[ -z "$extra" ]] && _tui_list_row_valid "$label" "$created" "$expires"; then
        printf '  %-34s %-21s %s\n' "$label" "$created" "$expires"
      else
        say "  <invalid entry #$n>"
      fi
    done <<<"$out"
    ((rows > 0)) || say "  (no entries)"
  fi
  if ((rc == 3)); then
    say "note: client tokens are disabled for tenant '$tenant' (GATEWAY_CLIENT_TOKENS_FILE=none); its daemon ignores this registry"
  fi
  say "note: --use-master-token grants are not registered tenant-side and are not listed"
}

# ── Desktop access file ──────────────────────────────────────────────────────

# Creates both directories as the user, tightens them to 0700 and commits the
# file through the user-side atomic writer (private parent policy). Sets
# TUI_DESKTOP_FILE.
_tui_desktop_config_write() {  # <user> <home> <tenant> <json>
  local user="$1" home="$2" tenant="$3" json="$4" base dir rc=0
  base="$home/.config/wingtip"
  dir="$base/tenants"
  TUI_DESKTOP_FILE="$dir/$tenant.json"
  tenant_mkdir_p "$user" "$dir" || die "could not create $dir as user '$user'"
  tenant_chmod "$user" 700 "$base" || die "could not chmod 0700 $base as user '$user'"
  tenant_chmod "$user" 700 "$dir" || die "could not chmod 0700 $dir as user '$user'"
  write_tenant_file_atomic "$user" "$TUI_DESKTOP_FILE" 600 private <<<"$json" || rc=$?
  ((rc == 0)) \
    || die "refusing to write $TUI_DESKTOP_FILE: the writer running as '$user' refused it (exit $rc: a symlink, non-regular file, foreign owner, hard link or unsafe parent); the tenant-side registry was not changed"
}

# Sets TUI_DESKTOP_FILE, TUI_DESKTOP_RESULT (removed|absent) and
# TUI_DESKTOP_KIND (unknown when present, empty when absent). The user's file is
# never opened: its kind only selects warning wording, and reading it could
# block revoke on a FIFO or oversized file while the lifecycle lock is held.
_tui_desktop_config_remove() {  # <user> <home> <tenant>
  local user="$1" home="$2" tenant="$3" dir
  dir="$home/.config/wingtip/tenants"
  TUI_DESKTOP_FILE="$dir/$tenant.json"
  TUI_DESKTOP_RESULT=absent TUI_DESKTOP_KIND=""
  darkirc_path_components_safe "$dir" \
    || die "unsafe desktop config path $dir (a symlinked component); remove $TUI_DESKTOP_FILE by hand"
  [[ -e "$TUI_DESKTOP_FILE" || -L "$TUI_DESKTOP_FILE" ]] || return 0
  _tui_as_user "$user" rm -f -- "$TUI_DESKTOP_FILE" \
    || die "could not remove $TUI_DESKTOP_FILE as user '$user'"
  TUI_DESKTOP_RESULT=removed
  TUI_DESKTOP_KIND=unknown
}

# ── Listener owner and probe ─────────────────────────────────────────────────

# "little" or "big"; status 1 when od cannot tell.
_tui_byte_order() {
  local probe
  probe="$(printf '\001\000' | od -An -tx2 2>/dev/null)" || return 1
  probe="${probe//[[:space:]]/}"
  case "$probe" in
    0001) printf 'little' ;;
    0100) printf 'big' ;;
    *) return 1 ;;
  esac
}

# The /proc/net/tcp{,6} spelling of an address given as 4 or 16 decimal
# bytes: the kernel prints each 32-bit word with %08X in host byte order, so
# 127.0.0.1 reads 0100007F and ::1 00000000000000000000000001000000 on
# little-endian hosts.
_tui_proc_hex() {  # <byte>...
  local order out="" i
  local -a b=("$@")
  ((${#b[@]} == 4 || ${#b[@]} == 16)) || return 1
  order="$(_tui_byte_order)" || return 1
  for ((i = 0; i < ${#b[@]}; i += 4)); do
    if [[ "$order" == little ]]; then
      printf -v out '%s%02X%02X%02X%02X' "$out" "${b[i + 3]}" "${b[i + 2]}" "${b[i + 1]}" "${b[i]}"
    else
      printf -v out '%s%02X%02X%02X%02X' "$out" "${b[i]}" "${b[i + 1]}" "${b[i + 2]}" "${b[i + 3]}"
    fi
  done
  printf '%s' "$out"
}

# Which listeners a connection to <url host> reaches, as tui/src/transport/
# listener-owner.ts matches them (design 3.8.1): an IPv4 host reaches IPv4
# sockets on its address or 0.0.0.0 and IPv6 sockets on its v4-mapped form or
# :: (dual-stack); an IPv6 host reaches IPv6 sockets on its address or ::; a
# v4-mapped IPv6 host is an IPv4 connection. Sets TUI_REACH_V4 and
# TUI_REACH_V6 (space-separated /proc spellings) and TUI_REACH_ANY (true for a
# DNS name, whose address is not known here: every listener on the port
# counts). Returns 1 when the host cannot be matched at all, including IPv4
# spellings other than a canonical dotted quad (a last label that is a number
# or 0x hex), which curl decodes as an address of its own.
_tui_listener_targets() {  # <url host, IPv6 bracketed>
  local host="${1,,}" bytes hex last
  local -a b=()
  TUI_REACH_V4="" TUI_REACH_V6="" TUI_REACH_ANY=false
  last="${host%.}"
  last="${last##*.}"
  if [[ "${host:0:1}" == "[" && "${host: -1}" == "]" ]]; then
    bytes="$(_tui_ipv6_bytes "${host:1:${#host}-2}")" || return 1
    read -r -a b <<<"$bytes"
    if [[ "${b[*]:0:12}" != "0 0 0 0 0 0 0 0 0 0 255 255" ]]; then
      hex="$(_tui_proc_hex "${b[@]}")" || return 1
      TUI_REACH_V6="$hex $TUI_ACCESS_PROC_ANY6"
      return 0
    fi
    b=("${b[@]:12:4}")
  elif bytes="$(_tui_ipv4_bytes "$host")"; then
    read -r -a b <<<"$bytes"
  elif [[ "$host" =~ ^[a-z0-9.-]{1,253}$ && ! "$last" =~ ^([0-9]+|0x[0-9a-f]*)$ ]]; then
    TUI_REACH_ANY=true
    return 0
  else
    return 1
  fi
  hex="$(_tui_proc_hex "${b[@]}")" || return 1
  TUI_REACH_V4="$hex 00000000"
  hex="$(_tui_proc_hex 0 0 0 0 0 0 0 0 0 0 255 255 "${b[@]}")" || return 1
  TUI_REACH_V6="$hex $TUI_ACCESS_PROC_ANY6"
}

# SEC-01: who holds <port> for a connection to <url host>, from /proc/net/tcp
# and tcp6 (column 2 local address:port in hex, column 4 state with 0A =
# LISTEN, column 8 uid). Prints "none" (nothing listens on the port),
# "conflict" (several uids hold the port, on any address), "uncovered" (the
# port is held, but no listener on it covers <url host>) or the single owning
# uid. A host that cannot be matched counts as uncovered. TUI_PROC_NET_DIR
# exists for the shell test.
_tui_port_owner_uid() {  # <port> <url host, IPv6 bracketed>
  local port="$1" host="${2:-}" dir="${TUI_PROC_NET_DIR:-/proc/net}" hex any=0
  local -a files=()
  if [[ ! "$port" =~ ^[0-9]{1,5}$ ]] || ((10#$port < 1 || 10#$port > 65535)); then
    printf 'none\n'
    return 0
  fi
  printf -v hex '%04X' "$((10#$port))"
  if ! _tui_listener_targets "$host"; then
    TUI_REACH_V4="" TUI_REACH_V6=""
  elif [[ "$TUI_REACH_ANY" == true ]]; then
    any=1
  fi
  if [[ -r "$dir/tcp" ]]; then files+=("$dir/tcp"); fi
  if [[ -r "$dir/tcp6" ]]; then files+=("$dir/tcp6"); fi
  if ((${#files[@]} == 0)); then
    printf 'none\n'
    return 0
  fi
  awk -v want="$hex" -v any="$any" -v v4="$TUI_REACH_V4" -v v6="$TUI_REACH_V6" '
    BEGIN {
      n = split(v4, list, " "); for (i = 1; i <= n; i++) reach4[list[i]] = 1
      n = split(v6, list, " "); for (i = 1; i <= n; i++) reach6[list[i]] = 1
    }
    $4 == "0A" && $8 ~ /^[0-9]+$/ {
      if (split(toupper($2), local_addr, ":") != 2 || local_addr[2] != want) next
      held[$8] = 1
      a = local_addr[1]
      if (any == 1 || (length(a) == 8 && (a in reach4)) || (length(a) == 32 && (a in reach6))) covers[$8] = 1
    }
    END {
      n = 0
      for (uid in held) { n++; owner = uid }
      c = 0
      for (uid in covers) c++
      if (n == 0) print "none"
      else if (n > 1) print "conflict"
      else if (c == 0) print "uncovered"
      else print owner
    }' "${files[@]}" 2>/dev/null || printf 'none\n'
}

_tui_owner_warning() {  # <port> <owner> <tenant> <tenant_uid> <url host>
  case "$2" in
    none) say "WARNING: nothing listens on gateway port $1 now, so its owner cannot be checked; skipping the authenticated probe" >&2 ;;
    conflict) say "WARNING: gateway port $1 is held by several uids, not only $3 ($4); skipping the authenticated probe" >&2 ;;
    uncovered) say "WARNING: no listener on gateway port $1 covers $5, the address the probe connects to; skipping the authenticated probe" >&2 ;;
    *) say "WARNING: gateway port $1 is held by uid $2, not $3 ($4); skipping the authenticated probe" >&2 ;;
  esac
}

# Prints the HTTP status of one GET (000 when nothing answered). -q keeps the
# invoking user's curlrc out, --noproxy keeps the token off any proxy, and
# --globoff keeps curl from reading [::1] as a URL glob.
_tui_http_code() {  # <url> [extra curl args...]
  local url="$1" code=""
  shift
  code="$(curl -q -s -g -o /dev/null -w '%{http_code}' --max-time 3 --noproxy '*' "$@" "$url" 2>/dev/null)" || true
  [[ "$code" =~ ^[0-9]{3}$ ]] || code=000
  printf '%s' "$code"
}

# Returns 0 (verified, or skipped with a warning) or 2 (token rejected).
_tui_gateway_probe() {  # <tenant> <url> <token>
  local tenant="$1" url="$2" token="$3" code hostport host port tenant_uid owner try last=""
  code="$(_tui_http_code "$url/api/health")"
  if [[ "$code" != 200 ]]; then
    say "  verify:  gateway not reachable now (GET /api/health -> $code); files kept, the token works once the daemon serves"
    return 0
  fi
  hostport="${url#http://}"
  host="${hostport%:*}"
  port="${hostport##*:}"
  if ! _tui_host_is_loopback "$host"; then
    say "  verify:  authenticated probe skipped (non-loopback URL; the token is not sent off-host)"
    return 0
  fi
  tenant_uid="$(id -u "$tenant" 2>/dev/null)" || tenant_uid=""
  for ((try = 1; try <= 4; try++)); do
    # Re-checked before every try: the listener can change during the ~8 s.
    owner="$(_tui_port_owner_uid "$port" "$host")"
    if [[ ! "$tenant_uid" =~ ^[1-9][0-9]*$ || "$owner" != "$tenant_uid" ]]; then
      _tui_owner_warning "$port" "$owner" "$tenant" "$tenant_uid" "$host"
      say "  verify:  skipped (listener owner check failed; the token was not sent)"
      return 0
    fi
    last="$(_tui_http_code "$url/api/gateway/status" \
      -K <(printf 'header = "Authorization: Bearer %s"\n' "$token"))"
    if [[ "$last" == 200 ]]; then
      say "  verify:  ok (GET /api/gateway/status -> 200)"
      return 0
    fi
    if ((try < 4)); then
      sleep 2
    fi
  done
  if [[ "$last" == 401 || "$last" == 403 ]]; then
    say "  verify:  rejected (GET /api/gateway/status -> $last after 4 tries over about 8 s)"
    return 2
  fi
  say "  verify:  inconclusive (GET /api/gateway/status -> $last after 4 tries); files kept"
  return 0
}

# Like _tui_http_code, but the body (at most 64 KiB) lands in <file>.
_tui_http_body() {  # <url> <file> [extra curl args...]
  local url="$1" file="$2" code=""
  shift 2
  code="$(curl -q -s -g -o "$file" -w '%{http_code}' --max-time 3 --max-filesize 65536 --noproxy '*' "$@" "$url" 2>/dev/null)" || true
  [[ "$code" =~ ^[0-9]{3}$ ]] || code=000
  printf '%s' "$code"
}

# Whether the running daemon authenticates client tokens (UZ-1): GET
# /api/chat/capabilities, sent with the master token under the same SEC-01
# rules as the probe, must list the `client_tokens` feature. Returns 0
# (supported), 2 (the daemon answered without it, or predates the route) or 3
# (undetermined: gateway down, non-loopback URL, listener owner check failed,
# master token unreadable); TUI_SUPPORT_REASON says why. The body never
# reaches stdout; the master token travels only in the curl -K config.
_tui_client_tokens_supported() {  # <tenant> <env_path> <url>
  local tenant="$1" env_path="$2" url="$3" code hostport host port tenant_uid owner token="" body features rc=3
  TUI_SUPPORT_REASON=""
  code="$(_tui_http_code "$url/api/health")"
  if [[ "$code" != 200 ]]; then
    TUI_SUPPORT_REASON="gateway not reachable now (GET /api/health -> $code)"
    return 3
  fi
  hostport="${url#http://}"
  host="${hostport%:*}"
  port="${hostport##*:}"
  if ! _tui_host_is_loopback "$host"; then
    TUI_SUPPORT_REASON="non-loopback URL; the master token is not sent off-host"
    return 3
  fi
  tenant_uid="$(id -u "$tenant" 2>/dev/null)" || tenant_uid=""
  owner="$(_tui_port_owner_uid "$port" "$host")"
  if [[ ! "$tenant_uid" =~ ^[1-9][0-9]*$ || "$owner" != "$tenant_uid" ]]; then
    _tui_owner_warning "$port" "$owner" "$tenant" "$tenant_uid" "$host"
    TUI_SUPPORT_REASON="listener owner check failed; the master token was not sent"
    return 3
  fi
  token="$(read_tenant_env_value "$tenant" "$env_path" GATEWAY_AUTH_TOKEN)" || token=""
  if ! _tui_master_token_valid "$token"; then
    token=""
    TUI_SUPPORT_REASON="GATEWAY_AUTH_TOKEN of the tenant could not be read or is malformed"
    return 3
  fi
  body="$(mktemp)" || { token=""; TUI_SUPPORT_REASON="could not create a temporary file"; return 3; }
  code="$(_tui_http_body "$url/api/chat/capabilities" "$body" \
    -K <(printf 'header = "Authorization: Bearer %s"\n' "$token"))"
  token=""
  case "$code" in
    200)
      features="$(jq -r '(.features // []) | map(select(type == "string")) | join(",")' "$body" 2>/dev/null)" || features=""
      if [[ ",$features," == *",client_tokens,"* ]]; then
        rc=0
      else
        TUI_SUPPORT_REASON="GET /api/chat/capabilities does not list the client_tokens feature"
        rc=2
      fi
      ;;
    404)
      TUI_SUPPORT_REASON="GET /api/chat/capabilities -> 404 (the daemon predates the chat contract)"
      rc=2
      ;;
    *)
      TUI_SUPPORT_REASON="GET /api/chat/capabilities -> $code"
      rc=3
      ;;
  esac
  rm -f -- "$body"
  return "$rc"
}

# ── Grant ────────────────────────────────────────────────────────────────────

# Sets TUI_ISSUED_AT and TUI_EXPIRES_AT (empty without --expires-in-days).
_tui_grant_timestamps() {  # <expires_days|"">
  local days="$1" now
  TUI_ISSUED_AT="" TUI_EXPIRES_AT=""
  now="$(date -u +%s)" || now=""
  [[ "$now" =~ ^[0-9]+$ ]] || die "could not read the clock"
  TUI_ISSUED_AT="$(date -u -d "@$now" "$TUI_ACCESS_TS_FORMAT")" || TUI_ISSUED_AT=""
  if [[ -n "$days" ]]; then
    TUI_EXPIRES_AT="$(date -u -d "@$((now + 10#$days * 86400))" "$TUI_ACCESS_TS_FORMAT")" || TUI_EXPIRES_AT="invalid"
  fi
  _tui_timestamp_valid "$TUI_ISSUED_AT" || die "could not format the issue timestamp"
  [[ -z "$TUI_EXPIRES_AT" ]] || _tui_timestamp_valid "$TUI_EXPIRES_AT" || die "could not format the expiry timestamp"
}

_tui_grant_report() {  # <tenant> <user> <kind> <label> <tenant_uid>
  local tenant="$1" user="$2" kind="$3" label="$4" tenant_uid="$5" owner
  say "tui-access: granted tenant '$tenant' to user '$user'"
  say "  url:     $TUI_GATEWAY_URL"
  say "  config:  $TUI_DESKTOP_FILE (0600, owner $user)"
  if [[ "$kind" == client ]]; then
    say "  token:   dedicated client token '$label' (SHA-256 registered tenant-side; plaintext only in the config file)"
  else
    say "  token:   the tenant master token (GATEWAY_AUTH_TOKEN, token_kind master); nothing registered tenant-side"
    if [[ "$TUI_REGISTRY_RESULT" == removed ]]; then
      say "  removed: earlier client token '$label' from $TUI_CLIENT_TOKENS_FILE (the daemon stops accepting it within about 2 s)"
    fi
  fi
  say "  expires: ${TUI_EXPIRES_AT:-never}"
  owner="$(_tui_port_owner_uid "$TUI_GATEWAY_PORT" "$TUI_GATEWAY_HOST")"
  case "$owner" in
    "$tenant_uid") say "  owner:   port $TUI_GATEWAY_PORT is held by uid $tenant_uid ($tenant); recorded as tenant_uid" ;;
    none) say "  owner:   nothing listens on port $TUI_GATEWAY_PORT now; recorded tenant_uid $tenant_uid (id -u $tenant)" ;;
    conflict) say "  owner:   port $TUI_GATEWAY_PORT is held by several uids; recorded tenant_uid $tenant_uid (id -u $tenant)" ;;
    uncovered) say "  owner:   no listener on port $TUI_GATEWAY_PORT covers $TUI_GATEWAY_HOST now; recorded tenant_uid $tenant_uid (id -u $tenant)" ;;
    *) say "  owner:   port $TUI_GATEWAY_PORT is held by uid $owner, not $tenant; recorded tenant_uid $tenant_uid (id -u $tenant)" ;;
  esac
}

_tui_rejected_guidance() {  # <tenant> <kind>
  if [[ "$2" == client ]]; then
    warn_err "the running daemon of tenant '$1' rejected the new client token after 4 probes. The files are kept: if the daemon was just upgraded or restarted, re-run the grant (it rotates the digest and re-checks client-token support); otherwise check the daemon log for the reload of $TUI_CLIENT_TOKENS_FILE. --use-master-token remains the stopgap (full gateway API access, revocable only by 'rotate-gateway-token $1')"
  else
    warn_err "the running daemon of tenant '$1' rejected GATEWAY_AUTH_TOKEN from its env file; it may still hold an older value. The files are kept: run 'restart-tenant $1', then re-run tui-access with --use-master-token"
  fi
}

_tui_access_grant() {  # <tenant> <user> <url_host> <expires_days> <use_master> <no_verify>
  local tenant="$1" user="$2" url_host="$3" expires_days="$4" use_master="$5" no_verify="$6"
  local label="tui:$user" env_path tenant_uid kind token="" digest="" json expires_json="" tokens_rc=0 probe_rc=0 support_rc=0
  TUI_REGISTRY_RESULT=""
  _tui_desktop_user_resolve "$user" || die "unknown user '$user' (no passwd entry)"
  tenant_uid="$(id -u "$tenant" 2>/dev/null)" || tenant_uid=""
  [[ "$tenant_uid" =~ ^[1-9][0-9]*$ ]] || die "tenant account '$tenant' does not exist or has no usable uid"
  env_path="$(tenant_env_dir "$tenant")/lunarwing.env"
  _tui_gateway_url "$tenant" "$env_path" "$url_host"
  if [[ "$TUI_GATEWAY_LOOPBACK" != true ]]; then
    say "WARNING: $TUI_GATEWAY_URL is not a loopback URL: the TUI refuses plain http to other hosts unless allow_insecure_http is set, and the token would cross the network in clear text" >&2
  fi
  _tui_grant_timestamps "$expires_days"

  if [[ "$use_master" == true ]]; then
    kind=master
    say "WARNING: --use-master-token copies the GATEWAY_AUTH_TOKEN of tenant '$tenant' into the desktop config: full gateway API access (settings, extensions, logs), a stopgap for daemons that predate client tokens. It stays valid until 'rotate-gateway-token $tenant', which restarts the tenant and invalidates every copy of the old token (web login, worker fallback, DarkIRC and Brave uploads included). Re-grant without --use-master-token once the tenant runs a binary with client-token support." >&2
    token="$(read_tenant_env_value "$tenant" "$env_path" GATEWAY_AUTH_TOKEN)" \
      || die "could not read GATEWAY_AUTH_TOKEN for tenant '$tenant' (tenant-side read of $env_path refused)"
    _tui_master_token_valid "$token" \
      || die "GATEWAY_AUTH_TOKEN of tenant '$tenant' is unset or not 16-512 characters of [A-Za-z0-9._~+/=-]; refusing to copy it"
  else
    kind=client
    _tui_client_tokens_path "$tenant" "$env_path" || tokens_rc=$?
    case "$tokens_rc" in
      0) ;;
      3) die "client tokens are disabled for tenant '$tenant' (GATEWAY_CLIENT_TOKENS_FILE=none); re-run with --use-master-token" ;;
      *) die "$TUI_REGISTRY_PATH_ERROR; refusing client-token mode (--use-master-token does not use the registry)" ;;
    esac
    # UZ-1: never register a client token that no running daemon authenticates.
    if [[ "$no_verify" == true ]]; then
      say "WARNING: --no-verify: not checking whether the daemon of tenant '$tenant' authenticates client tokens; the registered token becomes live only on a daemon that loads $TUI_CLIENT_TOKENS_FILE" >&2
    else
      _tui_client_tokens_supported "$tenant" "$env_path" "$TUI_GATEWAY_URL" || support_rc=$?
      case "$support_rc" in
        0) ;;
        2) die "the running daemon of tenant '$tenant' does not authenticate client tokens ($TUI_SUPPORT_REASON); nothing was written. Upgrade the tenant to a binary with client-token support first, or re-run with --use-master-token (a stopgap: full gateway API access, revocable only by 'rotate-gateway-token $tenant')" ;;
        *) say "  support: not checked ($TUI_SUPPORT_REASON); the registered token becomes live only on a daemon that loads $TUI_CLIENT_TOKENS_FILE" ;;
      esac
    fi
    token="$(generate_token)" || die "failed to generate a client token"
    digest="$(printf '%s' "$token" | sha256sum | cut -d' ' -f1)" || digest=""
    [[ "$digest" =~ ^[0-9a-f]{64}$ ]] || die "failed to digest the client token"
    # Validate the registry before anything is written.
    _tui_registry_plan_upsert "$tenant" "$TUI_CLIENT_TOKENS_FILE" "$label" "$digest" "$TUI_ISSUED_AT" "$TUI_EXPIRES_AT"
  fi

  if [[ -n "$TUI_EXPIRES_AT" ]]; then
    expires_json=",\"expires_at\":\"$TUI_EXPIRES_AT\""
  fi
  # printf -v is a builtin: the token never reaches argv. Every interpolated
  # value was validated as JSON-safe above.
  printf -v json '{"schema":"wingtip/tenant-access","version":1,"tenant":"%s","transport":"gateway","url":"%s","tenant_uid":%s,"token":"%s","token_kind":"%s","label":"%s","issued_at":"%s"%s}' \
    "$tenant" "$TUI_GATEWAY_URL" "$tenant_uid" "$token" "$kind" "$label" "$TUI_ISSUED_AT" "$expires_json"
  # The desktop file goes first: on rotation the new token is in place before
  # the old digest disappears, and a refused desktop path leaves the registry
  # untouched.
  _tui_desktop_config_write "$user" "$TUI_USER_HOME" "$tenant" "$json"
  json=""
  if [[ "$kind" == client ]]; then
    _tui_registry_upsert "$tenant" "$TUI_CLIENT_TOKENS_FILE" "$label" "$digest" "$TUI_ISSUED_AT" "$TUI_EXPIRES_AT" \
      || die "could not register '$label' in $TUI_CLIENT_TOKENS_FILE as tenant '$tenant' (tenant-side writer refused it); $TUI_DESKTOP_FILE already holds the new token, which stays inactive until a grant succeeds"
  else
    # An earlier client-token grant left a digest under the same label, which
    # daemons with client-token support would keep accepting although the
    # desktop file now holds the master token. Only a warning when that
    # fails: the master token does not use the registry.
    _tui_registry_drop_label_step "$tenant" "$env_path" "$label"
    if [[ "$TUI_REGISTRY_RESULT" == failed ]]; then
      say "WARNING: an earlier client token '$label' may still be registered for tenant '$tenant' (see the error above); fix that and re-run this grant" >&2
    fi
  fi

  _tui_grant_report "$tenant" "$user" "$kind" "$label" "$tenant_uid"
  if [[ "$no_verify" == true ]]; then
    say "  verify:  skipped (--no-verify)"
  else
    _tui_gateway_probe "$tenant" "$TUI_GATEWAY_URL" "$token" || probe_rc=$?
  fi
  token="" digest=""
  unset token json digest
  case "$probe_rc" in
    0) return 0 ;;
    2)
      _tui_rejected_guidance "$tenant" "$kind"
      return 2
      ;;
  esac
  die "internal error: unexpected probe status $probe_rc"
}

# ── Revoke ───────────────────────────────────────────────────────────────────

_tui_revoke_report() {  # <tenant> <user> <label> <desktop: checked|no-account|unusable>
  local tenant="$1" user="$2" label="$3" desktop="$4" master_note
  master_note="it stays valid until 'rotate-gateway-token $tenant', which restarts the tenant and invalidates every copy of the old token (web login, master-token TUI files, worker fallback, DarkIRC and Brave uploads)"
  if [[ "$TUI_REGISTRY_RESULT" == failed ]]; then
    say "tui-access: revocation of tenant '$tenant' for user '$user' is incomplete"
  elif [[ "$TUI_REGISTRY_RESULT" == removed || "$TUI_DESKTOP_RESULT" == removed ]]; then
    say "tui-access: revoked tenant '$tenant' for user '$user'"
  elif [[ "$desktop" == checked ]]; then
    say "tui-access: nothing to revoke for user '$user' on tenant '$tenant'"
  else
    say "tui-access: nothing to revoke tenant-side for user '$user' on tenant '$tenant'"
  fi
  case "$TUI_REGISTRY_RESULT" in
    removed) say "  registry: removed '$label' from $TUI_CLIENT_TOKENS_FILE (the daemon stops accepting it within about 2 s)" ;;
    skipped) say "  registry: not checked (see the warning above)" ;;
    failed) say "  registry: not changed (see the error above); '$label' may still be registered" ;;
    *) say "  registry: no '$label' entry" ;;
  esac
  case "$desktop" in
    checked)
      if [[ "$TUI_DESKTOP_RESULT" == removed ]]; then
        say "  config:   removed $TUI_DESKTOP_FILE (token_kind unknown; not read)"
      else
        say "  config:   no desktop config"
      fi
      ;;
    no-account) say "  config:   not checked (no passwd entry for '$user')" ;;
    *) say "  config:   not checked ($TUI_USER_HOME_ERROR); remove ~$user/.config/wingtip/tenants/$tenant.json by hand if it still exists" ;;
  esac
  if [[ "$TUI_DESKTOP_KIND" == unknown ]]; then
    say "WARNING: that desktop config may have held the master token of tenant '$tenant'; $master_note" >&2
  elif [[ "$desktop" != checked ]]; then
    say "WARNING: should a desktop config of '$user' hold the master token of tenant '$tenant' (token_kind master), $master_note" >&2
  fi
  if ((TUI_REGISTRY_TOKENS_RC == 3)); then
    say "note: client tokens are disabled for tenant '$tenant' (GATEWAY_CLIENT_TOKENS_FILE=none)"
  fi
}

# Tenant side first, so the token is dead before its desktop copy disappears.
# Neither side blocks the other (design 9.4: --revoke is the per-user
# emergency cut-off): a refused registry still lets the desktop file go and
# then fails the command, and a missing, foreign-owned or unsafe desktop home
# still lets the tenant-side entry go.
_tui_access_revoke() {  # <tenant> <user>
  local tenant="$1" user="$2"
  local label="tui:$user" env_path resolve_rc=0 desktop=checked
  _tui_desktop_user_resolve "$user" revoke || resolve_rc=$?
  case "$resolve_rc" in
    0) ;;
    2)
      desktop=no-account
      say "WARNING: user '$user' has no passwd entry; only the tenant-side '$label' entry can be removed" >&2
      ;;
    3)
      desktop=unusable
      say "WARNING: user '$user': $TUI_USER_HOME_ERROR; its desktop config is not checked, only the tenant-side '$label' entry can be removed" >&2
      ;;
    *) die "internal error: unexpected desktop user status $resolve_rc" ;;
  esac
  env_path="$(tenant_env_dir "$tenant")/lunarwing.env"
  _tui_registry_drop_label_step "$tenant" "$env_path" "$label"
  TUI_DESKTOP_RESULT=absent TUI_DESKTOP_KIND="" TUI_DESKTOP_FILE=""
  if [[ "$desktop" == checked ]]; then
    _tui_desktop_config_remove "$user" "$TUI_USER_HOME" "$tenant"
  fi
  _tui_revoke_report "$tenant" "$user" "$label" "$desktop"
  if [[ "$TUI_REGISTRY_RESULT" == failed ]]; then
    warn_err "the tenant-side '$label' entry of tenant '$tenant' may still be registered (see the error above); fix that and re-run --revoke"
    return 1
  fi
}

# ── Command ──────────────────────────────────────────────────────────────────

_tui_access_usage() {
  die "usage: tui-access <name> --for-user <user> [--url-host <host>] [--expires-in-days <n>] [--use-master-token] [--no-verify] | <name> --for-user <user> --revoke | <name> --list"
}

tui_access_command() {
  local tenant="" user="" url_host="" expires_days="" use_master=false no_verify=false mode=grant
  local user_set=false url_host_set=false name
  while (($# > 0)); do
    case "$1" in
      --for-user)
        (($# >= 2)) || die "--for-user requires a user name"
        user="$2" user_set=true
        shift 2
        ;;
      --url-host)
        (($# >= 2)) && [[ -n "$2" ]] || die "--url-host requires a host"
        url_host="$2" url_host_set=true
        shift 2
        ;;
      --expires-in-days)
        # An empty value would read as "never" and skip the checks below.
        (($# >= 2)) && [[ -n "$2" ]] || die "--expires-in-days requires a number of days"
        expires_days="$2"
        shift 2
        ;;
      --use-master-token) use_master=true; shift ;;
      --no-verify) no_verify=true; shift ;;
      --revoke | --list)
        [[ "$mode" == grant ]] || die "--revoke and --list are mutually exclusive"
        mode="${1#--}"
        shift
        ;;
      -*) die "unknown tui-access flag: $(sanitize_display_value "$1" 64)" ;;
      *)
        [[ -z "$tenant" ]] || die "unexpected tui-access argument: $(sanitize_display_value "$1" 64)"
        tenant="$1"
        shift
        ;;
    esac
  done
  [[ -n "$tenant" ]] || _tui_access_usage
  if [[ "$mode" != grant ]]; then
    if [[ "$url_host_set" == true || -n "$expires_days" || "$use_master" == true || "$no_verify" == true ]]; then
      die "--$mode does not take --url-host, --expires-in-days, --use-master-token or --no-verify"
    fi
  fi
  case "$mode" in
    list) [[ "$user_set" == false ]] || die "--list lists every label of the tenant; drop --for-user" ;;
    *) [[ "$user_set" == true && -n "$user" ]] || die "tui-access requires --for-user <user> (no default: a credential grant names its user)" ;;
  esac
  if [[ -n "$expires_days" ]]; then
    if [[ ! "$expires_days" =~ ^[0-9]{1,4}$ ]] || ((10#$expires_days < 1 || 10#$expires_days > TUI_ACCESS_MAX_EXPIRY_DAYS)); then
      die "--expires-in-days must be a whole number from 1 to $TUI_ACCESS_MAX_EXPIRY_DAYS"
    fi
    [[ "$use_master" == false ]] \
      || die "--expires-in-days applies to client tokens only; the master token does not expire"
  fi

  ports_registry_require_readonly
  name="$(sanitize_name "$tenant")"
  [[ "$name" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]] || die "invalid tenant name: $(sanitize_display_value "$tenant" 64)"
  tenant_exists_in_registry "$name" || die "tenant '$name' not found in registry"
  case "$mode" in
    list) _tui_registry_list "$name" ;;
    revoke) with_tenant_lifecycle_lock "$name" _tui_access_revoke "$name" "$user" ;;
    grant) with_tenant_lifecycle_lock "$name" _tui_access_grant "$name" "$user" "$url_host" "$expires_days" "$use_master" "$no_verify" ;;
  esac
}
