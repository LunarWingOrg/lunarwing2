#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"

# Preserve the real implementation before later blocks stub it. The
# WI#133/#134 tenant_writers_active container-branch tests at the bottom of
# this file restore it via eval so they exercise the REAL classification logic
# (last function definition wins in bash — a global stub would shadow it). The
# WI#137 poller tests restore await_writer_quiescence the same way; only its
# tenant_writers_active dependency is stubbed per-case.
_TWA_REAL_DEF="$(declare -f tenant_writers_active)"
_AWAIT_QUIESCE_REAL_DEF="$(declare -f await_writer_quiescence)"
_STOP_SYSTEMD_REAL_DEF="$(declare -f stop_tenant_systemd)"
_STOP_OPENRC_REAL_DEF="$(declare -f stop_tenant_openrc)"

failures=0
CALLS=()
fixture="$(mktemp -d)"
trap 'command rm -rf "$fixture"' EXIT
OPENRC_INITD_DIR="$fixture/init.d"
OPENRC_CONFD_DIR="$fixture/conf.d"
OPENRC_RUNLEVELS_DIR="$fixture/runlevels"
mkdir -p "$OPENRC_INITD_DIR" "$OPENRC_CONFD_DIR" "$OPENRC_RUNLEVELS_DIR/default"

fail() {
  printf 'FAIL: %s\n' "$1"
  failures=$((failures + 1))
}

assert_call() {
  local expected="$1"
  local call
  for call in "${CALLS[@]}"; do
    [[ "$call" == "$expected" ]] && { printf 'PASS: %s\n' "$expected"; return; }
  done
  fail "missing call: $expected"
}

assert_no_call() {
  local unexpected="$1" call
  for call in "${CALLS[@]}"; do
    [[ "$call" != "$unexpected" ]] || { fail "unexpected call: $unexpected"; return; }
  done
  printf 'PASS: no %s\n' "$unexpected"
}

assert_before() {
  local first="$1" second="$2" first_index=-1 second_index=-1 index
  for index in "${!CALLS[@]}"; do
    [[ "${CALLS[$index]}" == "$first" ]] && first_index="$index"
    [[ "${CALLS[$index]}" == "$second" ]] && second_index="$index"
  done
  if [[ "$first_index" -ge 0 && "$second_index" -gt "$first_index" ]]; then
    printf 'PASS: %s before %s\n' "$first" "$second"
  else
    fail "expected '$first' before '$second'"
  fi
}

sanitize_name() { printf '%s' "$1"; }
tenant_exists_in_registry() { return 0; }
tenant_worker_enabled() { return 0; }
ensure_container_runtime() {
  # Globals consumed by sourced lifecycle helpers.
  # shellcheck disable=SC2034
  CONTAINER_RT="podman"
  MT_ROOTLESS="${MT_ROOTLESS:-true}"
}
ensure_init_system() { :; }
podman_supports_quadlet() { return 0; }
say() { :; }
_systemctl_user() { CALLS+=("systemctl:$*"); }
rc-service() { CALLS+=("rc:$*"); }
rc-update() { CALLS+=("rc-update:$*"); }
ports_set_worker_desired_running() { CALLS+=("desired:$*"); }
_ctr() {
  CALLS+=("ctr:$*")
  return 0
}
start_tenant_opencode() { CALLS+=("start-opencode:$1"); }
start_tenant_kimi() { CALLS+=("start-kimi:$1"); }

INIT_SYSTEM="systemd"
CALLS=()
stop_worker alice opencode
assert_call "systemctl:alice stop lunarwing-opencode-alice.service"

CALLS=()
kill_worker alice opencode
assert_call "systemctl:alice stop --no-block lunarwing-opencode-alice.service"
assert_call "ctr:alice stop --time 0 lunarwing-opencode-alice"
assert_call "systemctl:alice stop lunarwing-opencode-alice.service"
assert_before \
  "systemctl:alice stop --no-block lunarwing-opencode-alice.service" \
  "ctr:alice stop --time 0 lunarwing-opencode-alice"
assert_before \
  "ctr:alice stop --time 0 lunarwing-opencode-alice" \
  "systemctl:alice stop lunarwing-opencode-alice.service"

