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

# errexit ignores a command negated with "!", so a negative assertion needs a
# helper that fails by itself.
refute() {
  if "$@"; then
    echo "unexpected success: $*" >&2
    exit 1
  fi
}

# An existing v17/v18 tenant must receive ports, selection, daemon config, and
# literal (unquoted) task env values when the lane is configured.
(
  tenant_exists_in_registry() { return 0; }
  tenant_env_dir() { printf '%s/env' "$TMP"; }
  read_tenant_env_value() { [[ ! -f "$2" ]] || sed -n "s/^$3=//p" "$2"; }
  write_tenant_file_atomic() { cat >"$2"; }
  # ZP-5: configure must not touch the installed overlay. It belongs to the
  # running units and is replaced only together with them, by start.
  render_lunardepth_overlay() { echo "configure rendered the overlay" >&2; exit 1; }
  ensure_external_worker_config() { printf '%s:%s:%s\n' "$1" "$2" "$3" >>"$TMP/configure.calls"; }
  _lunardepth_pair_running() { [[ -e "$TMP/pair-running" ]]; }
  start_tenant_lunardepth() { printf 'start:%s\n' "$1" >>"$TMP/configure.calls"; }
  mkdir -p "$TMP/env"
  : >"$TMP/configure.calls"
  configure_lunardepth demo tensormoon::function_name::lunarwing \
    https://tensormoon.example.test/openai/v1 '' true true >"$TMP/configure.out"
  grep -q '^LUNARDEPTH_BASE_URL=https://tensormoon.example.test/openai/v1$' "$TMP/env/lunardepth.env"
  [[ "$(jq -r '.tenants.demo.workers.lunardepth' "$registry")" == "true" ]]
  [[ "$(jq -r '.tenants.demo.lunardepth_ports.lunardepth_wss' "$registry")" == "52000" ]]
  grep -q '^demo:lunardepth:lunardepth_wss$' "$TMP/configure.calls"
  # Not running: nothing is started, and the operator is told how to apply it.
  refute grep -q '^start:' "$TMP/configure.calls"
  grep -q 'no key stored' "$TMP/configure.out"
  grep -q 'start-worker demo lunardepth' "$TMP/configure.out"
  configure_lunardepth demo '' '' test-only-key false false >"$TMP/configure.out"
  grep -q '^LUNARDEPTH_API_KEY=test-only-key$' "$TMP/env/lunardepth-task.env"
  grep -q '^LUNARDEPTH_ALLOW_INSECURE_HTTP=false$' "$TMP/env/lunardepth.env"
  grep -q '^LUNARDEPTH_ALLOW_PLACEHOLDER_KEY=false$' "$TMP/env/lunardepth.env"
  grep -q 'key staged only in the task env' "$TMP/configure.out"
  # Running: the new configuration is applied in the same command, so the unit
  # never keeps an overlay digest that no longer matches its environment.
  : >"$TMP/pair-running"
  configure_lunardepth demo tensormoon::other_function::lunarwing '' '' preserve preserve >"$TMP/configure.out"
  grep -q '^start:demo$' "$TMP/configure.calls"
  grep -q '^LUNARDEPTH_MODEL=tensormoon::other_function::lunarwing$' "$TMP/env/lunardepth-task.env"
  grep -q '^LUNARDEPTH_API_KEY=test-only-key$' "$TMP/env/lunardepth-task.env"
  rm -f "$TMP/pair-running"
  # ZP-8: a key handed over while the unauthenticated-gateway exception is on
  # would be dropped without a word. Refuse, and leave both files as they were.
  before_env="$(sha256sum "$TMP/env/lunardepth.env" "$TMP/env/lunardepth-task.env")"
  if (configure_lunardepth demo '' '' another-key false true) >"$TMP/configure.out" 2>&1; then
    echo "configure accepted a key together with the unauthenticated-gateway exception" >&2
    exit 1
  fi
  grep -q -- '--require-api-key' "$TMP/configure.out"
  [[ "$before_env" == "$(sha256sum "$TMP/env/lunardepth.env" "$TMP/env/lunardepth-task.env")" ]]
  # An invalid route is refused before anything is written.
  if (configure_lunardepth demo '' 'http://203.0.113.9/openai/v1' '' false false) >/dev/null 2>&1; then
    echo "configure accepted cleartext HTTP to a public address" >&2
    exit 1
  fi
  [[ "$before_env" == "$(sha256sum "$TMP/env/lunardepth.env" "$TMP/env/lunardepth-task.env")" ]]
  # The upgrade preflight validates the stored route and renders nothing.
  opencode2_builder_supported() { return 0; }
  lunardepth_landlock_floor_problem() { return 1; }
  lunardepth_preflight demo
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

# ── ZP-3: on OpenRC the bridge is created with --no-healthcheck ──────────────
# Without the flag the bridge keeps the image healthcheck, and every later start
# removes the running bridge again to get rid of it.
(
  : >"$TMP/bridge.args"
  _openrc_podman_uses_init_health() { return 0; }
  _reconcile_openrc_podman_healthcheck() { :; }
  prepare_worker_task_launch() { TASK_WORKSPACE_DIR="$TMP/workspace"; TASK_USERNS_ARGS=(); TASK_ENV_FLAGS=(); TASK_MOUNTS=(); TASK_RESTART_ARG=(); TASK_WORKSPACE_ARGS=(); }
  start_worker_task_container() { :; }
  worker_egress_reconcile_container() { :; }
  _register_worker_unit() { :; }
  _ctr_inspect() { return 1; }
  say() { :; }
  tenant_env_dir() { printf '%s/env' "$TMP"; }
  resolve_worker_auth_token() { printf 'test-only-hub-token-0123456789abcdef0123456789abcdef'; }
  ports_get() { [[ "$2" != lunardepth_health ]] || printf '52001'; }
  worker_bridge_uid() { printf '10105'; }
  worker_ipc_volume_name() { printf 'lunarwing-ipc-%s-%s' "$1" "$2"; }
  worker_egress_network_name() { printf 'lw-egress-%s' "$1"; }
  _store_container_hash() { :; }
  _ctr() { shift; printf '%s\n' "$@" >"$TMP/bridge.args"; }
  MT_ROOTLESS=true
  LUNARDEPTH_CONTAINER_GID=10005
  WORKER_EGRESS_LAUNCH_ARGS=()
  _start_lunardepth_imperative demo contract-v1 52000 --userns keep-id
  grep -qx -- '--no-healthcheck' "$TMP/bridge.args"
  grep -qx -- '--userns' "$TMP/bridge.args"
  grep -qx -- 'keep-id' "$TMP/bridge.args"
  # The extra arguments are run options: they come before the image name.
  image_line="$(grep -nx -- 'lunarwing-worker-lunardepth:latest' "$TMP/bridge.args" | cut -d: -f1)"
  flag_line="$(grep -nx -- '--no-healthcheck' "$TMP/bridge.args" | cut -d: -f1)"
  (( flag_line < image_line ))
  # On a host where the init system does not own health, nothing is added.
  _openrc_podman_uses_init_health() { return 1; }
  _start_lunardepth_imperative demo contract-v1 52000
  refute grep -qx -- '--no-healthcheck' "$TMP/bridge.args"
)

# ── ZP-5, ZP-6: the image renders its own overlay, in a contained container ──
(
  tenant_exists_in_registry() { return 0; }
  tenant_env_dir() { printf '%s/env' "$TMP"; }
  read_tenant_env_value() { [[ ! -f "$2" ]] || sed -n "s/^$3=//p" "$2"; }
  _ctr() { shift; printf '%s\n' "$@" >"$TMP/render.args"; printf 'rendered-overlay\n'; }
  _lunardepth_route demo
  [[ "$(_lunardepth_render_in_image demo)" == "rendered-overlay" ]]
  for expected in run --rm --pull=never none --read-only ALL no-new-privileges --no-healthcheck /usr/bin/python3 \
      lunarwing-worker-lunardepth:latest -I /app/scripts/render_overlay.py \
      /app/config/lunarwing-lane.patch.yml --stdout tensormoon::other_function::lunarwing; do
    grep -qx -- "$expected" "$TMP/render.args" || { echo "render container lacks: $expected" >&2; exit 1; }
  done
  # Nothing from the operator checkout is mounted into or passed to that container.
  refute grep -q -- "$SCRIPT_DIR" "$TMP/render.args"
  refute grep -qx -- '--volume' "$TMP/render.args"
  refute grep -qx -- '-v' "$TMP/render.args"
  refute grep -qx -- '--privileged' "$TMP/render.args"
  # The host accepts a bounded, non-empty result only.
  : >"$TMP/empty.yml"
  refute _lunardepth_overlay_size_ok "$TMP/empty.yml"
  head -c 65536 /dev/zero >"$TMP/max.yml"
  _lunardepth_overlay_size_ok "$TMP/max.yml"
  head -c 65537 /dev/zero >"$TMP/over.yml"
  refute _lunardepth_overlay_size_ok "$TMP/over.yml"
  refute _lunardepth_overlay_size_ok "$TMP/missing.yml"
)

# ── ZP-6: the out-of-band verifier never reads the image as root ─────────────
(
  mkdir -p "$TMP/oob-source"
  cp -R "$SCRIPT_DIR/../../lunardepth4lunarwing" "$TMP/oob-source/"
  rm -rf "$TMP/oob-source/lunardepth4lunarwing/scripts/node_modules"
  : >"$TMP/oob.calls"
  fake_rt() {
    case "$1" in
      create) printf 'cid123\n' ;;
      export) : >"$3" ;;
      image) printf '[]\n' ;;
      run) shift; printf 'run:%s\n' "$*" >>"$TMP/oob.calls" ;;
      rm) : ;;
    esac
  }
  CONTAINER_RT=fake_rt
  id() { case "$*" in '-u nobody'|'-g nobody') printf '65534\n' ;; *) command id "$@" ;; esac; }
  chown() { :; }
  setpriv() { printf 'setpriv:%s\n' "$*" >>"$TMP/oob.calls"; }
  _verify_lunardepth_image_out_of_band sha256:test "$TMP/oob-source"
  grep -q '^run:.*--user 65534:65534 ' "$TMP/oob.calls"
  grep -q '^run:.*--cap-drop ALL ' "$TMP/oob.calls"
  grep -q '^setpriv:--reuid=65534 --regid=65534 --clear-groups --no-new-privs -- env -i PATH=/usr/bin:/bin PYTHONDONTWRITEBYTECODE=1 ' "$TMP/oob.calls"
  grep -q '^setpriv:.* -I /var/tmp/lw-lunardepth-oob\.[A-Za-z0-9]*/lane/scripts/verify_exposure_out_of_band\.py ' "$TMP/oob.calls"
  # The verifier reads the private snapshot, not the checkout it was copied from.
  refute grep -q -- "$TMP/oob-source" "$TMP/oob.calls"
  # A failing verifier fails the build step.
  setpriv() { return 1; }
  if (_verify_lunardepth_image_out_of_band sha256:test "$TMP/oob-source") >/dev/null 2>&1; then
    echo "a failed out-of-band verification was accepted" >&2
    exit 1
  fi
  # Without an unprivileged account there is no verification at all.
  id() { case "$*" in '-u nobody'|'-g nobody') return 1 ;; *) command id "$@" ;; esac; }
  if (_verify_lunardepth_image_out_of_band sha256:test "$TMP/oob-source") >/dev/null 2>&1; then
    echo "out-of-band verification ran without an unprivileged account" >&2
    exit 1
  fi
)

