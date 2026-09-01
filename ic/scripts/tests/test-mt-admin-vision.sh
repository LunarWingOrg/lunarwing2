#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"

ROOT="$(mktemp -d)"
trap 'command rm -rf "$ROOT"' EXIT
failures=0
CALLS=()

fail() {
  printf 'FAIL: %s\n' "$1"
  failures=$((failures + 1))
}

pass() { printf 'PASS: %s\n' "$1"; }

assert_contains() {
  local haystack="$1" needle="$2" label="$3"
  if grep -qF -- "$needle" <<<"$haystack"; then pass "$label"; else fail "$label (missing: $needle)"; fi
}

assert_call() {
  local expected="$1" call
  for call in "${CALLS[@]}"; do
    [[ "$call" == "$expected" ]] && { pass "$expected"; return; }
  done
  fail "missing call: $expected"
}

assert_no_call_prefix() {
  local prefix="$1" call
  for call in "${CALLS[@]}"; do
    [[ "$call" != "$prefix"* ]] || { fail "unexpected call: $call"; return; }
  done
  pass "no call starting with: $prefix"
}

assert_before() {
  local first="$1" second="$2" first_index=-1 second_index=-1 index
  for index in "${!CALLS[@]}"; do
    [[ "${CALLS[$index]}" == "$first" ]] && first_index="$index"
    [[ "${CALLS[$index]}" == "$second" ]] && second_index="$index"
  done
  if [[ "$first_index" -ge 0 && "$second_index" -gt "$first_index" ]]; then
    pass "$first before $second"
  else
    fail "expected '$first' before '$second'"
  fi
}

# ── Port validation and legacy registry compatibility ────────────────────────

PORTS_REGISTRY="$ROOT/ports.json"
cat >"$PORTS_REGISTRY" <<'JSON'
{
  "tenants": {
    "alpha": {
      "extended_ports": {
        "vision_service": 20005,
        "vision_health": 20006
      }
    },
    "legacy": {
      "extended_ports": {
        "reserved_5": 21005,
        "reserved_6": 21006
      }
    }
  }
}
JSON

if _vision_port_valid 20005 && ! _vision_port_valid 0 && ! _vision_port_valid 65536 \
    && ! _vision_port_valid "" && ! _vision_port_valid abc; then
  pass "vision ports are range-checked"
else
  fail "vision port validation"
fi
[[ "$(ports_get legacy vision_service)" == 21005 ]] \
  && pass "legacy vision service port resolves" || fail "legacy vision service port"
[[ "$(ports_get legacy vision_health)" == 21006 ]] \
  && pass "legacy vision health port resolves" || fail "legacy vision health port"

# ── Rendered systemd/OpenRC contracts ────────────────────────────────────────

tenant_home() { printf '%s/home/%s' "$ROOT" "$1"; }
tenant_env_dir() { printf '%s/home/%s/lunarwing/env' "$ROOT" "$1"; }
tenant_quadlet_dir() { printf '%s/home/%s/.config/containers/systemd' "$ROOT" "$1"; }
ensure_container_runtime() { CONTAINER_RT=podman; MT_ROOTLESS=true; }
ensure_babysitter_helper() { :; }
chown() { CALLS+=("chown:$*"); }
id() {
  if [[ "${1:-}" == "-u" && "${2:-}" == "alpha" ]]; then printf '1001\n'; return 0; fi
  builtin command id "$@"
}
command() {
  if [[ "${1:-}" == "-v" && "${2:-}" == "podman" ]]; then printf '/usr/bin/podman\n'; return 0; fi
  builtin command "$@"
}

mkdir -p "$(tenant_env_dir alpha)" "$ROOT/init.d"
printf '%s\n' 'LUNARWING_AUTH_TOKEN=test-only-token' 'OCR_PORT=8088' 'OCR_HEALTH_PORT=8089' \
  >"$(tenant_env_dir alpha)/vision.env"
OPENRC_INITD_DIR="$ROOT/init.d"
# shellcheck disable=SC2034  # Consumed by sourced mt-admin functions.
CONTAINER_RT=podman
# shellcheck disable=SC2034  # Consumed by sourced mt-admin functions.
MT_ROOTLESS=true

