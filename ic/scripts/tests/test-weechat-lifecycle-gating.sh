#!/usr/bin/env bash
# Regression coverage for ISSUE-006: enable_weechat controls rendered units,
# daemon dependencies, boot persistence, and strict lifecycle operations on
# both systemd user managers and OpenRC.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$SCRIPT_DIR/../lunarwing-mt-admin.sh"

# shellcheck source=../lunarwing-mt-admin.sh
source "$ADMIN_SCRIPT"

failures=0
pass() { printf '  PASS: %s\n' "$1"; }
fail() { printf '  FAIL: %s\n' "$1"; failures=$((failures + 1)); }

assert_file() {
  [[ -f "$2" ]] && pass "$1" || fail "$1"
}

assert_no_file() {
  [[ ! -e "$2" && ! -L "$2" ]] && pass "$1" || fail "$1"
}

assert_contains() {
  grep -qF -- "$3" "$2" && pass "$1" || fail "$1"
}

assert_not_contains() {
  if grep -qF -- "$3" "$2"; then fail "$1"; else pass "$1"; fi
}

assert_eq() {
  [[ "$2" == "$3" ]] && pass "$1" || fail "$1 (expected [$3], got [$2])"
}

assert_ok() {
  local label="$1"
  shift
  if "$@" >/dev/null 2>&1; then pass "$label"; else fail "$label"; fi
}

assert_fails() {
  local label="$1"
  shift
  if "$@" >/dev/null 2>&1; then fail "$label"; else pass "$label"; fi
}

FIXTURE="$(mktemp -d)"
trap 'rm -rf "$FIXTURE"' EXIT
TENANT="lifecycle-test"
# Overrides consumed by sourced lifecycle/rendering helpers.
# shellcheck disable=SC2034
TENANT_LIFECYCLE_LOCK_DIR="$FIXTURE/locks"
PORTS_REGISTRY="$FIXTURE/ports.json"
OPENRC_INITD_DIR="$FIXTURE/openrc/init.d"
OPENRC_CONFD_DIR="$FIXTURE/openrc/conf.d"
OPENRC_RUNLEVELS_DIR="$FIXTURE/openrc/runlevels"
# shellcheck disable=SC2034
OPENRC_ENV_EXEC="$FIXTURE/openrc/lunarwing-openrc-env-exec"

mkdir -p "$OPENRC_INITD_DIR" "$OPENRC_CONFD_DIR" "$OPENRC_RUNLEVELS_DIR/default"

write_weechat_flag() {
  local enabled="$1"
  cat >"$PORTS_REGISTRY" <<JSON
{
  "tenants": {
    "$TENANT": {
      "enable_weechat": $enabled,
      "ports": {}
    }
  }
}
JSON
}

tenant_home() { printf '%s/tenant' "$FIXTURE"; }
tenant_repo() { printf '%s/tenant/lunarwing/ic' "$FIXTURE"; }
tenant_env_dir() { printf '%s/tenant/lunarwing/env' "$FIXTURE"; }
tenant_state_dir() { printf '%s/tenant/lunarwing/state' "$FIXTURE"; }
tenant_run_dir() { printf '%s/tenant/lunarwing/run' "$FIXTURE"; }
tenant_log_dir() { printf '%s/tenant/lunarwing/logs' "$FIXTURE"; }
tenant_lw_root() { printf '%s/tenant/lunarwing' "$FIXTURE"; }
tenant_weechat_home() { printf '%s/tenant/.config/weechat' "$FIXTURE"; }
ports_get() { printf '18000'; }
tenant_proxy_enabled() { return 1; }
tenant_darkirc_enabled() { return 1; }
podman_supports_quadlet() { return 1; }
install_openrc_env_exec() { :; }
render_container_babysitter_unit() { :; }
ensure_container_runtime() {
  # Globals consumed by sourced renderers.
  # shellcheck disable=SC2034
  CONTAINER_RT="/bin/true"
  # shellcheck disable=SC2034
  MT_ROOTLESS=false
}
chown() { :; }