test_pebble_quadlet_bounce_recovery() (
  local systemctl_stop_fails="false"
  local call index reset_index=-1 start_index=-1
  local unit_stop_seen="false" container_stop_seen="false"

  ensure_container_runtime() {
    CONTAINER_RT="podman"
    MT_ROOTLESS="true"
  }
  ensure_init_system() { INIT_SYSTEM="systemd"; }
  ports_get() {
    [[ "$2" == "pebble_wss" ]] && printf '19091\n'
  }
  _ensure_tenant_image() { return 0; }
  _wait_user_manager() { :; }
  tenant_quadlet_dir() { printf '%s/quadlets\n' "$fixture"; }
  render_worker_quadlet() { :; }
  _container_config_changed() { return 1; }
  _store_container_hash() { :; }
  _systemctl_user() {
    CALLS+=("systemctl:$*")
    [[ "$systemctl_stop_fails" != "true" || "${2:-}" != "stop" ]]
  }
  _ctr() {
    CALLS+=("ctr:$*")
    return 0
  }

  CALLS=()
  stop_tenant_pebble alice
  for call in "${CALLS[@]}"; do
    [[ "$call" == "systemctl:alice stop lunarwing-pebble-alice.service" ]] \
      && unit_stop_seen="true"
    [[ "$call" == "ctr:alice stop lunarwing-pebble-alice" ]] \
      && container_stop_seen="true"
  done
  if [[ "$unit_stop_seen" != "true" || "$container_stop_seen" == "true" ]]; then
    printf 'Pebble Quadlet stop did not prefer its systemd unit\n' >&2
    return 1
  fi

  CALLS=()
  start_tenant_pebble alice
  for index in "${!CALLS[@]}"; do
    case "${CALLS[$index]}" in
      "systemctl:alice reset-failed lunarwing-pebble-alice.service") reset_index="$index" ;;
      "systemctl:alice start lunarwing-pebble-alice.service") start_index="$index" ;;
    esac
  done
  if (( reset_index < 0 || start_index <= reset_index )); then
    printf 'Pebble Quadlet start did not clear failure state before restart\n' >&2
    return 1
  fi

  systemctl_stop_fails="true"
  unit_stop_seen="false"
  container_stop_seen="false"
  CALLS=()
  stop_tenant_pebble alice
  for call in "${CALLS[@]}"; do
    [[ "$call" == "systemctl:alice stop lunarwing-pebble-alice.service" ]] \
      && unit_stop_seen="true"
    [[ "$call" == "ctr:alice stop lunarwing-pebble-alice" ]] \
      && container_stop_seen="true"
  done
  if [[ "$unit_stop_seen" != "true" || "$container_stop_seen" != "true" ]]; then
    printf 'Pebble Quadlet stop omitted the container fallback\n' >&2
    return 1
  fi
)

if test_pebble_quadlet_bounce_recovery; then
  printf 'PASS: Pebble Quadlet tenant bounce recovery\n'
else
  fail "Pebble Quadlet tenant bounce recovery"
fi

# Goal #10: kimi/opencode/opencode2 stop paths must prefer their Quadlet
# systemd unit on systemd+rootless, with a container fallback when the unit
# stop fails — the same contract WI#128 gave pebble. A bare container stop
# under an active unit would be resurrected by Restart=on-failure.
test_worker_quadlet_stop_parity() (
  local worker unit_stop_seen container_stop_seen call

  ensure_container_runtime() {
    CONTAINER_RT="podman"
    MT_ROOTLESS="true"
  }
  ensure_init_system() { INIT_SYSTEM="systemd"; }
  _systemctl_user() {
    CALLS+=("systemctl:$*")
    [[ "$systemctl_stop_fails" != "true" || "${2:-}" != "stop" ]]
  }
  _ctr() {
    CALLS+=("ctr:$*")
    return 0
  }

  for worker in kimi opencode opencode2; do
    systemctl_stop_fails="false"
    unit_stop_seen="false"
    container_stop_seen="false"
    CALLS=()
    "stop_tenant_${worker}" alice
    for call in "${CALLS[@]}"; do
      [[ "$call" == "systemctl:alice stop lunarwing-${worker}-alice.service" ]] \
        && unit_stop_seen="true"
      [[ "$call" == "ctr:alice stop lunarwing-${worker}-alice" ]] \
        && container_stop_seen="true"
    done
    if [[ "$unit_stop_seen" != "true" || "$container_stop_seen" == "true" ]]; then
      printf '%s Quadlet stop did not prefer its systemd unit\n' "$worker" >&2
      return 1
    fi

    # Unit stop failing must fall back to the container stop, never leave the
    # worker running under a live unit.
    systemctl_stop_fails="true"
    unit_stop_seen="false"
    container_stop_seen="false"
    CALLS=()
    "stop_tenant_${worker}" alice
    for call in "${CALLS[@]}"; do
      [[ "$call" == "systemctl:alice stop lunarwing-${worker}-alice.service" ]] \
        && unit_stop_seen="true"
      [[ "$call" == "ctr:alice stop lunarwing-${worker}-alice" ]] \
        && container_stop_seen="true"
    done
    if [[ "$unit_stop_seen" != "true" || "$container_stop_seen" != "true" ]]; then
      printf '%s Quadlet stop omitted the container fallback\n' "$worker" >&2
      return 1
    fi
  done
)

if test_worker_quadlet_stop_parity; then
  printf 'PASS: worker Quadlet stop parity (kimi/opencode/opencode2)\n'
else
  fail "worker Quadlet stop parity (kimi/opencode/opencode2)"
fi

