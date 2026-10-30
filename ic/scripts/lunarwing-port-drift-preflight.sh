#!/usr/bin/env bash
#
# lunarwing-port-drift-preflight.sh — read-only env-vs-registry port-drift
# verifier for every registry-known service (WI#509, kumo finding f#191).
#
# lunarwing-weechat-preflight.sh was the only script that cross-checked a
# channel's live env port against the ports registry. This generalizes that
# check shape (expected-port table + env-vs-registry compare + non-fatal
# report) to every other service whose port lands in a rendered file:
#
#   lunarwing.env       GATEWAY_PORT, HTTP_PORT, ORCHESTRATOR_PORT,
#                       COORD_ADMIN_PORT, ADAPTER_PORT, WEECHAT_ADAPTER_PORT,
#                       the four worker WSS ports, XMPP_BRIDGE_URL, RELAY_URL,
#                       WS_ADAPTER_URL, DARKIRC_ADAPTER_URL,
#                       VISION_SERVICE_URL, VOICE_SERVICE_URL, DATABASE_URL
#   xmpp-bridge.env     XMPP_BRIDGE_BIND
#   proxy.env           PROXY_PORT                         (proxy enabled)
#   darkirc-adapter.env DARKIRC_PORT, ADAPTER_PORT        (darkirc enabled)
#   darkirc_config.toml irc_listen, rpc_listen             (darkirc enabled)
#   systemd Quadlets    PublishPort host ports for the pg / vision / voice
#                       containers and every enabled external worker (must be
#                       strict 127.0.0.1 mappings; a host-less or 0.0.0.0
#                       publish FAILs instead of hiding as "no host ports")
#   systemd proxy unit  ExecStart --port + --bind 127.0.0.1  (proxy enabled)
#   OpenRC proxy unit   proxy_args --port + --bind 127.0.0.1 (proxy enabled)
#
# The WeeChat relay.conf / weechat.env credential checks stay in
# lunarwing-weechat-preflight.sh; this script covers the same lunarwing.env
# WeeChat keys plus every other service.
#
# Strictly read-only: never writes, never restarts a service, and never
# prints secret values. Env content is reduced to a port, a host-shaped
# token, or a strict loopback URL before it is displayed; anything else is
# reported as '<… (not printed)>' so operator-controlled env bytes can never
# reach the log. A listener/URL host that is not 127.0.0.1 FAILs (the WI#357
# self-exposure class) instead of being accepted because the port matched.
#
# Severity mirrors the WeeChat preflight's rationale: `mt-admin patch-env`
# is idempotent and SKIPS variables already present, so a WRONG existing
# value is not auto-corrected (FAIL — resolve before restarting) while a
# MISSING one is back-filled by the next render (WARN). COORD_ADMIN_PORT is
# the one key a full re-render deliberately preserves, so its mismatch is
# a WARN, not a FAIL. A missing env/config file for an ENABLED service is a
# FAIL (the unit cannot start without it). A missing Quadlet/unit is INFO —
# OpenRC hosts, docker-rootful hosts and pre-render tenants legitimately
# have none.
#
# Pre-migration registry slots (reserved_5/6/7/8/9 where the dedicated names
# live today) are deliberately NOT aliased here: only explicitly dedicated
# numeric slots are compared, so an unmigrated registry skips those checks
# instead of guessing which slot is authoritative (mt-admin's migrations
# rename them on its next invocation).
#
# Usage:
#   sudo ic/scripts/lunarwing-port-drift-preflight.sh [tenant]
#     (no args)  check every registered tenant
#     <tenant>   check just that tenant
#
# Exit status:
#   0  all consistent (warnings allowed — those are the expected backfill steps)
#   1  one or more FAILs (genuine drift — resolve before restarting)
#   2  setup error (missing jq / registry / bad args)
#
# Env overrides (mainly for testing):
#   LUNARWING_PORTS_REGISTRY       default /etc/lunarwing/ports.json
#   LUNARWING_TENANT_HOME_BASE     default /home
#   LUNARWING_MT_OPENRC_INITD_DIR  default /etc/init.d (same override mt-admin uses)

set -euo pipefail

PORTS_REGISTRY="${LUNARWING_PORTS_REGISTRY:-/etc/lunarwing/ports.json}"
TENANT_HOME_BASE="${LUNARWING_TENANT_HOME_BASE:-/home}"
OPENRC_INITD_DIR="${LUNARWING_MT_OPENRC_INITD_DIR:-/etc/init.d}"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 2; }