# Imported above, then replaced with a call-recording mock later in this test.
# shellcheck disable=SC2218
render_vision_quadlet alpha
quadlet="$(tenant_quadlet_dir alpha)/lunarwing-vision-alpha.container"
quadlet_text="$(<"$quadlet")"
assert_contains "$quadlet_text" 'PublishPort=127.0.0.1:20005:8088' "Quadlet publishes API port"
assert_contains "$quadlet_text" 'PublishPort=127.0.0.1:20006:8089' "Quadlet publishes health port"
assert_contains "$quadlet_text" 'HealthCmd=curl -sf http://127.0.0.1:8089/health || exit 1' "Quadlet checks dedicated listener"
assert_contains "$quadlet_text" 'PodmanArgs=--security-opt=no-new-privileges' "Quadlet sets no-new-privileges"
assert_contains "$quadlet_text" "EnvironmentFile=$(tenant_env_dir alpha)/vision.env" "Quadlet uses secret env file"
if grep -qF 'test-only-token' "$quadlet"; then fail "Quadlet embedded auth token"; else pass "Quadlet does not embed auth token"; fi

# Imported above, then replaced with a call-recording mock later in this test.
# shellcheck disable=SC2218
render_vision_openrc_unit alpha
openrc_text="$(<"$OPENRC_INITD_DIR/lunarwing-vision-alpha")"
assert_contains "$openrc_text" 'before lunarwing-alpha' "OpenRC starts vision before daemon"
assert_contains "$openrc_text" 'container runtime unavailable' "OpenRC fails loudly without runtime"
assert_contains "$openrc_text" 'http://127.0.0.1:${vis_health_internal_port}/health' "OpenRC checks dedicated listener"

helper="$ROOT/lunarwing-ctr-babysit"
printf '#!/bin/sh\nexit 0\n' >"$helper"
chmod 0755 "$helper"
# shellcheck disable=SC2034  # Consumed by sourced mt-admin functions.
LUNARWING_MT_BABYSITTER_HELPER="$helper"
# Imported above, then replaced with a call-recording mock later in this test.
# shellcheck disable=SC2218
render_vision_systemd_fallback_unit alpha
fallback="$(tenant_home alpha)/.config/systemd/user/lunarwing-vision-alpha.service"
fallback_text="$(<"$fallback")"
assert_contains "$fallback_text" 'Environment=XDG_RUNTIME_DIR=/run/user/1001' "fallback unit has rootless runtime dir"
assert_contains "$fallback_text" "ExecStart=$helper lunarwing-vision-alpha" "fallback unit supervises podman wait"
assert_contains "$fallback_text" 'Restart=always' "fallback unit restarts babysitter"

# ── OpenRC registration includes direct health unit and crash supervisor ─────

render_vision_openrc_unit() { CALLS+=("render-openrc:$1"); }
render_container_babysitter_unit() { CALLS+=("render-supervisor:$*"); }
rc-update() { CALLS+=("rc-update:$*"); }
rc-service() { CALLS+=("rc-service:$*"); }
CALLS=()
_register_vision_openrc alpha
assert_call 'render-openrc:alpha'
assert_call 'rc-update:add lunarwing-vision-alpha default'
assert_call 'rc-service:lunarwing-vision-alpha start'
assert_call "render-supervisor:alpha vision lunarwing-vision-alpha 1001 $(tenant_home alpha)"
assert_call 'rc-update:add lunarwing-vision-alpha-sup default'
assert_call 'rc-service:lunarwing-vision-alpha-sup start'
assert_before 'rc-service:lunarwing-vision-alpha start' 'rc-service:lunarwing-vision-alpha-sup start'

INIT_SYSTEM=openrc
tenant_proxy_enabled() { return 1; }
tenant_weechat_enabled() { return 1; }
tenant_darkirc_enabled() { return 1; }
tenant_worker_enabled() { return 1; }
mapfile -t writer_services < <(tenant_writer_services alpha)
vision_sup_index=-1
vision_direct_index=-1
for index in "${!writer_services[@]}"; do
  [[ "${writer_services[$index]}" == "lunarwing-vision-alpha-sup" ]] && vision_sup_index="$index"
  [[ "${writer_services[$index]}" == "lunarwing-vision-alpha" ]] && vision_direct_index="$index"
done
if [[ "$vision_sup_index" -ge 0 && "$vision_direct_index" -gt "$vision_sup_index" ]]; then
  pass "writer quiescence stops vision supervisor before direct unit"