# Override consumed by sourced stop_tenant dispatcher.
# shellcheck disable=SC2034
INIT_SYSTEM="openrc"
for worker in opencode kimi; do
  touch "$OPENRC_INITD_DIR/lunarwing-${worker}-alice" \
    "$OPENRC_INITD_DIR/lunarwing-${worker}-alice-sup"
  chmod +x "$OPENRC_INITD_DIR/lunarwing-${worker}-alice" \
    "$OPENRC_INITD_DIR/lunarwing-${worker}-alice-sup"
  touch "$OPENRC_RUNLEVELS_DIR/default/lunarwing-${worker}-alice" \
    "$OPENRC_RUNLEVELS_DIR/default/lunarwing-${worker}-alice-sup"
done
CALLS=()
stop_worker alice opencode
assert_call "desired:alice opencode false"
assert_call "rc:lunarwing-opencode-alice-sup stop"
assert_call "rc:lunarwing-opencode-alice stop"
assert_call "rc-update:del lunarwing-opencode-alice-sup default"
assert_call "rc-update:del lunarwing-opencode-alice default"
assert_before "desired:alice opencode false" "rc:lunarwing-opencode-alice-sup stop"
assert_before "rc:lunarwing-opencode-alice-sup stop" "rc:lunarwing-opencode-alice stop"

CALLS=()
kill_worker alice opencode
assert_call "desired:alice opencode false"
assert_call "rc:lunarwing-opencode-alice-sup stop"
assert_call "rc-update:del lunarwing-opencode-alice-sup default"
assert_call "rc-update:del lunarwing-opencode-alice default"
assert_call "ctr:alice stop --time 0 lunarwing-opencode-alice"
assert_call "rc:lunarwing-opencode-alice stop"
assert_before \
  "rc:lunarwing-opencode-alice-sup stop" \
  "ctr:alice stop --time 0 lunarwing-opencode-alice"

CALLS=()
start_worker alice opencode
assert_call "desired:alice opencode true"
assert_call "start-opencode:alice"
assert_before "desired:alice opencode true" "start-opencode:alice"

CALLS=()
start_worker alice kimi
assert_call "desired:alice kimi true"
assert_call "start-kimi:alice"

CALLS=()
stop_worker alice kimi
assert_call "desired:alice kimi false"
assert_call "rc:lunarwing-kimi-alice-sup stop"
assert_call "rc:lunarwing-kimi-alice stop"

# Destructive reconciliation requires explicit registry evidence. Unknown or
# malformed state must not authorize stopping a live worker.
PORTS_REGISTRY="$fixture/ports.json"
jq -n '{tenants:{alice:{workers:{pebble:true,opencode:true,kimi:false},worker_runtime:{opencode:{desired_running:false}}}}}' >"$PORTS_REGISTRY"
if tenant_worker_openrc_stop_requested alice opencode; then
  printf 'PASS: explicit desired-running false authorizes cleanup\n'
else
  fail "explicit desired-running false did not authorize cleanup"
fi
if tenant_worker_openrc_stop_requested alice kimi; then
  printf 'PASS: explicit unselected worker authorizes cleanup\n'
else
  fail "explicit unselected worker did not authorize cleanup"
fi
if tenant_worker_openrc_stop_requested alice pebble; then
  fail "selected worker with absent intent authorized cleanup"
else
  printf 'PASS: selected worker with absent intent remains running\n'
fi
printf '{malformed' >"$PORTS_REGISTRY"
if tenant_worker_openrc_stop_requested alice opencode; then
  fail "malformed registry authorized destructive cleanup"
else
  printf 'PASS: malformed registry fails safe\n'
fi

# start-tenant must clean stale supervisors instead of starting workers whose
# selection or desired-running state says they should stay down.
tenant_worker_openrc_stop_requested() { [[ "$2" != "pebble" ]]; }
openrc_worker_stop_disable() { CALLS+=("reconcile-stop:$*"); }
tenant_weechat_enabled() { return 1; }
tenant_darkirc_enabled() { return 1; }
tenant_proxy_enabled() { return 1; }
CALLS=()
start_tenant_openrc alice
assert_call "reconcile-stop:alice opencode"
assert_call "reconcile-stop:alice kimi"
assert_no_call "rc:lunarwing-opencode-alice-sup start"
assert_no_call "rc:lunarwing-kimi-alice-sup start"

INIT_SYSTEM="systemd"
MT_ROOTLESS="false"
CALLS=()
stop_worker alice opencode
assert_call "ctr:alice stop lunarwing-opencode-alice"

if (stop_worker alice arbitrary >/dev/null 2>&1); then
  fail "unknown worker accepted"
else
  printf 'PASS: unknown worker rejected\n'
fi

if (stop_worker alice nanocode >/dev/null 2>&1); then
  fail "retired Nanocode worker accepted"
else
  printf 'PASS: retired Nanocode worker rejected\n'