# Existing helper-specific tests cover command content. These replacements keep
# this harness focused on conditional ownership by the full tenant renderers.
_render_weechat_systemd_unit() {
  printf '[Unit]\nDescription=test WeeChat\n' >"$2/lunarwing-weechat-${1}.service"
}

_render_weechat_openrc_unit() {
  printf '#!/sbin/openrc-run\n' >"$2"
  chmod 0755 "$2"
}

echo "=== systemd rendering and dependency gating ==="
write_weechat_flag true
# Function comes from sourced mt-admin later in ShellCheck's composed parse.
# shellcheck disable=SC2218
render_tenant_systemd_units "$TENANT" >/dev/null

SYSTEMD_DIR="$(tenant_home "$TENANT")/.config/systemd/user"
SYSTEMD_MAIN="$SYSTEMD_DIR/lunarwing-${TENANT}.service"
SYSTEMD_BACKEND="$SYSTEMD_DIR/lunarwing-weechat-${TENANT}.service"
SYSTEMD_ADAPTER="$SYSTEMD_DIR/lunarwing-weechat-adapter-${TENANT}.service"

assert_file "enabled systemd backend rendered" "$SYSTEMD_BACKEND"
assert_file "enabled systemd adapter rendered" "$SYSTEMD_ADAPTER"
assert_contains "enabled systemd main wants backend" "$SYSTEMD_MAIN" "lunarwing-weechat-${TENANT}.service"
assert_contains "enabled systemd main wants adapter" "$SYSTEMD_MAIN" "lunarwing-weechat-adapter-${TENANT}.service"

mkdir -p "$SYSTEMD_DIR/default.target.wants"
ln -s "$SYSTEMD_BACKEND" "$SYSTEMD_DIR/default.target.wants/lunarwing-weechat-${TENANT}.service"
ln -s "$SYSTEMD_ADAPTER" "$SYSTEMD_DIR/default.target.wants/lunarwing-weechat-adapter-${TENANT}.service"

write_weechat_flag false
# Function comes from sourced mt-admin later in ShellCheck's composed parse.
# shellcheck disable=SC2218
render_tenant_systemd_units "$TENANT" >/dev/null

assert_no_file "disabled systemd backend removed" "$SYSTEMD_BACKEND"
assert_no_file "disabled systemd adapter removed" "$SYSTEMD_ADAPTER"
assert_no_file "disabled systemd backend enable link removed" "$SYSTEMD_DIR/default.target.wants/lunarwing-weechat-${TENANT}.service"
assert_no_file "disabled systemd adapter enable link removed" "$SYSTEMD_DIR/default.target.wants/lunarwing-weechat-adapter-${TENANT}.service"
assert_not_contains "disabled systemd main drops backend dependency" "$SYSTEMD_MAIN" "lunarwing-weechat-${TENANT}.service"
assert_not_contains "disabled systemd main drops adapter dependency" "$SYSTEMD_MAIN" "lunarwing-weechat-adapter-${TENANT}.service"

echo "=== OpenRC rendering and dependency gating ==="
write_weechat_flag true
render_tenant_openrc_units "$TENANT" >/dev/null

OPENRC_MAIN="$OPENRC_INITD_DIR/lunarwing-${TENANT}"
OPENRC_MAIN_CONF="$OPENRC_CONFD_DIR/lunarwing-${TENANT}"
OPENRC_BACKEND="$OPENRC_INITD_DIR/lunarwing-weechat-${TENANT}"
OPENRC_ADAPTER="$OPENRC_INITD_DIR/lunarwing-weechat-adapter-${TENANT}"

assert_file "enabled OpenRC backend rendered" "$OPENRC_BACKEND"
assert_file "enabled OpenRC adapter rendered" "$OPENRC_ADAPTER"
assert_contains "enabled OpenRC main orders after backend" "$OPENRC_MAIN" "lunarwing-weechat-${TENANT}"
assert_contains "enabled OpenRC main needs backend" "$OPENRC_MAIN_CONF" "lunarwing-weechat-${TENANT}"
assert_contains "OpenRC main consumes configured optional dependencies" "$OPENRC_MAIN" 'want ${lunarwing_rc_need}'

