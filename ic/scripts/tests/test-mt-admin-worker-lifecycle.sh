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
LEGACY_STOP_FAILURE=""
_legacy_nanocode_ctr() {
  local store="$1" name="$2" command="$3"
  shift 3
  CALLS+=("legacy-ctr:$store:$name $command $*")
  if [[ "$command" == "stop" && "$store" == "$LEGACY_STOP_FAILURE" ]]; then
    return 1
  fi
  if [[ "$command" == "inspect" && "${1:-}" == "-f" ]]; then
    printf 'false\n'
  fi
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

CALLS=()
start_worker alice kimi
assert_call "start-kimi:alice"

CALLS=()
stop_worker alice kimi
assert_call "rc:lunarwing-kimi-alice-sup stop"
assert_call "ctr:alice stop lunarwing-kimi-alice"

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
  fail "retired nanocode worker accepted by stop-worker"
else
  printf 'PASS: retired nanocode worker rejected by stop-worker\n'
fi

# ── Legacy Nanocode retirement ────────────────────────────────────────────────

fixture="$(mktemp -d)"
trap 'command rm -rf "$fixture"' EXIT
tenant_home() { printf '%s/home/%s' "$fixture" "$1"; }
tenant_lw_root() { printf '%s/home/%s/lunarwing' "$fixture" "$1"; }
tenant_env_dir() { printf '%s/env' "$(tenant_lw_root "$1")"; }
tenant_state_dir() { printf '%s/state' "$(tenant_lw_root "$1")"; }
tenant_quadlet_dir() { printf '%s/quadlets' "$fixture"; }
chown() { :; }
id() { [[ "${1:-}" == "-u" ]] && { printf '1000\n'; return 0; }; return 1; }
rc-update() { CALLS+=("rc-update:$*"); }
_deregister_babysitter() { CALLS+=("deregister-babysitter:$*"); }
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
  printf 'NANOGPT_API_KEY=retired-secret\n' >"$(tenant_env_dir alice)/nanocode.env"
  cat >"$(tenant_state_dir alice)/config.toml" <<'EOF'
keep_setting = true

# External worker: nanocode — create_job(mode: "nanocode")
[[sandbox.external_workers]]
name = "nanocode"
url = "ws://127.0.0.1:10007/ws/agent"

[[sandbox.external_workers.endpoints]]
url = "ws://127.0.0.2:10007/ws/agent"

# External worker: opencode — create_job(mode: "opencode")
[[sandbox.external_workers]]
name = "opencode"
url = "ws://127.0.0.1:10008/ws/agent"

[[sandbox.external_workers.endpoints]]
url = "ws://127.0.0.2:10008/ws/agent"
EOF
  touch "$(tenant_quadlet_dir alice)/lunarwing-nanocode-alice.container"
}

write_legacy_nanocode_config
INIT_SYSTEM="systemd"
CALLS=()
retire_tenant_nanocode alice
assert_call "systemctl:alice stop lunarwing-nanocode-alice.service"
assert_call "systemctl:alice daemon-reload"
assert_call "legacy-ctr:rootful:alice stop lunarwing-nanocode-alice"
assert_call "legacy-ctr:rootless:alice stop lunarwing-nanocode-alice"
if grep -q '^NANOCODE_' "$(tenant_env_dir alice)/lunarwing.env"; then
  fail "retirement kept NANOCODE_* env"
else
  printf 'PASS: retirement removes NANOCODE_* env\n'
fi
if grep -q '^KEEP_ME=yes' "$(tenant_env_dir alice)/lunarwing.env"; then
  printf 'PASS: retirement preserves unrelated env\n'
else
  fail "retirement dropped unrelated env"
fi
if [[ -e "$(tenant_env_dir alice)/nanocode.env" ]]; then
  fail "retirement kept legacy nanocode.env"
else
  printf 'PASS: retirement removes legacy nanocode.env\n'
fi
if grep -q 'name = "nanocode"' "$(tenant_state_dir alice)/config.toml"; then
  fail "retirement kept Nanocode external-worker route"
else
  printf 'PASS: retirement removes Nanocode external-worker route\n'
fi
if grep -q '127.0.0.2:10007' "$(tenant_state_dir alice)/config.toml"; then
  fail "retirement kept Nanocode endpoint route"