else
  fail "writer quiescence vision stop ordering"
fi

# ── Imperative container reconciliation is convergent and health-gated ───────

CONTAINER_EXISTS=true
CONFIG_CHANGED=false
UNSUPPORTED_HEALTH=false
CONTAINER_HEALTHY=true

_ctr() {
  local name="$1" action="$2"
  shift 2
  CALLS+=("ctr:$name $action $*")
  case "$action" in
    inspect) [[ "$CONTAINER_EXISTS" == true ]] ;;
    rm) CONTAINER_EXISTS=false ;;
  esac
}
_container_config_changed() { [[ "$CONFIG_CHANGED" == true ]]; }
_vision_has_unsupported_openrc_healthcheck() { [[ "$UNSUPPORTED_HEALTH" == true ]]; }
_vision_container_healthy() { [[ "$CONTAINER_HEALTHY" == true ]]; }
_wait_tenant_vision_healthy() { CALLS+=("wait:$*"); return 0; }
_stop_vision_supervision() { CALLS+=("stop-supervision:$*"); }
_create_vision_container() { CALLS+=("create:$*"); CONTAINER_EXISTS=true; }
_store_container_hash() { CALLS+=("store:$*"); }
ports_get() {
  case "$2" in
    vision_service) printf '20005' ;;
    vision_health) printf '20006' ;;
    *) return 1 ;;
  esac
}
INIT_SYSTEM=systemd

# ── render-units refreshes existing assets without starting services ─────────

render_vision_quadlet() { CALLS+=("render-quadlet:$1"); }
render_vision_systemd_fallback_unit() { CALLS+=("render-fallback:$1"); }
QUADLET_SUPPORTED=true
podman_supports_quadlet() { [[ "$QUADLET_SUPPORTED" == true ]]; }
qfile="$(tenant_quadlet_dir alpha)/lunarwing-vision-alpha.container"
fallback="$(tenant_home alpha)/.config/systemd/user/lunarwing-vision-alpha.service"
rm -f -- "$qfile" "$fallback"

CONTAINER_EXISTS=false
CALLS=()
render_vision_systemd_assets alpha
assert_no_call_prefix 'render-quadlet:'
assert_no_call_prefix 'render-fallback:'
assert_no_call_prefix 'systemctl:'

CONTAINER_EXISTS=true
CALLS=()
render_vision_systemd_assets alpha
assert_call 'render-quadlet:alpha'
assert_no_call_prefix 'systemctl:'

mkdir -p "$(dirname "$fallback")"
touch "$fallback"
QUADLET_SUPPORTED=false
CALLS=()
render_vision_systemd_assets alpha
assert_call 'render-fallback:alpha'
assert_no_call_prefix 'systemctl:'
rm -f -- "$fallback"

CONTAINER_EXISTS=false
rm -f -- "$OPENRC_INITD_DIR/lunarwing-vision-alpha" "$OPENRC_INITD_DIR/lunarwing-vision-alpha-sup"
CALLS=()
render_vision_openrc_assets alpha
assert_no_call_prefix 'render-openrc:'
assert_no_call_prefix 'render-supervisor:'

CONTAINER_EXISTS=true
touch "$OPENRC_INITD_DIR/lunarwing-vision-alpha"
CALLS=()
render_vision_openrc_assets alpha
assert_call 'render-openrc:alpha'
assert_call "render-supervisor:alpha vision lunarwing-vision-alpha 1001 $(tenant_home alpha)"
assert_no_call_prefix 'rc-service:'
assert_no_call_prefix 'rc-update:'

_retire_tenant_nanocode() { CALLS+=("retire:$1"); }
# shellcheck disable=SC2034  # Updates state consumed by sourced mt-admin functions.
ensure_init_system() { INIT_SYSTEM=systemd; }
tenant_voice_enabled() { return 1; }
render_tenant_systemd_units() { CALLS+=("render-systemd:$1"); }
_systemctl_user() { CALLS+=("systemctl:$*"); }
CALLS=()
_render_tenant_units_locked alpha
assert_call 'render-systemd:alpha'
assert_call 'systemctl:alpha daemon-reload'
assert_no_call_prefix 'systemctl:alpha start'
assert_no_call_prefix 'systemctl:alpha restart'

