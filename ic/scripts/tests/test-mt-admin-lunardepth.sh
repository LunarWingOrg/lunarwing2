#!/usr/bin/env bash
# shellcheck disable=SC2034  # fixture globals are consumed by sourced mt-admin helpers.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
registry="$TMP/ports.json"
cat >"$registry" <<'JSON'
{
  "version": 17,
  "range": {"start": 10000, "end": 19999},
  "extended_range": {"start": 20000, "end": 29999},
  "kimi_range": {"start": 30000, "end": 39999},
  "voice_range": {"start": 40000, "end": 49999},
  "opencode2_range": {"start": 51000, "end": 51999},
  "health_metrics_port": 50000,
  "tenants": {
    "demo": {
      "base_port": 10000,
      "ports": {"gateway": 10000, "pebble_wss": 10008},
      "extended_ports": {"opencode_wss": 20007},
      "kimi_ports": {"kimi_wss": 30000, "kimi_health": 30001},
      "voice_ports": {"voice_service": 40000, "voice_health": 40001},
      "opencode2_base": 51000,
      "opencode2_ports": {"opencode2_wss": 51000, "opencode2_health": 51001},
      "workers": {"pebble": false, "opencode": false, "kimi": false, "opencode2": false}
    }
  }
}
JSON

before="$(jq -S '.tenants.demo | del(.workers.lunardepth, .lunardepth_base, .lunardepth_ports)' "$registry")"
PORTS_REGISTRY="$registry" bash "$SCRIPT_DIR/migrate-ports-v18.sh" >/dev/null
after="$(jq -S '.tenants.demo | del(.workers.lunardepth, .lunardepth_base, .lunardepth_ports)' "$registry")"
[[ "$before" == "$after" ]]
[[ "$(jq -r '.version' "$registry")" == "18" ]]
[[ "$(jq -r '.lunardepth_range.start' "$registry")" == "52000" ]]
[[ "$(jq -r '.tenants.demo.lunardepth_ports.lunardepth_wss' "$registry")" == "52000" ]]
[[ "$(jq -r '.tenants.demo.lunardepth_ports.lunardepth_health' "$registry")" == "52001" ]]
[[ "$(jq -r '.tenants.demo.workers.lunardepth' "$registry")" == "false" ]]
checksum="$(sha256sum "$registry")"
PORTS_REGISTRY="$registry" bash "$SCRIPT_DIR/migrate-ports-v18.sh" >/dev/null
[[ "$checksum" == "$(sha256sum "$registry")" ]]

# Force a fresh v17 candidate whose custom range collides with existing assignments.
jq '.version = 17 | del(.lunardepth_range, .lunardepth_block_size) | .tenants.demo |= del(.lunardepth_base, .lunardepth_ports)' "$registry" >"$TMP/v17.json"
cp "$TMP/v17.json" "$registry"
checksum="$(sha256sum "$registry")"
if LUNARDEPTH_RANGE_START=10000 PORTS_REGISTRY="$registry" bash "$SCRIPT_DIR/migrate-ports-v18.sh" >/dev/null 2>&1; then
  echo "collision migration unexpectedly succeeded" >&2
  exit 1
fi
[[ "$checksum" == "$(sha256sum "$registry")" ]]
PORTS_REGISTRY="$registry" bash "$SCRIPT_DIR/migrate-ports-v18.sh" >/dev/null

# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/lunarwing-mt-admin.sh"
PORTS_REGISTRY="$registry"

# An existing v17/v18 tenant must receive ports, selection, daemon config, and
# literal (unquoted) task env values when the lane is configured.
(
  tenant_exists_in_registry() { return 0; }
  tenant_env_dir() { printf '%s/env' "$TMP"; }
  read_tenant_env_value() { [[ ! -f "$2" ]] || sed -n "s/^$3=//p" "$2"; }
  write_tenant_file_atomic() { cat >"$2"; }
  render_lunardepth_overlay() { LUNARDEPTH_OVERLAY_FILE="$TMP/overlay.yml"; }
  ensure_external_worker_config() { printf '%s:%s:%s\n' "$1" "$2" "$3" >>"$TMP/configure.calls"; }
  mkdir -p "$TMP/env"
  : >"$TMP/configure.calls"
  configure_lunardepth demo tensormoon::function_name::lunarwing \
    https://tensormoon.example.test/openai/v1 '' true true
  grep -q '^LUNARDEPTH_BASE_URL=https://tensormoon.example.test/openai/v1$' "$TMP/env/lunardepth.env"
  [[ "$(jq -r '.tenants.demo.workers.lunardepth' "$registry")" == "true" ]]
  [[ "$(jq -r '.tenants.demo.lunardepth_ports.lunardepth_wss' "$registry")" == "52000" ]]
  grep -q '^demo:lunardepth:lunardepth_wss$' "$TMP/configure.calls"
  configure_lunardepth demo '' '' test-only-key false false
  grep -q '^LUNARDEPTH_API_KEY=test-only-key$' "$TMP/env/lunardepth-task.env"
  grep -q '^LUNARDEPTH_ALLOW_INSECURE_HTTP=false$' "$TMP/env/lunardepth.env"
  grep -q '^LUNARDEPTH_ALLOW_PLACEHOLDER_KEY=false$' "$TMP/env/lunardepth.env"
)

# The shared lifecycle dispatcher must accept the new lane as well as its
# dedicated start_tenant_lunardepth function.
[[ "$(prepare_worker_lifecycle demo lunardepth)" == "demo lunardepth" ]]
if (prepare_worker_lifecycle demo not-a-worker) >/dev/null 2>&1; then
  echo "unknown worker unexpectedly accepted" >&2
  exit 1
fi

calls="$TMP/lifecycle.calls"
: >"$calls"
(
  sanitize_name() { printf '%s' "$1"; }
  tenant_worker_enabled() { return 0; }
  ensure_init_system() { INIT_SYSTEM=openrc; }
  ensure_container_runtime() { :; }
  ports_set_worker_desired_running() { printf 'desired:%s:%s\n' "$2" "$3" >>"$calls"; }
  start_tenant_lunardepth() { printf 'start:%s\n' "$1" >>"$calls"; }
  _ctr_inspect() { return 1; }
  openrc_worker_stop_disable() { printf 'stop:%s:%s\n' "$1" "$2" >>"$calls"; }
  stop_worker_task_container() { printf 'stop-task:%s:%s\n' "$1" "$2" >>"$calls"; }
  say() { :; }
  start_worker demo lunardepth
  stop_worker demo lunardepth
)
expected="$TMP/lifecycle.expected"
printf '%s\n' \
  'desired:lunardepth:true' 'start:demo' \
  'desired:lunardepth:false' 'stop:demo:lunardepth' 'stop-task:demo:lunardepth' >"$expected"
diff -u "$expected" "$calls"

echo "test-mt-admin-lunardepth: registry v18 migration and lifecycle validation checks passed"