fi

if (render_worker_quadlet alice nanocode 8443 >/dev/null 2>&1); then
  fail "retired Nanocode Quadlet accepted"
else
  printf 'PASS: retired Nanocode Quadlet rejected\n'
fi

# ── Legacy Nanocode lifecycle ────────────────────────────────────────────────

tenant_home() { printf '%s/home/%s' "$fixture" "$1"; }
tenant_quadlet_dir() { printf '%s/quadlets' "$fixture"; }
id() {
  if [[ "${1:-}" == "-u" ]]; then printf '1000\n'; return 0; fi
  return 1
}
getent() {
  [[ "${1:-}" == "passwd" ]] || return 1
  printf 'alice:x:1000:1000::%s/home/alice:/bin/bash\n' "$fixture"
}
command() {
  if [[ "${1:-}" == "-v" && ( "${2:-}" == "docker" || "${2:-}" == "podman" ) ]]; then
    return 0
  fi
  builtin command "$@"
}

_stop_nanocode_container_store() {
  local label="$1" container="$2"
  shift 2
  CALLS+=("store:$label:$container:$*")
}

CALLS=()
_stop_nanocode_containers alice
assert_call "store:root Docker store:lunarwing-nanocode-alice:docker"
assert_call "store:root Podman store:lunarwing-nanocode-alice:podman"
assert_call "store:tenant alice rootless Podman store:lunarwing-nanocode-alice:sudo -u alice env HOME=$fixture/home/alice XDG_RUNTIME_DIR=/run/user/1000 podman"

mkdir -p "$fixture/quadlets" "$fixture/home/alice/.config/systemd/user"
touch "$fixture/quadlets/lunarwing-nanocode-alice.container"
_systemctl_user() {
  CALLS+=("systemctl:$*")
  case "${2:-}" in
    show) printf 'loaded\n' ;;
    is-active) return 1 ;;
  esac
}
INIT_SYSTEM="systemd"
CALLS=()
_stop_nanocode_supervision alice
assert_call "systemctl:alice stop lunarwing-nanocode-alice.service"

touch "$OPENRC_INITD_DIR/lunarwing-nanocode-alice" \
  "$OPENRC_INITD_DIR/lunarwing-nanocode-alice-sup"
INIT_SYSTEM="openrc"
CALLS=()
_stop_nanocode_supervision alice
assert_call "rc:lunarwing-nanocode-alice-sup stop"
assert_call "rc:lunarwing-nanocode-alice stop"

INIT_SYSTEM="systemd"
CALLS=()
uninstall_tenant_systemd alice
assert_no_call "ctr:alice rm -f lunarwing-nanocode-alice"
assert_call "ctr:alice rm -f lunarwing-pebble-alice"
assert_call "ctr:alice rm -f lunarwing-opencode-alice"
assert_call "ctr:alice rm -f lunarwing-kimi-alice"

stop_tenant_systemd() { CALLS+=("stop-systemd:$1"); }
stop_tenant_openrc() { CALLS+=("stop-openrc:$1"); }
stop_tenant_vision() { CALLS+=("stop-vision:$1"); }
stop_tenant_pebble() { CALLS+=("stop-pebble:$1"); }
stop_tenant_opencode() { CALLS+=("stop-opencode:$1"); }
stop_tenant_kimi() { CALLS+=("stop-kimi:$1"); }
stop_tenant_postgres() { CALLS+=("stop-postgres:$1"); }
RETIRE_FAILURE="false"
retire_tenant_nanocode() {
  CALLS+=("retire-nanocode:$1")
  [[ "$RETIRE_FAILURE" != "true" ]]
}
die() { return 1; }

# WI#133/#134: stop_tenant now verifies writer quiescence after shutdown.
# The quiescence verdict is polled through await_writer_quiescence (WI#137);
# stub the poller here so these fail-loud contract tests stay instant — the
# real poller gets its own bounded-window tests at the bottom of this file.
WRITERS_ACTIVE_RC=1
await_writer_quiescence() {
  # Convert the old tenant_writers_active stub convention (1 = quiesced) to the
  # poller's convention (0 = quiesced, 1 = still active, 2 = unclassifiable).
  case "$WRITERS_ACTIVE_RC" in
    1) return 0 ;;
    0) return 1 ;;
    *) return 2 ;;
  esac
}
tenant_writers_active() { return "$WRITERS_ACTIVE_RC"; }

INIT_SYSTEM="systemd"
CALLS=()
stop_tenant alice
assert_before "stop-systemd:alice" "retire-nanocode:alice"
assert_before "stop-vision:alice" "retire-nanocode:alice"
assert_before "stop-pebble:alice" "retire-nanocode:alice"
assert_before "stop-opencode:alice" "retire-nanocode:alice"
assert_before "stop-kimi:alice" "retire-nanocode:alice"
assert_before "stop-postgres:alice" "retire-nanocode:alice"