# Colors only when stdout is a terminal.
if [[ -t 1 ]]; then
  C_OK=$'\033[32m'; C_WARN=$'\033[33m'; C_FAIL=$'\033[31m'; C_DIM=$'\033[2m'; C_RST=$'\033[0m'
else
  C_OK=""; C_WARN=""; C_FAIL=""; C_DIM=""; C_RST=""
fi

command -v jq >/dev/null 2>&1 || die "required command not found: jq"
[[ -f "$PORTS_REGISTRY" ]] || die "port registry not found: $PORTS_REGISTRY (run add-tenant first?)"
jq -e '.tenants | type == "object"' "$PORTS_REGISTRY" >/dev/null 2>&1 \
  || die "invalid port registry: $PORTS_REGISTRY"

fail_total=0
warn_total=0
tenant_total=0

tenant_lw_root() { printf '%s/%s/lunarwing' "$TENANT_HOME_BASE" "$1"; }
tenant_systemd_user_dir() { printf '%s/%s/.config/systemd/user' "$TENANT_HOME_BASE" "$1"; }
tenant_quadlet_dir() { printf '%s/%s/.config/containers/systemd' "$TENANT_HOME_BASE" "$1"; }

valid_tenant_name() { [[ "$1" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]]; }

# Extract the trailing :PORT from a URL (tolerates a trailing slash).
# Prints nothing unless the remainder is 1-5 ASCII digits (the ASCII class
# keeps a UTF-8 locale from smuggling non-ASCII digits past the compare).
url_port() {
  local u="${1%/}"
  u="${u##*:}"
  if [[ "$u" =~ ^[0123456789]{1,5}$ ]]; then printf '%s' "$u"; fi
  return 0
}

# Read one KEY from an env file WITHOUT sourcing it (no code execution).
# Returns the last definition; strips a trailing CR and optional quotes.
env_get() {
  local file="$1" key="$2" line
  line="$(grep -E "^${key}=" "$file" 2>/dev/null | tail -n1 || true)"
  [[ -n "$line" ]] || { printf ''; return 0; }
  line="${line#*=}"
  line="${line%$'\r'}"
  line="${line%\"}"; line="${line#\"}"
  printf '%s' "$line"
}

# Port of a postgres://user:pass@host:PORT/db URL. The port comes from the
# matched host:PORT group, never from "after the last colon of the whole URL":
# a `/db:tag` suffix used to be reported as the port. Prints only the port —
# the password never reaches the output. Prints nothing when unparseable.
db_url_port() {
  local u="$1"
  local re='^postgres(ql)?://[^/@[:space:]]+@[^/:[:space:]]+:([0123456789]{1,5})(/|[?]|$)'
  if [[ "$u" =~ $re ]]; then printf '%s' "${BASH_REMATCH[2]}"; fi
  return 0
}

# Display discipline: only port-shaped values, host-shaped tokens and strict
# loopback URLs are ever echoed; anything else (operator-controlled env
# content) is replaced.
display_port() {
  if [[ "$1" =~ ^[0123456789]{1,5}$ ]]; then printf '%s' "$1"
  else printf '<unexpected value (not printed)>'; fi
}

# A host token read from a listen/bind directive. Realistic drift values
# (0.0.0.0, ::, a hostname or IPv4) are shown; arbitrary env bytes are not.
display_host() {
  if [[ "$1" =~ ^[A-Za-z0-9_.:-]{1,64}$ ]]; then printf '%s' "$1"
  else printf '<unexpected host (not printed)>'; fi
}

display_url() {
  if [[ "$1" =~ ^https?://127[.]0[.]0[.]1:[0123456789]{1,5}(/[A-Za-z0-9._~%+/-]*)?$ ]]; then printf '%s' "$1"
  else printf '<non-loopback value (not printed)>'; fi
}

mark() { # $1=OK|WARN|FAIL|INFO  $2=label  $3=detail
  local c=""
  case "$1" in
    OK)   c="$C_OK" ;;
    WARN) c="$C_WARN"; warn_total=$((warn_total + 1)) ;;
    FAIL) c="$C_FAIL"; fail_total=$((fail_total + 1)) ;;
    INFO) c="$C_DIM" ;;
  esac
  printf '    %s[%-4s]%s %-22s %s\n' "$c" "$1" "$C_RST" "$2" "$3"
}