ln -s "$OPENRC_BACKEND" "$OPENRC_RUNLEVELS_DIR/default/lunarwing-weechat-${TENANT}"
ln -s "$OPENRC_ADAPTER" "$OPENRC_RUNLEVELS_DIR/default/lunarwing-weechat-adapter-${TENANT}"

write_weechat_flag false
render_tenant_openrc_units "$TENANT" >/dev/null

assert_no_file "disabled OpenRC backend removed" "$OPENRC_BACKEND"
assert_no_file "disabled OpenRC adapter removed" "$OPENRC_ADAPTER"
assert_no_file "disabled OpenRC backend runlevel link removed" "$OPENRC_RUNLEVELS_DIR/default/lunarwing-weechat-${TENANT}"
assert_no_file "disabled OpenRC adapter runlevel link removed" "$OPENRC_RUNLEVELS_DIR/default/lunarwing-weechat-adapter-${TENANT}"
assert_not_contains "disabled OpenRC main drops backend ordering" "$OPENRC_MAIN" "lunarwing-weechat-${TENANT}"
assert_not_contains "disabled OpenRC main drops backend need" "$OPENRC_MAIN_CONF" "lunarwing-weechat-${TENANT}"

echo "=== strict systemd lifecycle ==="
write_weechat_flag true
# Function comes from sourced mt-admin later in ShellCheck's composed parse.
# shellcheck disable=SC2218
render_tenant_systemd_units "$TENANT" >/dev/null

id() {
  if [[ "${1:-}" == "-u" ]]; then printf '1000\n'; return 0; fi
  command id "$@"
}

SYSTEMCTL_LOG="$FIXTURE/systemctl.log"
SYSTEMCTL_FAIL=""
SYSTEMCTL_LOAD_STATE="loaded"
_systemctl_user() {
  printf '%s\n' "$*" >>"$SYSTEMCTL_LOG"
  if [[ "${2:-}" == "show" ]]; then printf '%s\n' "$SYSTEMCTL_LOAD_STATE"; fi
  if [[ -n "$SYSTEMCTL_FAIL" && " $* " == *" $SYSTEMCTL_FAIL "* ]]; then return 1; fi
  return 0
}

# Override consumed by sourced reconciliation helper.
# shellcheck disable=SC2034
INIT_SYSTEM=systemd
: >"$SYSTEMCTL_LOG"
assert_ok "systemd disable succeeds when stop/disable succeed" weechat_lifecycle_stop_disable "$TENANT"
assert_contains "systemd disable calls stop" "$SYSTEMCTL_LOG" " stop "
assert_contains "systemd disable removes enablement" "$SYSTEMCTL_LOG" " disable "

SYSTEMCTL_FAIL=stop
assert_fails "systemd disable propagates stop failure" weechat_lifecycle_stop_disable "$TENANT"
SYSTEMCTL_FAIL=""

SYSTEMCTL_FAIL=show-environment
assert_fails "systemd disable fails when user manager is unavailable" weechat_lifecycle_stop_disable "$TENANT"
SYSTEMCTL_FAIL=""

SYSTEMCTL_FAIL=show
assert_fails "systemd disable fails when unit query fails" weechat_lifecycle_stop_disable "$TENANT"
SYSTEMCTL_FAIL=""

: >"$SYSTEMCTL_LOG"
assert_ok "systemd enable succeeds when all operations succeed" weechat_lifecycle_enable_start "$TENANT"
assert_contains "systemd enable calls enable" "$SYSTEMCTL_LOG" " enable "
assert_contains "systemd enable starts backend" "$SYSTEMCTL_LOG" " start lunarwing-weechat-${TENANT}.service"
assert_contains "systemd enable starts adapter" "$SYSTEMCTL_LOG" " start lunarwing-weechat-adapter-${TENANT}.service"