CONTAINER_EXISTS=true
QUADLET_SUPPORTED=true
CALLS=()
_reconcile_vision_container alpha lunarwing-vision-alpha "$(tenant_env_dir alpha)/vision.env" contract-a
assert_no_call_prefix 'stop-supervision:'
assert_no_call_prefix 'create:'
assert_no_call_prefix 'ctr:alpha stop'
assert_no_call_prefix 'ctr:alpha start'
assert_call "store:alpha lunarwing-vision-alpha $(tenant_env_dir alpha)/vision.env contract-a"

CONFIG_CHANGED=true
CALLS=()
_reconcile_vision_container alpha lunarwing-vision-alpha "$(tenant_env_dir alpha)/vision.env" contract-b
assert_call 'stop-supervision:alpha lunarwing-vision-alpha'
assert_call 'ctr:alpha stop --time 30 lunarwing-vision-alpha'
assert_call 'ctr:alpha rm -f lunarwing-vision-alpha'
assert_call "create:alpha lunarwing-vision-alpha $(tenant_env_dir alpha)/vision.env 20005 20006"
assert_before 'stop-supervision:alpha lunarwing-vision-alpha' 'ctr:alpha rm -f lunarwing-vision-alpha'

CONFIG_CHANGED=false
CONTAINER_EXISTS=true
CONTAINER_HEALTHY=false
CALLS=()
_reconcile_vision_container alpha lunarwing-vision-alpha "$(tenant_env_dir alpha)/vision.env" contract-c
assert_call 'stop-supervision:alpha lunarwing-vision-alpha'
assert_call 'ctr:alpha stop --time 30 lunarwing-vision-alpha'
assert_call 'ctr:alpha start lunarwing-vision-alpha'
assert_call 'wait:alpha lunarwing-vision-alpha'

# ── Required ports fail before image/container mutation ──────────────────────

# shellcheck disable=SC2034  # Updates state consumed by sourced mt-admin functions.
ensure_init_system() { INIT_SYSTEM=systemd; }
_ensure_tenant_image() { CALLS+=("ensure-image:$*"); }
ports_get() {
  case "$2" in
    vision_service) printf '20005' ;;
    vision_health) return 1 ;;
    *) return 1 ;;
  esac
}
CALLS=()
if start_tenant_vision alpha >/dev/null 2>&1; then
  fail "missing health port accepted"
else
  pass "missing health port fails start"
fi
assert_no_call_prefix 'ensure-image:'

# ── External instances: validation, lifecycle, and rollback ──────────────────

for external_url in \
  https://vision.example.test \
  https://vision.example.test:8443/ \
  'https://[2001:db8::1]:8443'; do
  if normalized="$(_normalize_external_vision_url "$external_url")" \
      && [[ "$normalized" == "${external_url%/}" ]]; then
    pass "accept external LunarVision origin: $external_url"
  else
    fail "rejected external LunarVision origin: $external_url"
  fi
done
for invalid_url in \
  http://vision.example.test \
  https://user@vision.example.test \
  https://vision.example.test/v1 \
  https://vision.example.test:0 \
  'https://vision example.test'; do
  if _normalize_external_vision_url "$invalid_url" >/dev/null; then
    fail "accepted unsafe external LunarVision URL: $invalid_url"
  else
    pass "reject unsafe external LunarVision URL: $invalid_url"
  fi
done

ports_set_vision_mode alpha external https://vision.example.test
[[ "$(tenant_vision_mode alpha)" == external ]] \
  && pass "external mode persists in registry" || fail "external mode registry persistence"