# The verifier itself must start in isolated mode, where the script directory
# is not on the import path. The interpreter is named by its full path, as
# mt-admin does: the cleared PATH need not contain it (CI's is in /usr/local/bin).
env -i PATH=/usr/bin:/bin PYTHONDONTWRITEBYTECODE=1 "$(command -v python3)" -I \
  "$SCRIPT_DIR/../../lunardepth4lunarwing/scripts/verify_exposure_out_of_band.py" --help >/dev/null

# ── Kernel floor: full Landlock enforcement needs ABI 5, Linux 6.10 ──────────
(
  printf 'capability,landlock,yama\n' >"$TMP/lsm-with"
  printf 'capability,yama,bpf\n' >"$TMP/lsm-without"
  LUNARDEPTH_LSM_FILE="$TMP/lsm-with"
  # The function under test has a local named "release"; the fake must not use that name.
  uname() { printf '%s\n' "$fake_kernel"; }
  for fake_kernel in 6.10.0 6.18.54-2-lts 7.0.1 10.2.0-rc1; do
    refute lunardepth_landlock_floor_problem
  done
  for fake_kernel in 6.9.12-arch1 6.1.0-18-amd64 5.15.0-91-generic 4.19.0; do
    [[ "$(lunardepth_landlock_floor_problem)" == *"older than Linux 6.10"* ]]
  done
  uname() { printf '6.18.54-2-lts\n'; }
  LUNARDEPTH_LSM_FILE="$TMP/lsm-without"
  [[ "$(lunardepth_landlock_floor_problem)" == *"not in the active LSM list"* ]]
  # An unreadable list is unknown, not a refusal.
  LUNARDEPTH_LSM_FILE="$TMP/no-such-file"
  refute lunardepth_landlock_floor_problem
  # The preflight stops with the reason.
  LUNARDEPTH_LSM_FILE="$TMP/lsm-without"
  _lunardepth_route() { :; }
  opencode2_builder_supported() { return 0; }
  if (lunardepth_preflight demo) >"$TMP/floor.out" 2>&1; then
    echo "the preflight passed on a host without landlock" >&2
    exit 1
  fi
  grep -q 'needs full Landlock enforcement' "$TMP/floor.out"
)