SYSTEMCTL_FAIL=enable
assert_fails "systemd enable propagates enable failure" weechat_lifecycle_enable_start "$TENANT"
SYSTEMCTL_FAIL=""

rm -f "$SYSTEMD_BACKEND" "$SYSTEMD_ADAPTER"
SYSTEMCTL_LOAD_STATE=not-found
assert_ok "systemd disable is idempotent when units are absent" weechat_lifecycle_stop_disable "$TENANT"
assert_fails "systemd enable fails when rendered units are absent" weechat_lifecycle_enable_start "$TENANT"
SYSTEMCTL_LOAD_STATE=loaded

echo "=== strict OpenRC lifecycle ==="
write_weechat_flag true
render_tenant_openrc_units "$TENANT" >/dev/null
ln -sf "$OPENRC_BACKEND" "$OPENRC_RUNLEVELS_DIR/default/lunarwing-weechat-${TENANT}"
ln -sf "$OPENRC_ADAPTER" "$OPENRC_RUNLEVELS_DIR/default/lunarwing-weechat-adapter-${TENANT}"
mkdir -p "$OPENRC_RUNLEVELS_DIR/rescue"
ln -sf "$OPENRC_BACKEND" "$OPENRC_RUNLEVELS_DIR/rescue/lunarwing-weechat-${TENANT}"

OPENRC_LOG="$FIXTURE/openrc.log"
OPENRC_FAIL=""
rc-service() {
  printf 'service %s\n' "$*" >>"$OPENRC_LOG"
  if [[ -n "$OPENRC_FAIL" && " $* " == *" $OPENRC_FAIL "* ]]; then return 1; fi
  return 0
}
rc-update() {
  printf 'update %s\n' "$*" >>"$OPENRC_LOG"
  if [[ -n "$OPENRC_FAIL" && " $* " == *" $OPENRC_FAIL "* ]]; then return 1; fi
  return 0
}

INIT_SYSTEM=openrc
: >"$OPENRC_LOG"
assert_ok "OpenRC disable succeeds when stop/runlevel removal succeed" weechat_lifecycle_stop_disable "$TENANT"
assert_contains "OpenRC disable stops adapter" "$OPENRC_LOG" "service lunarwing-weechat-adapter-${TENANT} stop"
assert_contains "OpenRC disable removes runlevel" "$OPENRC_LOG" "update del lunarwing-weechat-${TENANT} default"
assert_contains "OpenRC disable removes non-default runlevel" "$OPENRC_LOG" "update del lunarwing-weechat-${TENANT} rescue"

OPENRC_FAIL=stop
assert_fails "OpenRC disable propagates stop failure" weechat_lifecycle_stop_disable "$TENANT"
OPENRC_FAIL=""

: >"$OPENRC_LOG"
assert_ok "OpenRC enable succeeds when all operations succeed" weechat_lifecycle_enable_start "$TENANT"
assert_contains "OpenRC enable adds backend runlevel" "$OPENRC_LOG" "update add lunarwing-weechat-${TENANT} default"
assert_contains "OpenRC enable starts adapter" "$OPENRC_LOG" "service lunarwing-weechat-adapter-${TENANT} start"

OPENRC_FAIL=start
assert_fails "OpenRC enable propagates start failure" weechat_lifecycle_enable_start "$TENANT"
OPENRC_FAIL=""

rm -f "$OPENRC_BACKEND" "$OPENRC_ADAPTER"
assert_ok "OpenRC disable is idempotent when scripts are absent" weechat_lifecycle_stop_disable "$TENANT"
assert_fails "OpenRC enable fails when rendered scripts are absent" weechat_lifecycle_enable_start "$TENANT"

echo "=== renderer exit containment ==="
# Override consumed by sourced reconciliation helper.
# shellcheck disable=SC2034
INIT_SYSTEM=systemd
render_tenant_systemd_units() { exit 1; }
assert_fails "reconcile converts renderer exit into catchable failure" weechat_reconcile_units "$TENANT"