# A writer still active after the shutdown sequence must fail the stop, and
# the quiescence check must run before Nanocode retirement. The stubbed die
# only returns, so these cases run in a subshell whose die actually exits —
# matching production — and retirement is tracked via a file marker.
retire_tenant_nanocode() {
  : >"$fixture/retire-ran"
  CALLS+=("retire-nanocode:$1")
  [[ "$RETIRE_FAILURE" != "true" ]]
}
rm -f "$fixture/retire-ran"

WRITERS_ACTIVE_RC=0
CALLS=()
if (die() { exit 1; }; stop_tenant alice) >/dev/null 2>&1; then
  fail "stop succeeded despite an active writer after shutdown"
else
  printf 'PASS: active writer after shutdown fails the stop\n'
fi
if [[ -e "$fixture/retire-ran" ]]; then
  fail "Nanocode retirement ran despite an active writer"
else
  printf 'PASS: retirement skipped while a writer is still active\n'
fi

# An unclassifiable writer state must also fail loudly, never pass silently.
rm -f "$fixture/retire-ran"
WRITERS_ACTIVE_RC=2
CALLS=()
if (die() { exit 1; }; stop_tenant alice) >/dev/null 2>&1; then
  fail "stop succeeded despite unverifiable writer state"
else
  printf 'PASS: unverifiable writer state fails the stop\n'
fi
if [[ -e "$fixture/retire-ran" ]]; then
  fail "Nanocode retirement ran despite unverifiable writer state"
else
  printf 'PASS: retirement skipped while writer state is unverifiable\n'
fi

retire_tenant_nanocode() {
  CALLS+=("retire-nanocode:$1")
  [[ "$RETIRE_FAILURE" != "true" ]]
}

# Override consumed by sourced stop_tenant dispatcher.
# shellcheck disable=SC2034
INIT_SYSTEM="openrc"
WRITERS_ACTIVE_RC=1
RETIRE_FAILURE="true"
CALLS=()
if stop_tenant alice >/dev/null 2>&1; then
  fail "stop succeeded despite Nanocode retirement failure"
else
  printf 'PASS: retirement failure reported after shutdown\n'
fi
assert_before "stop-openrc:alice" "retire-nanocode:alice"
assert_before "stop-postgres:alice" "retire-nanocode:alice"

# Podman cannot schedule image healthchecks without systemd. OpenRC-managed
# containers carrying a pre-fix healthcheck must be recreated once; other init
# systems and Docker keep their runtime-native healthchecks.
_ctr() {
  CALLS+=("ctr:$*")
  if [[ "$2" == "inspect" ]]; then
    printf '%s\n' "$HEALTHCHECK_JSON"
  fi
}
INIT_SYSTEM="openrc"
CONTAINER_RT="podman"
HEALTHCHECK_JSON='{"Test":["CMD-SHELL","curl -f /health"]}'
CALLS=()
_reconcile_openrc_podman_healthcheck alice lunarwing-opencode-alice
assert_call "ctr:alice stop lunarwing-opencode-alice"
assert_call "ctr:alice rm -f lunarwing-opencode-alice"

HEALTHCHECK_JSON='{"Test":["NONE"]}'
CALLS=()
_reconcile_openrc_podman_healthcheck alice lunarwing-opencode-alice
assert_no_call "ctr:alice stop lunarwing-opencode-alice"
assert_no_call "ctr:alice rm -f lunarwing-opencode-alice"

INIT_SYSTEM="systemd"
CALLS=()
_reconcile_openrc_podman_healthcheck alice lunarwing-opencode-alice
assert_no_call "ctr:alice inspect -f {{json .Config.Healthcheck}} lunarwing-opencode-alice"

# Globals consumed by the sourced reconciliation helper.
# shellcheck disable=SC2034
INIT_SYSTEM="openrc"
# shellcheck disable=SC2034
CONTAINER_RT="docker"
CALLS=()
_reconcile_openrc_podman_healthcheck alice lunarwing-opencode-alice
assert_no_call "ctr:alice inspect -f {{json .Config.Healthcheck}} lunarwing-opencode-alice"

# WI#133/#134 follow-up: tenant_writers_active's container branch must treat an
# absent container runtime as quiesced (no writers can exist or respawn), while a
# present-but-broken runtime still fails closed (a live runtime could respawn).
test_writers_active_no_runtime_is_quiesced() (
  set +e  # tenant_writers_active is EXPECTED to return non-zero here; set -e would abort the harness
  eval "$_TWA_REAL_DEF"  # restore the real impl (a global stub later in this file would otherwise shadow it)
  sanitize_name() { printf '%s' "$1"; }
  tenant_exists_in_registry() { return 0; }
  ensure_init_system() { INIT_SYSTEM="systemd"; }
  tenant_proxy_enabled() { return 1; }
  tenant_weechat_enabled() { return 1; }
  tenant_darkirc_enabled() { return 1; }
  tenant_vision_external() { :; }
  tenant_worker_enabled() { return 1; }
  _systemctl_user() { printf 'inactive\n'; }
  detect_container_runtime() { return 1; }  # no runtime
  _ctr() { return 1; }  # must never be reached when runtime is absent

  tenant_writers_active alice
  local rc=$?
  if [[ "$rc" == "0" ]]; then
    printf 'no-runtime: tenant_writers_active reported a writer (0)\n' >&2
    return 1
  fi
  if [[ "$rc" != "1" ]]; then
    printf 'no-runtime: expected quiesced (1), got %s\n' "$rc" >&2
    return 1
  fi
)