[[ "$(tenant_vision_external_url alpha)" == https://vision.example.test ]] \
  && pass "external URL persists in registry" || fail "external URL registry persistence"

EXTERNAL_HEALTHY=true
_wait_external_vision_healthy() {
  CALLS+=("wait-external:$1")
  [[ "$EXTERNAL_HEALTHY" == true ]]
}
ensure_container_runtime() { CALLS+=("ensure-runtime"); }
CALLS=()
# Imported above, then replaced with a call-recording mock later in this test.
# shellcheck disable=SC2218
start_tenant_vision alpha >/dev/null
assert_call 'wait-external:https://vision.example.test'
assert_no_call_prefix 'ensure-runtime'
CALLS=()
stop_tenant_vision alpha >/dev/null
assert_no_call_prefix 'ensure-runtime'

EXTERNAL_HEALTHY=false
if start_tenant_vision alpha >/dev/null 2>&1; then
  fail "unhealthy external LunarVision accepted"
else
  pass "unhealthy external LunarVision blocks tenant start"
fi
EXTERNAL_HEALTHY=true

remove_vision_systemd_assets() { CALLS+=("remove-systemd:$1"); }
remove_vision_openrc_assets() { CALLS+=("remove-openrc:$1"); }
CALLS=()
render_vision_systemd_assets alpha
assert_call 'remove-systemd:alpha'
assert_no_call_prefix 'render-quadlet:'
assert_no_call_prefix 'render-fallback:'
CALLS=()
render_vision_openrc_assets alpha
assert_call 'remove-openrc:alpha'
assert_no_call_prefix 'render-openrc:'
assert_no_call_prefix 'render-supervisor:'

mapfile -t external_writer_services < <(tenant_writer_services alpha)
if printf '%s\n' "${external_writer_services[@]}" | grep -q 'lunarwing-vision-alpha'; then
  fail "external LunarVision appears in writer service set"
else
  pass "external LunarVision is absent from writer service set"
fi

lunarwing_env="$(tenant_env_dir alpha)/lunarwing.env"
cat >"$lunarwing_env" <<'ENV'
VISION_SERVICE_URL=http://127.0.0.1:20005
VISION_AUTH_TOKEN=local-token
KEEP_ME=yes
ENV
chmod 0600 "$lunarwing_env"
_retire_local_vision_runtime() { CALLS+=("retire-local:$1"); }
RELOAD_OK=true
_reload_vision_consumer_if_running() {
  CALLS+=("reload:$1")
  [[ "$RELOAD_OK" == true ]]
}
CALLS=()
external_output="$ROOT/external-output"
configure_tenant_vision_external alpha https://vision.example.test:8443 external-token \
  >"$external_output"
assert_call 'retire-local:alpha'
assert_call 'reload:alpha'
[[ "$(_env_existing "$lunarwing_env" VISION_SERVICE_URL)" == https://vision.example.test:8443 ]] \
  && pass "external transition writes daemon URL" || fail "external transition daemon URL"
[[ "$(_env_existing "$lunarwing_env" VISION_AUTH_TOKEN)" == external-token ]] \
  && pass "external transition writes daemon token" || fail "external transition daemon token"
[[ "$(_env_existing "$lunarwing_env" KEEP_ME)" == yes ]] \
  && pass "external transition preserves unrelated env" || fail "external transition env preservation"
if grep -qF external-token "$external_output"; then
  fail "external transition leaked bearer token"
else
  pass "external transition does not print bearer token"
fi

# shellcheck disable=SC2034  # Consumed by sourced helper after mocked runtime detection.
CONTAINER_RT=true
start_tenant_vision() { CALLS+=("start-local:$1"); return 0; }
CALLS=()
configure_tenant_vision_local alpha >/dev/null
assert_call 'start-local:alpha'
assert_call 'reload:alpha'
[[ "$(tenant_vision_mode alpha)" == local ]] \
  && pass "local transition persists mode" || fail "local transition mode persistence"
[[ "$(_env_existing "$lunarwing_env" VISION_SERVICE_URL)" == http://127.0.0.1:20005 ]] \
  && pass "local transition restores loopback URL" || fail "local transition loopback URL"
[[ "$(_env_existing "$lunarwing_env" VISION_AUTH_TOKEN)" == test-only-token ]] \
  && pass "local transition restores local token" || fail "local transition token"

RELOAD_OK=false
if configure_tenant_vision_external alpha https://vision.example.test rollback-token \
    >/dev/null 2>&1; then
  fail "failed daemon reload reported external transition success"
else
  pass "failed daemon reload reports external transition failure"
fi
[[ "$(tenant_vision_mode alpha)" == local ]] \
  && pass "failed external transition restores local mode" || fail "failed external mode rollback"
[[ "$(_env_existing "$lunarwing_env" VISION_SERVICE_URL)" == http://127.0.0.1:20005 ]] \
  && pass "failed external transition restores local URL" || fail "failed external URL rollback"
[[ "$(_env_existing "$lunarwing_env" VISION_AUTH_TOKEN)" == test-only-token ]] \
  && pass "failed external transition restores local token" || fail "failed external token rollback"
RELOAD_OK=true

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all LunarVision mt-admin tests passed\n'