echo "=== transactional command ordering ==="
ACTION_LOG="$FIXTURE/actions.log"
ACTION_FAIL=""
REGISTRY_STATE=true
ensure_init_system() { :; }
weechat_lifecycle_stop_disable() {
  printf 'stop-disable\n' >>"$ACTION_LOG"
  [[ "$ACTION_FAIL" != "stop-disable" ]]
}
weechat_lifecycle_enable_start() {
  printf 'enable-start\n' >>"$ACTION_LOG"
  [[ "$ACTION_FAIL" != "enable-start" ]]
}
ports_set_weechat() {
  printf 'flag-%s\n' "$2" >>"$ACTION_LOG"
  REGISTRY_STATE="$2"
  [[ "$ACTION_FAIL" != "flag-$2" ]]
}
weechat_reconcile_units() {
  printf 'reconcile-%s\n' "$REGISTRY_STATE" >>"$ACTION_LOG"
  [[ "$ACTION_FAIL" != "reconcile-$REGISTRY_STATE" ]]
}

: >"$ACTION_LOG"
ACTION_FAIL=stop-disable
REGISTRY_STATE=true
assert_fails "disable transaction stops before changing registry" set_tenant_weechat_enabled "$TENANT" false
assert_eq "failed stop leaves registry enabled" "$REGISTRY_STATE" "true"
assert_eq "failed stop performs no later action" "$(<"$ACTION_LOG")" "stop-disable"

: >"$ACTION_LOG"
ACTION_FAIL=reconcile-false
REGISTRY_STATE=true
assert_fails "disable transaction reports reconciliation failure" set_tenant_weechat_enabled "$TENANT" false
assert_eq "failed disabled reconciliation restores enabled flag" "$REGISTRY_STATE" "true"
assert_contains "failed disabled reconciliation attempts enabled restore" "$ACTION_LOG" "flag-true"

: >"$ACTION_LOG"
ACTION_FAIL=""
REGISTRY_STATE=true
assert_ok "disable transaction succeeds in strict order" set_tenant_weechat_enabled "$TENANT" false
assert_eq "successful disable leaves registry disabled" "$REGISTRY_STATE" "false"
assert_eq "successful disable action order" "$(<"$ACTION_LOG")" $'stop-disable\nflag-false\nreconcile-false'

: >"$ACTION_LOG"
REGISTRY_STATE=false
assert_ok "enable transaction renders before start" set_tenant_weechat_enabled "$TENANT" true
assert_eq "successful enable leaves registry enabled" "$REGISTRY_STATE" "true"
assert_eq "successful enable action order" "$(<"$ACTION_LOG")" $'flag-true\nreconcile-true\nenable-start'

echo "=== per-tenant lifecycle serialization ==="
LOCK_LOG="$FIXTURE/lifecycle-lock.log"
: >"$LOCK_LOG"
lock_probe() {
  local label="$1"
  printf '%s-start\n' "$label" >>"$LOCK_LOG"
  sleep 0.1
  printf '%s-end\n' "$label" >>"$LOCK_LOG"
}
with_tenant_lifecycle_lock "$TENANT" lock_probe first &
lock_pid_one=$!
with_tenant_lifecycle_lock "$TENANT" lock_probe second &
lock_pid_two=$!
wait "$lock_pid_one"
wait "$lock_pid_two"
mapfile -t lock_lines <"$LOCK_LOG"
first_label="${lock_lines[0]%-start}"
second_label="${lock_lines[2]%-start}"
if [[ "${lock_lines[1]:-}" == "$first_label-end" \
  && "${lock_lines[3]:-}" == "$second_label-end" \
  && "$first_label" != "$second_label" ]]; then
  pass "same-tenant lifecycle operations do not overlap"
else
  fail "same-tenant lifecycle operations do not overlap"
fi

echo ""
if ((failures == 0)); then
  echo "ALL TESTS PASSED"
  exit 0
fi

echo "$failures TEST(S) FAILED"
exit 1