test_writers_active_broken_runtime_fails_closed() (
  set +e  # non-zero returns are expected classification results here, not failures
  eval "$_TWA_REAL_DEF"  # restore the real impl (a global stub later in this file would otherwise shadow it)
  sanitize_name() { printf '%s' "$1"; }
  tenant_exists_in_registry() { return 0; }
  # shellcheck disable=SC2034  # read by the eval'd tenant_writers_active body
  ensure_init_system() { INIT_SYSTEM="systemd"; }
  tenant_proxy_enabled() { return 1; }
  tenant_weechat_enabled() { return 1; }
  tenant_darkirc_enabled() { return 1; }
  tenant_vision_external() { :; }
  tenant_worker_enabled() { return 1; }
  _systemctl_user() { printf 'inactive\n'; }
  detect_container_runtime() { printf '%s' "podman"; }  # runtime present
  ensure_container_runtime() { return 1; }              # ...but introspection broken
  _ctr() { return 1; }

  tenant_writers_active alice
  local rc=$?
  if [[ "$rc" == "0" ]]; then
    printf 'broken-runtime: expected unclassifiable (2), got success (0)\n' >&2
    return 1
  fi
  if [[ "$rc" != "2" ]]; then
    printf 'broken-runtime: expected unclassifiable (2), got %s\n' "$rc" >&2
    return 1
  fi
)

test_writers_active_no_runtime_is_quiesced
test_writers_active_broken_runtime_fails_closed

# ── WI#137: OpenRC WeeChat status wording ─────────────────────────────────────
# The WeeChat unit reports "session is up" / "session is not running" instead of
# the standard started/stopped wording. "not running" must classify as STOPPED —
# the bare *running* substring made every stopped WeeChat tenant (starforce)
# report an active writer forever. A live "session is up" must classify ACTIVE.
test_writers_active_openrc_weechat_wording() (
  set +e  # tenant_writers_active returns non-zero for quiesced; that is the point
  eval "$_TWA_REAL_DEF"  # restore the real impl (a global stub later in this file would otherwise shadow it)
  sanitize_name() { printf '%s' "$1"; }
  tenant_exists_in_registry() { return 0; }
  # shellcheck disable=SC2034  # read by the eval'd tenant_writers_active body
  ensure_init_system() { INIT_SYSTEM="openrc"; }
  tenant_proxy_enabled() { return 1; }
  tenant_weechat_enabled() { return 0; }
  tenant_darkirc_enabled() { return 1; }
  tenant_vision_external() { :; }
  tenant_worker_enabled() { return 1; }
  OPENRC_INITD_DIR="$fixture/init.d"
  mkdir -p "$OPENRC_INITD_DIR"
  local svc
  while IFS= read -r svc; do
    printf '#!/bin/sh\n' >"$OPENRC_INITD_DIR/$svc"
    chmod +x "$OPENRC_INITD_DIR/$svc"
  done < <(tenant_writer_services alice)

  # Stopped WeeChat: "session is not running" -> quiesced (1), not active (0).
  WEECHAT_STATUS='* WeeChat (alice) session is not running'
  rc-service() { printf '%s\n' "$WEECHAT_STATUS"; return 1; }
  detect_container_runtime() { return 1; }
  tenant_writers_active alice
  local rc=$?
  if [[ "$rc" == "0" ]]; then
    printf 'weechat-stopped: "session is not running" reported an active writer (0)\n' >&2
    return 1
  fi
  if [[ "$rc" != "1" ]]; then
    printf 'weechat-stopped: expected quiesced (1), got %s\n' "$rc" >&2
    return 1
  fi

  # Running WeeChat: "session is up" -> active (0).
  WEECHAT_STATUS='* WeeChat (alice) session is up'
  rc-service() { printf '%s\n' "$WEECHAT_STATUS"; return 0; }
  tenant_writers_active alice
  rc=$?
  if [[ "$rc" != "0" ]]; then
    printf 'weechat-running: "session is up" was not classified active (got %s)\n' "$rc" >&2
    return 1
  fi
)