else
  printf 'PASS: retirement removes nested Nanocode endpoint routes\n'
fi
grep -q 'name = "opencode"' "$(tenant_state_dir alice)/config.toml" \
  && printf 'PASS: retirement preserves adjacent worker route\n' \
  || fail "retirement removed adjacent worker route"
grep -q '127.0.0.2:10008' "$(tenant_state_dir alice)/config.toml" \
  && printf 'PASS: retirement preserves adjacent worker endpoint route\n' \
  || fail "retirement removed adjacent worker endpoint route"
grep -q '^keep_setting = true' "$(tenant_state_dir alice)/config.toml" \
  && printf 'PASS: retirement preserves unrelated config settings\n' \
  || fail "retirement dropped unrelated config settings"
if python3 -c 'import sys, tomllib; tomllib.load(open(sys.argv[1], "rb"))' \
    "$(tenant_state_dir alice)/config.toml"; then
  printf 'PASS: retired route cleanup leaves valid TOML\n'
else
  fail "retired route cleanup produced invalid TOML"
fi
[[ ! -e "$(tenant_quadlet_dir alice)/lunarwing-nanocode-alice.container" ]] \
  && printf 'PASS: retirement removes Nanocode Quadlet\n' \
  || fail "retirement kept Nanocode Quadlet"

write_legacy_nanocode_config
INIT_SYSTEM="openrc"
CALLS=()
retire_tenant_nanocode alice
assert_call "rc:lunarwing-nanocode-alice-sup stop"
assert_call "rc:lunarwing-nanocode-alice stop"
assert_call "deregister-babysitter:lunarwing-nanocode-alice"
assert_call "rc-update:del lunarwing-nanocode-alice default"
assert_call "legacy-ctr:rootful:alice stop lunarwing-nanocode-alice"
assert_call "legacy-ctr:rootless:alice stop lunarwing-nanocode-alice"

write_legacy_nanocode_config
INIT_SYSTEM="systemd"
CALLS=()
LEGACY_STOP_FAILURE="rootful"
if retire_tenant_nanocode alice >/dev/null 2>&1; then
  fail "retirement ignored a legacy container stop failure"
else
  printf 'PASS: retirement fails when a legacy container cannot stop\n'
fi
LEGACY_STOP_FAILURE=""
grep -q 'name = "nanocode"' "$(tenant_state_dir alice)/config.toml" \
  && printf 'PASS: failed retirement preserves route for recovery\n' \
  || fail "failed retirement removed route and hid running worker"
[[ -e "$(tenant_quadlet_dir alice)/lunarwing-nanocode-alice.container" ]] \
  && printf 'PASS: failed retirement preserves supervision for recovery\n' \
  || fail "failed retirement removed supervision"

INIT_SYSTEM="systemd"
CALLS=()
uninstall_tenant_systemd alice
assert_no_call "ctr:alice rm -f lunarwing-nanocode-alice"
assert_call "ctr:alice rm -f lunarwing-pebble-alice"
assert_call "ctr:alice rm -f lunarwing-opencode-alice"
assert_call "ctr:alice rm -f lunarwing-kimi-alice"

RETIRE_FAILURE="false"
retire_tenant_nanocode() {
  CALLS+=("retire:$1")
  [[ "$RETIRE_FAILURE" != "true" ]]
}
stop_tenant_systemd() { CALLS+=("stop-systemd:$1"); }
uninstall_tenant_systemd() { CALLS+=("uninstall-systemd:$1"); }
stop_tenant_postgres() { CALLS+=("stop-pg:$1"); }
ports_deallocate() { CALLS+=("deallocate:$1"); }
remove_tenant_user() { CALLS+=("remove-user:$1:$2"); }
all_tenant_names() { printf 'alice\n'; }
CALLS=()
remove_tenant alice false
assert_call "retire:alice"
assert_before "retire:alice" "uninstall-systemd:alice"

CALLS=()
RETIRE_FAILURE="true"
if remove_tenant alice false >/dev/null 2>&1; then
  fail "tenant removal continued after Nanocode retirement failure"
else
  printf 'PASS: tenant removal aborts after Nanocode retirement failure\n'
fi
assert_no_call "uninstall-systemd:alice"

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all worker lifecycle tests passed\n'