# FAIL when a file an enabled service needs is absent; WARN when unreadable.
require_file() { # <file> <label>
  if [[ ! -e "$1" ]]; then
    mark FAIL "$2" "missing: $1"
    return 1
  fi
  if [[ ! -r "$1" ]]; then
    mark WARN "$2" "not readable — run with sudo: $1"
    return 1
  fi
  return 0
}

# ── Registry snapshot ─────────────────────────────────────────────────────────

# Per-tenant snapshot of every explicitly dedicated, numeric port slot
# across all five per-tenant port maps. Pre-migration reserved_* slots are
# intentionally not aliased (see the header).
declare -A reg=()
load_registry_snapshot() { # <tenant>
  local name="$1" slot port
  reg=()
  while IFS=$'\t' read -r slot port; do
    [[ -n "$slot" ]] || continue
    reg["$slot"]="$port"
  done < <(jq -r --arg name "$name" '
      .tenants[$name] // {}
      | [ ((.ports // {}) | to_entries[]),
          ((.extended_ports // {}) | to_entries[]),
          ((.kimi_ports // {}) | to_entries[]),
          ((.voice_ports // {}) | to_entries[]),
          ((.opencode2_ports // {}) | to_entries[]) ]
      | .[] | select((.value | type) == "number")
      | "\(.key)\t\(.value)"
    ' "$PORTS_REGISTRY")
}

registry_port() { printf '%s' "${reg[$1]:-}"; }

tenant_flag() { # <tenant> <field> — true when .tenants[$name].<field> == true
  [[ "$(jq -r --arg name "$1" --arg field "$2" \
      '.tenants[$name][$field] == true' "$PORTS_REGISTRY" 2>/dev/null || true)" == "true" ]]
}

tenant_worker_enabled() { # <tenant> <worker>
  [[ "$(jq -r --arg name "$1" --arg w "$2" \
      '.tenants[$name].workers[$w] == true' "$PORTS_REGISTRY" 2>/dev/null || true)" == "true" ]]
}

tenant_vision_external() { # <tenant>
  [[ "$(jq -r --arg name "$1" \
      '.tenants[$name].vision.mode // "local"' "$PORTS_REGISTRY" 2>/dev/null || true)" == "external" ]]
}

# ── Check helpers ─────────────────────────────────────────────────────────────

# Bare numeric env key vs a registry slot. Wrong value → FAIL (patch-env
# will not overwrite it); missing → WARN (the next render back-fills it).
# The optional 4th arg downgrades a mismatch to WARN for keys a full
# re-render deliberately preserves (COORD_ADMIN_PORT).
check_port_key() { # <env_file> <env_key> <registry_slot> [soft]
  local file="$1" key="$2" slot="$3" soft="${4:-}"
  local expected value
  expected="$(registry_port "$slot")"
  [[ -n "$expected" ]] || return 0
  value="$(env_get "$file" "$key")"
  if [[ -z "$value" ]]; then
    mark WARN "$key" "missing — the next patch-env/render adds $key=$expected"
  elif [[ "$value" == "$expected" ]]; then
    mark OK "$key" "$value"
  elif [[ -n "$soft" ]]; then
    mark WARN "$key" "$(display_port "$value") (registry $slot=$expected) — preserved by re-render; fix if unintentional"
  else
    mark FAIL "$key" "$(display_port "$value") (registry $slot=$expected) — patch-env will NOT overwrite this"
  fi
}

# URL-valued env key: the scheme/host:port is parsed, the host must be
# loopback (a non-loopback listener is the WI#357 drift class, not an OK),
# and only the port is compared. Only a verified loopback URL is displayed.
check_url_port() { # <env_file> <env_key> <registry_slot>
  local file="$1" key="$2" slot="$3"
  local expected value host="" port=""
  expected="$(registry_port "$slot")"
  [[ -n "$expected" ]] || return 0
  value="$(env_get "$file" "$key")"
  if [[ -z "$value" ]]; then
    mark WARN "$key" "missing — the next render adds http://127.0.0.1:$expected"
    return 0
  fi
  local url_re='^(https?)://([^/:[:space:]@]+):([0123456789]{1,5})(/[^[:space:]]*)?$'
  if [[ "$value" =~ $url_re ]]; then
    host="${BASH_REMATCH[2]}"
    port="${BASH_REMATCH[3]}"
  fi
  if [[ -z "$port" ]]; then
    mark FAIL "$key" "no scheme://host:port in value (registry $slot=$expected)"
  elif [[ "$host" != "127.0.0.1" ]]; then
    mark FAIL "$key" "host $(display_host "$host") is not loopback (registry $slot=$expected)"
  elif [[ "$port" == "$expected" ]]; then
    mark OK "$key" "$(display_url "$value")"
  else
    mark FAIL "$key" "port $(display_port "$port") (registry $slot=$expected) — patch-env will NOT overwrite this"
  fi
}

# DATABASE_URL: reduced to its port before comparing (and printing).
check_db_port() { # <env_file> <env_key> <registry_slot>
  local file="$1" key="$2" slot="$3"
  local expected value port
  expected="$(registry_port "$slot")"
  [[ -n "$expected" ]] || return 0
  value="$(env_get "$file" "$key")"
  if [[ -z "$value" ]]; then
    mark WARN "$key" "missing — the next render re-derives it from the registry"
    return 0
  fi
  port="$(db_url_port "$value")"
  if [[ -z "$port" ]]; then
    mark FAIL "$key" "unparseable database URL (expected port $expected)"
  elif [[ "$port" == "$expected" ]]; then
    mark OK "$key" "postgres port $(display_port "$port")"
  else
    mark FAIL "$key" "postgres port $(display_port "$port") (registry $slot=$expected) — patch-env will NOT overwrite this"
  fi
}

# host:PORT bind value (XMPP_BRIDGE_BIND): loopback host required too.
check_bind_port() { # <env_file> <env_key> <registry_slot>
  local file="$1" key="$2" slot="$3"
  local expected value host port
  expected="$(registry_port "$slot")"
  [[ -n "$expected" ]] || return 0
  value="$(env_get "$file" "$key")"
  if [[ -z "$value" ]]; then
    mark WARN "$key" "missing — the next render adds 127.0.0.1:$expected"
    return 0
  fi
  host="${value%%:*}"
  port="$(url_port "$value")"
  if [[ "$host" != "127.0.0.1" ]]; then
    mark FAIL "$key" "bind host $(display_host "$host") is not loopback (registry $slot=$expected)"
  elif [[ "$port" == "$expected" ]]; then
    mark OK "$key" "127.0.0.1:$port"
  else
    mark FAIL "$key" "port $(display_port "$port") (registry $slot=$expected)"
  fi
}

# A `listen = "tcp://127.0.0.1:PORT"` directive in darkirc_config.toml. The
# host must be loopback; on drift the host that was read is reported, never
# a reconstructed 127.0.0.1 (a `tcp://0.0.0.0:PORT` value used to print OK).
check_toml_listen() { # <toml_file> <directive> <registry_slot>
  local file="$1" directive="$2" slot="$3"
  local expected line scheme="" host="" port=""
  expected="$(registry_port "$slot")"
  [[ -n "$expected" ]] || return 0
  line="$(grep -E "^[[:space:]]*${directive}[[:space:]]*=" "$file" 2>/dev/null | tail -n1 || true)"
  if [[ -z "$line" ]]; then
    mark WARN "$directive" "missing in $(basename "$file") — regenerate the darkirc config"
    return 0
  fi
  local listen_re='^[[:space:]]*'"$directive"'[[:space:]]*=[[:space:]]*"(tcp|udp)://([^:"[:space:]]+):([0123456789]{1,5})"'
  if [[ "$line" =~ $listen_re ]]; then
    scheme="${BASH_REMATCH[1]}"
    host="${BASH_REMATCH[2]}"
    port="${BASH_REMATCH[3]}"
  fi
  if [[ -z "$port" ]]; then
    mark FAIL "$directive" "no tcp://host:port listen (registry $slot=$expected) — regenerate the darkirc config"
  elif [[ "$host" != "127.0.0.1" ]]; then
    mark FAIL "$directive" "listen host $(display_host "$host") is not loopback (registry $slot=$expected)"
  elif [[ "$port" == "$expected" ]]; then
    mark OK "$directive" "$scheme://127.0.0.1:$port"
  else
    mark FAIL "$directive" "port $(display_port "$port") (registry $slot=$expected) — regenerate the darkirc config"
  fi
}

# Host-side ports of every strictly loopback PublishPort line in a Quadlet.
quadlet_host_ports() { # <file>
  grep -oE 'PublishPort=127[.]0[.]0[.]1:[0123456789]{1,5}:[0123456789]{1,5}' "$1" 2>/dev/null \
    | cut -d: -f2 || true
}

# Every PublishPort line that is NOT a strict 127.0.0.1 mapping. A host-less
# `PORT:CONTAINER` mapping or an explicit `0.0.0.0`/other-address mapping
# publishes on all interfaces; matching only loopback lines used to hide that
# as "host ports [none] lack registry port(s)".
quadlet_nonloopback_publishes() { # <file>
  grep -E '^[[:space:]]*PublishPort=' "$1" 2>/dev/null \
    | grep -vE '^[[:space:]]*PublishPort=127[.]0[.]0[.]1:[0123456789]{1,5}:[0123456789]{1,5}$' \
    || true
}

# Every expected registry host port must appear among the Quadlet's loopback
# PublishPort host ports, and no PublishPort may bind beyond 127.0.0.1.
# Extra loopback ports are not drift (an operator may add one); a re-mapped
# expected port or a non-loopback/all-interfaces mapping is.
check_quadlet_ports() { # <file> <label> <expected-host-ports...>
  local file="$1" label="$2"
  shift 2
  if [[ ! -e "$file" ]]; then
    mark INFO "$label" "not rendered ($file) — OpenRC/docker host or pre-render; skipped"
    return 0
  fi
  if [[ ! -r "$file" ]]; then
    mark WARN "$label" "not readable — run with sudo: $file"
    return 0
  fi
  local -a have=() missing=() unsafe=()
  mapfile -t have < <(quadlet_host_ports "$file")
  mapfile -t unsafe < <(quadlet_nonloopback_publishes "$file")
  local want h found
  for want in "$@"; do
    [[ -n "$want" ]] || continue
    found=false
    for h in "${have[@]}"; do
      if [[ "$h" == "$want" ]]; then found=true; break; fi
    done
    [[ "$found" == true ]] || missing+=("$want")
  done
  if [[ "${#missing[@]}" -eq 0 && "${#unsafe[@]}" -eq 0 ]]; then
    mark OK "$label" "PublishPort covers registry port(s): $*"
    return 0
  fi
  local detail=""
  if [[ "${#unsafe[@]}" -gt 0 ]]; then
    detail+="${#unsafe[@]} PublishPort line(s) are not strict 127.0.0.1 mappings (published on all interfaces or another address)"
    [[ "${#missing[@]}" -eq 0 ]] || detail+="; "
  fi
  if [[ "${#missing[@]}" -gt 0 ]]; then
    detail+="host ports [${have[*]:-none}] lack registry port(s): ${missing[*]}"
  fi
  mark FAIL "$label" "${detail} — re-render units"
}

# Port and bind host from the line carrying `--port <digits>` in a rendered
# proxy unit (the systemd ExecStart and the OpenRC proxy_args line share the
# flag). Mirrors the OpenRC safe-unit contract's strictness: the same line
# must carry `--bind 127.0.0.1`; a bare first `--port` anywhere is not enough.
unit_cli_bind() { # <file> -> "PORT<TAB>HOST" (HOST may be empty), or nothing
  local file="$1" line port="" host=""
  line="$(grep -E -- '--port [0123456789]{1,5}' "$file" 2>/dev/null | head -n1 || true)"
  [[ -n "$line" ]] || return 0
  if [[ "$line" =~ --port\ ([0123456789]{1,5}) ]]; then port="${BASH_REMATCH[1]}"; fi
  if [[ "$line" =~ --bind\ ([^[:space:]]+) ]]; then host="${BASH_REMATCH[1]}"; fi
  [[ -n "$port" ]] && printf '%s\t%s\n' "$port" "$host"
  return 0
}

check_unit_port() { # <file> <label> <registry_slot>
  local file="$1" label="$2" slot="$3"
  local expected parsed port host=""
  expected="$(registry_port "$slot")"
  [[ -n "$expected" ]] || return 0
  if [[ ! -e "$file" ]]; then
    mark INFO "$label" "not rendered ($file) — other init or pre-render; skipped"
    return 0
  fi
  if [[ ! -r "$file" ]]; then
    mark WARN "$label" "not readable — run with sudo: $file"
    return 0
  fi
  parsed="$(unit_cli_bind "$file")"
  port="${parsed%%$'\t'*}"
  [[ "$parsed" == *$'\t'* ]] && host="${parsed#*$'\t'}"
  if [[ -z "$port" ]]; then
    mark WARN "$label" "no --port in $file — not an mt-admin-rendered proxy unit?"
  elif [[ -z "$host" ]]; then
    mark FAIL "$label" "no --bind on the --port line — expected 127.0.0.1 (registry $slot=$expected)"
  elif [[ "$host" != "127.0.0.1" ]]; then
    mark FAIL "$label" "binds $(display_host "$host") instead of 127.0.0.1 (registry $slot=$expected)"
  elif [[ "$port" == "$expected" ]]; then
    mark OK "$label" "binds --port $port on 127.0.0.1"
  else
    mark FAIL "$label" "binds --port $(display_port "$port") (registry $slot=$expected) — re-render units"
  fi
}

# ── Per-tenant checks ─────────────────────────────────────────────────────────

check_main_env() { # <lunarwing.env> <darkirc_on> <vision_external> <voice_on>
  local env="$1" darkirc_on="$2" vision_external="$3" voice_on="$4"
  check_port_key "$env" GATEWAY_PORT gateway
  check_port_key "$env" HTTP_PORT http
  check_port_key "$env" ORCHESTRATOR_PORT orchestrator
  check_port_key "$env" ADAPTER_PORT weechat_adapter
  check_port_key "$env" WEECHAT_ADAPTER_PORT weechat_adapter
  check_port_key "$env" PEBBLE_WSS_PORT pebble_wss
  check_port_key "$env" OPENCODE_WSS_PORT opencode_wss
  check_port_key "$env" KIMI_WSS_PORT kimi_wss
  check_port_key "$env" OPENCODE2_WSS_PORT opencode2_wss
  check_port_key "$env" COORD_ADMIN_PORT coord_admin soft
  check_url_port "$env" XMPP_BRIDGE_URL bridge
  check_url_port "$env" RELAY_URL weechat
  check_url_port "$env" WS_ADAPTER_URL weechat_adapter
  if [[ "$darkirc_on" == true ]]; then
    check_url_port "$env" DARKIRC_ADAPTER_URL darkirc_adapter
  fi
  if [[ "$vision_external" == true ]]; then
    mark INFO "VISION_SERVICE_URL" "external LunarVision mode — registry ports not used"
  else
    check_url_port "$env" VISION_SERVICE_URL vision_service
  fi
  if [[ "$voice_on" == true ]]; then
    check_url_port "$env" VOICE_SERVICE_URL voice_service
  fi
  check_db_port "$env" DATABASE_URL postgres
}

check_proxy_files() { # <tenant> <env_dir>
  local name="$1" env="$2/proxy.env"
  if require_file "$env" "proxy.env"; then
    check_port_key "$env" PROXY_PORT proxy
  fi
  check_unit_port "$(tenant_systemd_user_dir "$name")/lunarwing-proxy-${name}.service" \
    "proxy unit (systemd)" proxy
  check_unit_port "$OPENRC_INITD_DIR/lunarwing-proxy-${name}" \
    "proxy unit (openrc)" proxy
}

check_darkirc_files() { # <tenant> <env_dir>
  local name="$1" env="$2/darkirc-adapter.env"
  local conf
  if require_file "$env" "darkirc-adapter.env"; then
    check_port_key "$env" DARKIRC_PORT darkirc_irc
    check_port_key "$env" ADAPTER_PORT darkirc_adapter
  fi
  conf="$(tenant_lw_root "$name")/state/darkirc/darkirc_config.toml"
  if require_file "$conf" "darkirc_config.toml"; then
    check_toml_listen "$conf" irc_listen darkirc_irc
    check_toml_listen "$conf" rpc_listen darkirc_rpc
  fi
}

check_quadlets() { # <tenant>
  local name="$1" qdir worker wss health
  qdir="$(tenant_quadlet_dir "$name")"
  check_quadlet_ports "$qdir/lunarwing-pg-${name}.container" "pg quadlet" \
    "$(registry_port postgres)"
  if tenant_flag "$name" enable_voice && [[ -n "$(registry_port voice_service)" ]]; then
    check_quadlet_ports "$qdir/lunarwing-voice-${name}.container" "voice quadlet" \
      "$(registry_port voice_service)" "$(registry_port voice_health)"
  fi
  if ! tenant_vision_external "$name" && [[ -n "$(registry_port vision_service)" ]]; then
    check_quadlet_ports "$qdir/lunarwing-vision-${name}.container" "vision quadlet" \
      "$(registry_port vision_service)" "$(registry_port vision_health)"
  fi
  for worker in pebble opencode kimi opencode2; do
    tenant_worker_enabled "$name" "$worker" || continue
    wss="$(registry_port "${worker}_wss")"
    health="$(registry_port "${worker}_health")"
    [[ -n "$wss" ]] || continue
    check_quadlet_ports "$qdir/lunarwing-${worker}-${name}.container" "${worker} quadlet" \
      "$wss" "$health"
  done
}

check_tenant() {
  local name="$1"
  tenant_total=$((tenant_total + 1))
  printf '%s== %s ==%s\n' "$C_DIM" "$name" "$C_RST"

  if ! valid_tenant_name "$name"; then
    mark FAIL "tenant" "registry name is not a safe tenant name — skipping"
    say ""
    return 0
  fi

  load_registry_snapshot "$name"
  if [[ ${#reg[@]} -eq 0 ]]; then
    mark INFO "registry" "no ports allocated for this tenant — skipping"
    say ""
    return 0
  fi

  local env_dir darkirc_on=false proxy_on=false voice_on=false vision_external=false
  env_dir="$(tenant_lw_root "$name")/env"
  tenant_flag "$name" enable_darkirc && darkirc_on=true
  tenant_flag "$name" enable_proxy && proxy_on=true
  tenant_flag "$name" enable_voice && voice_on=true
  tenant_vision_external "$name" && vision_external=true

  if ! require_file "$env_dir/lunarwing.env" "env file"; then
    say ""
    return 0
  fi

  check_main_env "$env_dir/lunarwing.env" "$darkirc_on" "$vision_external" "$voice_on"

  if require_file "$env_dir/xmpp-bridge.env" "xmpp-bridge.env"; then
    check_bind_port "$env_dir/xmpp-bridge.env" XMPP_BRIDGE_BIND bridge
  fi

  if [[ "$proxy_on" == true ]]; then
    check_proxy_files "$name" "$env_dir"
  fi
  if [[ "$darkirc_on" == true ]]; then
    check_darkirc_files "$name" "$env_dir"
  fi
  check_quadlets "$name"
  say ""
}

# ── Main ──────────────────────────────────────────────────────────────────────

main() {
  local names=()
  if [[ $# -gt 1 ]]; then
    die "usage: lunarwing-port-drift-preflight.sh [tenant]"
  elif [[ $# -eq 1 ]]; then
    valid_tenant_name "$1" || die "invalid tenant name: $1"
    jq -e --arg t "$1" '.tenants[$t]' "$PORTS_REGISTRY" >/dev/null 2>&1 \
      || die "tenant '$1' not found in $PORTS_REGISTRY"
    names=("$1")
  else
    mapfile -t names < <(jq -r '.tenants | keys[]' "$PORTS_REGISTRY" 2>/dev/null)
  fi

  if [[ ${#names[@]} -eq 0 ]]; then
    say "no tenants registered in $PORTS_REGISTRY"
    return 0
  fi

  say "Port drift pre-flight  (registry: $PORTS_REGISTRY)"
  say ""
  local n
  for n in "${names[@]}"; do
    [[ -n "$n" ]] && check_tenant "$n"
  done

  say "----------------------------------------------------------------"
  if [[ "$fail_total" -gt 0 ]]; then
    printf '%sFAIL%s  %d failing check(s), %d warning(s), %d tenant(s)\n' \
      "$C_FAIL" "$C_RST" "$fail_total" "$warn_total" "$tenant_total"
    say "Resolve FAILs before restarting — a wrong *existing* env value is not auto-corrected by patch-env."
    return 1
  elif [[ "$warn_total" -gt 0 ]]; then
    printf '%sOK (with warnings)%s  %d warning(s), %d tenant(s)\n' \
      "$C_WARN" "$C_RST" "$warn_total" "$tenant_total"
    say "Warnings are the expected backfill steps: re-run add-tenant/render-units, then restart."
    return 0
  else
    printf '%sOK%s  all %d tenant(s) consistent — env files and rendered units match the registry\n' \
      "$C_OK" "$C_RST" "$tenant_total"
    return 0
  fi
}

main "$@"