# ── WI#137: bounded quiesce-wait for async supervise-daemon teardown ──────────
# The poller must tolerate a writer that reports active for a few seconds after
# a healthy stop (async supervisor teardown), fail only after the deadline when
# it NEVER settles, and fail closed immediately on unclassifiable state.
test_quiesce_wait_active_then_settled() (
  set +e
  eval "$_AWAIT_QUIESCE_REAL_DEF"  # restore the real poller (the global stub shadows it)
  local polls=0
  sleep() { polls=$((polls + 1)); }
  tenant_writers_active() {
    polls=$((polls + 1))
    [[ "$polls" -le 2 ]] && return 0
    return 1
  }
  sanitize_name() { printf '%s' "$1"; }
  await_writer_quiescence alice
  local rc=$?
  if [[ "$rc" != "0" ]]; then
    printf 'active-then-settled: expected quiesced (0), got %s\n' "$rc" >&2
    return 1
  fi
  if [[ "$polls" -lt 3 ]]; then
    printf 'active-then-settled: expected >=3 polls, got %s\n' "$polls" >&2
    return 1
  fi
)

test_quiesce_wait_never_settles_fails_after_deadline() (
  set +e
  eval "$_AWAIT_QUIESCE_REAL_DEF"
  local polls=0 slept=0
  sleep() { slept=$((slept + 1)); }
  tenant_writers_active() { polls=$((polls + 1)); return 0; }
  sanitize_name() { printf '%s' "$1"; }
  LUNARWING_MT_QUIESCE_INTERVAL=1 LUNARWING_MT_QUIESCE_DEADLINE=5 \
    await_writer_quiescence alice
  local rc=$?
  if [[ "$rc" != "1" ]]; then
    printf 'never-settles: expected active-after-deadline (1), got %s\n' "$rc" >&2
    return 1
  fi
  # 6 polls (t=0..5) with a 1s interval and a 5s deadline.
  if [[ "$polls" != "6" || "$slept" != "5" ]]; then
    printf 'never-settles: expected 6 polls/5 sleeps, got polls=%s slept=%s\n' \
      "$polls" "$slept" >&2
    return 1
  fi
)

test_quiesce_wait_unclassifiable_fails_closed() (
  set +e
  eval "$_AWAIT_QUIESCE_REAL_DEF"
  local polls=0 slept=0
  sleep() { slept=$((slept + 1)); }
  tenant_writers_active() { polls=$((polls + 1)); return 2; }
  sanitize_name() { printf '%s' "$1"; }
  await_writer_quiescence alice
  local rc=$?
  if [[ "$rc" != "2" ]]; then
    printf 'unclassifiable: expected fail-closed (2), got %s\n' "$rc" >&2
    return 1
  fi
  if [[ "$polls" != "1" || "$slept" != "0" ]]; then
    printf 'unclassifiable: expected single poll and no sleep, got polls=%s slept=%s\n' \
      "$polls" "$slept" >&2
    return 1
  fi
)

test_quiesce_wait_active_then_settled
test_quiesce_wait_never_settles_fails_after_deadline
test_quiesce_wait_unclassifiable_fails_closed
test_writers_active_openrc_weechat_wording

# ── WI#231 F2: stop-tenant disables units (boot + health pipeline) ──────────
# stop-tenant must leave the tenant durably stopped: on systemd the enable-list
# units are disabled (F3-B then grades them `skipped`), on OpenRC the runlevel
# entries are removed (tenant_started() then reads not-started → `skipped`).
# start-tenant re-enables on the way up (existing start paths already do).

test_stop_tenant_disables_systemd_units() (
  local disable_calls=0 primary_disabled=0 call
  eval "$_STOP_SYSTEMD_REAL_DEF"
  id() {
    if [[ "${1:-}" == "-u" ]]; then printf '1000\n'; return 0; fi
    return 1
  }
  _systemctl_user() {
    CALLS+=("systemctl:$*")
    return 0   # is-active true (stop runs) + is-enabled true (disable runs)
  }
  CALLS=()
  stop_tenant_systemd alice
  for call in "${CALLS[@]}"; do
    [[ "$call" == "systemctl:alice disable "* ]] && disable_calls=$((disable_calls + 1))
    [[ "$call" == "systemctl:alice disable lunarwing-alice.service" ]] && primary_disabled=1
  done
  # The whole enable-list (primary, bridge, weechat, proxy, darkirc) is
  # disabled — 7 units on a fully-featured tenant; floor of 2 keeps the
  # assertion robust to list trimming, plus the primary must be among them.
  if (( disable_calls < 2 )) || (( primary_disabled == 0 )); then
    printf 'stop-tenant systemd: expected enable-list units disabled, saw %d (primary=%d)\n' \
      "$disable_calls" "$primary_disabled" >&2
    return 1
  fi
)