# ── The in-library v18 migration refuses a colliding or out-of-range result ──
(
  PORTS_REGISTRY="$TMP/lib-ports.json"
  PORTS_LOCK_HELD=true
  # A v17 registry whose second tenant sits in slot 100: its LunarDepth block
  # would be 53000, outside 52000-52999, and its opencode2 port is already 52000.
  jq '.version = 17 | del(.lunardepth_range, .lunardepth_block_size)
      | .tenants.demo |= del(.lunardepth_base, .lunardepth_ports, .workers.lunardepth)
      | .tenants.far = {"base_port": 11000, "ports": {"gateway": 11000},
          "opencode2_base": 52000, "opencode2_ports": {"opencode2_wss": 52000, "opencode2_health": 52001},
          "workers": {"opencode2": true}}' "$registry" >"$PORTS_REGISTRY"
  checksum="$(sha256sum "$PORTS_REGISTRY")"
  if (ports_migrate_v18) >"$TMP/lib-migrate.out" 2>&1; then
    echo "the in-library v18 migration accepted colliding LunarDepth ports" >&2
    exit 1
  fi
  grep -q 'the registry is unchanged' "$TMP/lib-migrate.out"
  [[ "$checksum" == "$(sha256sum "$PORTS_REGISTRY")" ]]
  [[ -z "$(find "$TMP" -maxdepth 1 -name 'lib-ports.json.tmp.*' -print -quit)" ]]
  # An ordinary registry still migrates.
  jq '.version = 17 | del(.lunardepth_range, .lunardepth_block_size)
      | .tenants.demo |= del(.lunardepth_base, .lunardepth_ports, .workers.lunardepth)' "$registry" >"$PORTS_REGISTRY"
  ports_migrate_v18 >/dev/null
  [[ "$(jq -r '.version' "$PORTS_REGISTRY")" == "18" ]]
  [[ "$(jq -r '.tenants.demo.lunardepth_ports.lunardepth_wss' "$PORTS_REGISTRY")" == "52000" ]]
)

