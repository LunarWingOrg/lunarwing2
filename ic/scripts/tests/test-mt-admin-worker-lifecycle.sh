#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"

failures=0
CALLS=()

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
ensure_container_runtime() { CONTAINER_RT="podman"; MT_ROOTLESS="${MT_ROOTLESS:-true}"; }
ensure_init_system() { :; }
podman_supports_quadlet() { return 0; }
say() { :; }
_systemctl_user() { CALLS+=("systemctl:$*"); }
rc-service() { CALLS+=("rc:$*"); }
_ctr() {
  CALLS+=("ctr:$*")
  return 0
}
start_tenant_opencode() { CALLS+=("start-opencode:$1"); }

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

INIT_SYSTEM="openrc"
CALLS=()
stop_worker alice opencode
assert_call "rc:lunarwing-opencode-alice-sup stop"
assert_call "ctr:alice stop lunarwing-opencode-alice"
assert_before "rc:lunarwing-opencode-alice-sup stop" "ctr:alice stop lunarwing-opencode-alice"

CALLS=()
kill_worker alice opencode
assert_call "rc:lunarwing-opencode-alice-sup stop"
assert_call "ctr:alice stop --time 0 lunarwing-opencode-alice"
assert_before \
  "rc:lunarwing-opencode-alice-sup stop" \
  "ctr:alice stop --time 0 lunarwing-opencode-alice"

CALLS=()
start_worker alice opencode
assert_call "start-opencode:alice"

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

fixture="$(mktemp -d)"
trap 'command rm -rf "$fixture"' EXIT
tenant_home() { printf '%s/home/%s' "$fixture" "$1"; }
tenant_lw_root() { printf '%s/home/%s/lunarwing' "$fixture" "$1"; }
tenant_env_dir() { printf '%s/env' "$(tenant_lw_root "$1")"; }
tenant_state_dir() { printf '%s/state' "$(tenant_lw_root "$1")"; }
tenant_quadlet_dir() { printf '%s/quadlets' "$fixture"; }
chown() { :; }
rc-update() { CALLS+=("rc-update:$*"); }
rm() {
  CALLS+=("rm:$*")
  local arg safe_args=()
  for arg in "$@"; do
    [[ "$arg" == /etc/init.d/* || "$arg" == /etc/conf.d/* ]] || safe_args+=("$arg")
  done
  [[ "${#safe_args[@]}" -eq 0 ]] || command rm "${safe_args[@]}"
}

write_legacy_nanocode_config() {
  mkdir -p "$(tenant_env_dir alice)" "$(tenant_state_dir alice)" "$(tenant_quadlet_dir alice)"
  cat >"$(tenant_env_dir alice)/lunarwing.env" <<'EOF'
KEEP_ME=yes
NANOCODE_WSS_PORT=10007
NANOCODE_MODEL=retired-model
NANOCODE_BASE_URL=https://retired.example.test/openai/v1
NANOCODE_MODE=websocket
EOF
  cat >"$(tenant_state_dir alice)/config.toml" <<'EOF'
keep_setting = true

# External worker: nanocode — create_job(mode: "nanocode")
[[sandbox.external_workers]]
name = "nanocode"
url = "ws://127.0.0.1:10007/ws/agent"

# External worker: opencode — create_job(mode: "opencode")
[[sandbox.external_workers]]
name = "opencode"
url = "ws://127.0.0.1:10008/ws/agent"
EOF
  touch "$(tenant_quadlet_dir alice)/lunarwing-nanocode-alice.container"
}

write_legacy_nanocode_config
INIT_SYSTEM="systemd"
CALLS=()
retire_tenant_nanocode alice
assert_call "systemctl:alice stop lunarwing-nanocode-alice.service"
assert_call "systemctl:alice daemon-reload"
assert_call "ctr:alice stop lunarwing-nanocode-alice"
if grep -q '^NANOCODE_' "$(tenant_env_dir alice)/lunarwing.env"; then
  fail "retirement kept NANOCODE_* env"
else
  printf 'PASS: retirement removes NANOCODE_* env\n'
fi
if grep -q 'name = "nanocode"' "$(tenant_state_dir alice)/config.toml"; then
  fail "retirement kept Nanocode external-worker route"
else
  printf 'PASS: retirement removes Nanocode external-worker route\n'
fi
grep -q 'name = "opencode"' "$(tenant_state_dir alice)/config.toml" \
  && printf 'PASS: retirement preserves adjacent worker route\n' \
  || fail "retirement removed adjacent worker route"
[[ ! -e "$(tenant_quadlet_dir alice)/lunarwing-nanocode-alice.container" ]] \
  && printf 'PASS: retirement removes Nanocode Quadlet\n' \
  || fail "retirement kept Nanocode Quadlet"

write_legacy_nanocode_config
INIT_SYSTEM="openrc"
CALLS=()
retire_tenant_nanocode alice
assert_call "rc:lunarwing-nanocode-alice-sup stop"
assert_call "rc:lunarwing-nanocode-alice stop"
assert_call "rc-update:del lunarwing-nanocode-alice-sup default"
assert_call "rc-update:del lunarwing-nanocode-alice default"
assert_call "ctr:alice stop lunarwing-nanocode-alice"

INIT_SYSTEM="systemd"
CALLS=()
id() { [[ "$1" == "-u" ]] && { printf '1000\n'; return 0; }; return 1; }
uninstall_tenant_systemd alice
assert_no_call "ctr:alice rm -f lunarwing-nanocode-alice"
assert_call "ctr:alice rm -f lunarwing-pebble-alice"
assert_call "ctr:alice rm -f lunarwing-opencode-alice"
assert_call "ctr:alice rm -f lunarwing-kimi-alice"

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all worker lifecycle tests passed\n'