test_stop_tenant_systemd_skips_disable_when_not_enabled() (
  local disable_calls=0 call
  eval "$_STOP_SYSTEMD_REAL_DEF"
  id() {
    if [[ "${1:-}" == "-u" ]]; then printf '1000\n'; return 0; fi
    return 1
  }
  _systemctl_user() {
    CALLS+=("systemctl:$*")
    [[ "${2:-}" == "is-enabled" ]] && return 1   # nothing enabled
    return 0
  }
  CALLS=()
  stop_tenant_systemd alice
  for call in "${CALLS[@]}"; do
    [[ "$call" == *" disable "* ]] && disable_calls=$((disable_calls + 1))
  done
  if (( disable_calls != 0 )); then
    printf 'stop-tenant systemd: disable ran on not-enabled units (%d)\n' \
      "$disable_calls" >&2
    return 1
  fi
)

test_stop_tenant_openrc_strips_runlevels() (
  local removed=0 link
  eval "$_STOP_OPENRC_REAL_DEF"
  tenant_darkirc_enabled() { return 1; }
  tenant_weechat_enabled() { return 0; }
  tenant_proxy_enabled() { return 0; }
  rc-service() { return 0; }
  rc-update() {
    CALLS+=("rc-update:$*")
    # Mirror the real rc-update del: remove the symlink if present.
    if [[ "${1:-}" == "del" ]]; then
      for link in "$OPENRC_RUNLEVELS_DIR"/*/"$2"; do
        [[ -e "$link" || -L "$link" ]] && command rm -f -- "$link"
      done
    fi
    return 0
  }
  # Seed the default runlevel with the tenant's persistence.
  ln -s "$OPENRC_INITD_DIR/lunarwing-alice" "$OPENRC_RUNLEVELS_DIR/default/lunarwing-alice" 2>/dev/null || true
  touch "$OPENRC_INITD_DIR/lunarwing-alice" 2>/dev/null || true
  ln -s "$OPENRC_INITD_DIR/xmpp-bridge-alice" "$OPENRC_RUNLEVELS_DIR/default/xmpp-bridge-alice" 2>/dev/null || true
  touch "$OPENRC_INITD_DIR/xmpp-bridge-alice" 2>/dev/null || true

  CALLS=()
  stop_tenant_openrc alice
  for link in "$OPENRC_RUNLEVELS_DIR"/default/*; do
    [[ -e "$link" || -L "$link" ]] && removed=$((removed + 1))
  done
  if (( removed != 0 )); then
    printf 'stop-tenant openrc: %d runlevel entry(ies) survived the stop\n' \
      "$removed" >&2
    return 1
  fi
  # The primary + bridge must both have gone through rc-update del.
  local primary_del=0 bridge_del=0 call
  for call in "${CALLS[@]}"; do
    [[ "$call" == "rc-update:del lunarwing-alice "*(default) ]] && primary_del=1
    [[ "$call" == "rc-update:del xmpp-bridge-alice "*(default) ]] && bridge_del=1
  done
  if (( primary_del == 0 || bridge_del == 0 )); then
    printf 'stop-tenant openrc: rc-update del missing (primary=%d bridge=%d)\n' \
      "$primary_del" "$bridge_del" >&2
    return 1
  fi
)

test_stop_tenant_openrc_quiet_on_never_started() (
  local del_calls=0 call
  eval "$_STOP_OPENRC_REAL_DEF"
  tenant_darkirc_enabled() { return 1; }
  tenant_weechat_enabled() { return 0; }
  tenant_proxy_enabled() { return 0; }
  rc-service() { return 0; }
  rc-update() { CALLS+=("rc-update:$*"); return 0; }
  # No runlevel entries at all (never-started tenant): no rc-update del churn.
  CALLS=()
  stop_tenant_openrc alice
  for call in "${CALLS[@]}"; do
    [[ "$call" == "rc-update:del "* ]] && del_calls=$((del_calls + 1))
  done
  if (( del_calls != 0 )); then
    printf 'stop-tenant openrc: rc-update del ran on a never-started tenant (%d)\n' \
      "$del_calls" >&2
    return 1
  fi
)

if test_stop_tenant_disables_systemd_units; then
  printf 'PASS: WI#231 F2 stop-tenant disables systemd enable-list units\n'
else
  fail "WI#231 F2 stop-tenant disables systemd enable-list units"
fi

if test_stop_tenant_systemd_skips_disable_when_not_enabled; then
  printf 'PASS: WI#231 F2 stop-tenant systemd skips not-enabled units\n'
else
  fail "WI#231 F2 stop-tenant systemd skips not-enabled units"
fi

if test_stop_tenant_openrc_strips_runlevels; then
  printf 'PASS: WI#231 F2 stop-tenant openrc strips runlevel persistence\n'
else
  fail "WI#231 F2 stop-tenant openrc strips runlevel persistence"
fi

if test_stop_tenant_openrc_quiet_on_never_started; then
  printf 'PASS: WI#231 F2 stop-tenant openrc quiet on never-started tenants\n'
else
  fail "WI#231 F2 stop-tenant openrc quiet on never-started tenants"
fi

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all worker lifecycle tests passed\n'