# ── ZP-7: no two tenants share a model-visible key ───────────────────────────
(
  keys="$TMP/fleet-keys"
  mkdir -p "$keys"
  printf 'key-for-ersa\n' >"$keys/ersa.key"
  printf 'key-for-pandia\n' >"$keys/pandia.key"
  lunardepth_fleet_keys_check '' "$keys" false ersa pandia
  [[ "$(lunardepth_fleet_key pandia "$keys")" == "key-for-pandia" ]]
  lunardepth_fleet_keys_check one-key '' false ersa
  lunardepth_fleet_keys_check '' '' true ersa pandia
  expect_refusal() {  # <text in the message> <arguments of the check...>
    local text="$1"; shift
    if (lunardepth_fleet_keys_check "$@") >"$TMP/fleet.out" 2>&1; then
      echo "fleet key check accepted: $*" >&2
      exit 1
    fi
    grep -q -- "$text" "$TMP/fleet.out" || { cat "$TMP/fleet.out" >&2; exit 1; }
  }
  expect_refusal 'same model-visible key' one-key '' false ersa pandia
  expect_refusal 'not both' one-key "$keys" false ersa
  expect_refusal 'no key would be stored' '' "$keys" true ersa
  expect_refusal 'no key would be stored' one-key '' true ersa
  expect_refusal 'not readable' '' "$keys" false ersa selene
  cp "$keys/ersa.key" "$keys/selene.key"
  expect_refusal "tenants 'ersa' and 'selene' have the same" '' "$keys" false ersa pandia selene
  # The refusal never repeats a key.
  refute grep -q 'key-for-' "$TMP/fleet.out"
  ln -s "$keys/pandia.key" "$keys/linked.key"
  expect_refusal 'must not be a symlink' '' "$keys" false linked
)

# ── ZP-4: an opencode2 build for a tenant at an older revision still archives ──
# worker_candidate_config.py exists only from the revision that split it out of
# the opencode2 verifier. Naming it for an older revision made git archive fail.
(
  repo="$TMP/archive-repo"
  mkdir -p "$repo/opencode2.4lunarwing" "$repo/.gitlab/scripts"
  git -C "$repo" init -q
  git -C "$repo" config user.name harness
  git -C "$repo" config user.email harness@example.invalid
  printf 'FROM scratch\n' >"$repo/opencode2.4lunarwing/Dockerfile"
  printf '# verifier\n' >"$repo/.gitlab/scripts/verify-opencode2-exposure-out-of-band.py"
  git -C "$repo" add -A
  git -C "$repo" commit -q -m 'before the shared module'
  old_revision="$(git -C "$repo" rev-parse HEAD)"
  printf '# shared\n' >"$repo/.gitlab/scripts/worker_candidate_config.py"
  git -C "$repo" add -A
  git -C "$repo" commit -q -m 'with the shared module'
  new_revision="$(git -C "$repo" rev-parse HEAD)"
  LUNARWING_ROOT="$repo"
  LUNARWING_WORKER_BUILD_TMPDIR="$TMP"
  WORKER_BUILD_SOURCE_REVISION="$old_revision"
  _archive_worker_source "$repo" opencode2.4lunarwing
  [[ -f "$WORKER_BUILD_ARCHIVE_ROOT/.gitlab/scripts/verify-opencode2-exposure-out-of-band.py" ]]
  [[ ! -e "$WORKER_BUILD_ARCHIVE_ROOT/.gitlab/scripts/worker_candidate_config.py" ]]
  old_root="$WORKER_BUILD_ARCHIVE_ROOT"
  WORKER_BUILD_SOURCE_REVISION="$new_revision"
  _archive_worker_source "$repo" opencode2.4lunarwing
  [[ -f "$WORKER_BUILD_ARCHIVE_ROOT/.gitlab/scripts/worker_candidate_config.py" ]]
  # The verification snapshot accepts the older layout too, and still copies
  # the module when the revision has it.
  for name in scripts config; do mkdir -p "$old_root/opencode2.4lunarwing/$name" "$WORKER_BUILD_ARCHIVE_ROOT/opencode2.4lunarwing/$name"; done
  for name in entrypoint.sh entrypoint-split.sh health_server.py agent_comm_protocol.json; do
    : >"$old_root/opencode2.4lunarwing/$name"
    : >"$WORKER_BUILD_ARCHIVE_ROOT/opencode2.4lunarwing/$name"
  done
  mkdir -p "$TMP/snap-old" "$TMP/snap-new"
  snapshot="$(_opencode2_snapshot_checkout "$old_root" "$old_root/opencode2.4lunarwing" "$TMP/snap-old")"
  [[ -f "$snapshot/.gitlab/scripts/verify-opencode2-exposure-out-of-band.py" ]]
  [[ ! -e "$snapshot/.gitlab/scripts/worker_candidate_config.py" ]]
  snapshot="$(_opencode2_snapshot_checkout "$WORKER_BUILD_ARCHIVE_ROOT" "$WORKER_BUILD_ARCHIVE_ROOT/opencode2.4lunarwing" "$TMP/snap-new")"
  [[ -f "$snapshot/.gitlab/scripts/worker_candidate_config.py" ]]
)

echo "test-mt-admin-lunardepth: registry v18 migration, lifecycle, bridge flags, overlay rendering, verifier isolation, fleet keys and archive checks passed"
