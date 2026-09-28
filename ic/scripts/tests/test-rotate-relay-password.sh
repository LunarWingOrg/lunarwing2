#!/usr/bin/env bash
# Fixture-only rotation test: no live services or secrets are touched.
# shellcheck disable=SC1091,SC2034  # dynamic source and fixture globals consumed by mt-admin
set -euo pipefail
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d)"
trap 'rm -rf -- "$fixture"' EXIT
# Exercise the real post-drop launcher even when CI invokes this test as root.
launcher_user=()
if [[ "$EUID" == 0 ]]; then
  launcher_user=(runuser -u nobody --)
  chmod 755 "$fixture"
fi
RELAY_ENV_EXEC="$fixture/relay-env-exec"
cp "$RELAY_ENV_EXEC_SRC" "$RELAY_ENV_EXEC"
chmod 755 "$RELAY_ENV_EXEC"
OPENRC_INITD_DIR="$fixture/init.d"
tenant_home() { printf '%s/%s\n' "$fixture" "$1"; }
tenant_env_dir() { printf '%s/%s/env\n' "$fixture" "$1"; }
tenant_weechat_home() { printf '%s/%s/weechat\n' "$fixture" "$1"; }
# The fixture tenant is "test"; map its privilege drop to a real unprivileged
# user without requiring a system account named test.
sudo() {
  [[ "$1" == -u && "$2" == test && "$3" == env ]] || return 1
  shift 2
  if [[ "$EUID" == 0 ]]; then
    runuser -u nobody -- "$@"
  else
    "$@"
  fi
}
tenant_exists_in_registry() { [[ "$1" == test ]]; }
tenant_weechat_enabled() { return 0; }
ports_get() { printf '18005\n'; }
ensure_init_system() { :; }
chown() { :; }
INIT_SYSTEM=systemd # consumed by the sourced mt-admin library
state="$fixture/services"
mkdir -p "$state" "$(tenant_env_dir test)" "$(tenant_weechat_home test)"
mkdir -p "$OPENRC_INITD_DIR"
chmod 700 "$(tenant_env_dir test)"
touch "$(tenant_env_dir test)/.relay-start.lock"
chmod 600 "$(tenant_env_dir test)/.relay-start.lock"
for svc in lunarwing-test lunarwing-weechat-test lunarwing-weechat-adapter-test; do
  touch "$state/$svc"
  printf 'command_args="python3 %s --env-file fixture -- target"\n' "$RELAY_ENV_EXEC" >"$OPENRC_INITD_DIR/$svc"
done
start_consumer() {
  local svc="$1" file="$lw"
  printf '%s\n' "$svc" >>"$fixture/start-attempts"
  [[ "$svc" != lunarwing-weechat-test ]] || file="$wc"
  "${launcher_user[@]}" python3 "$RELAY_ENV_EXEC" --env-file "$file" -- /usr/bin/touch "$state/$svc"
}
_systemctl_user() {
  local action="$2" svc="${!#}"
  svc="${svc%.service}"
  case "$action" in
    show-environment) : ;;
    reset-failed) : ;;
    show) printf 'python3 %s --env-file fixture -- target\n' "$RELAY_ENV_EXEC" ;;
    is-active) [[ -f "$state/$svc" ]] ;;
    stop) rm -f "$state/$svc" ;;
    start) start_consumer "$svc" ;;
    restart) rm -f "$state/$svc"; start_consumer "$svc" ;;
    *) return 1 ;;
  esac
}
generate_token() {
  local n=0
  [[ ! -f "$fixture/counter" ]] || n="$(<"$fixture/counter")"
  n=$((n + 1))
  printf '%s\n' "$n" >"$fixture/counter"
  printf '%08d%s\n' "$n" 'abcdabcdabcdabcdabcdabcdabcdabcd'
}
lw="$(tenant_env_dir test)/lunarwing.env"
wc="$(tenant_env_dir test)/weechat.env"
printf 'OTHER_KEY=preserve\nRELAY_PASSWORD=old-password\n' >"$lw"
printf 'RELAY_PASSWORD=old-password\n' >"$wc"
chmod 600 "$lw" "$wc"
if [[ "$EUID" == 0 ]]; then
  command chown -R nobody "$fixture/test" "$state"
  # The production writers preserve tenant ownership too.
  chown() { command chown nobody "$2"; }
  prepare_relay_start_barrier test
  lock_inode="$(stat -c %i "$(tenant_env_dir test)/.relay-start.lock")"
  mkdir "$(tenant_env_dir test)/.relay-rotation-blocked"
  prepare_relay_start_barrier test
  [[ "$lock_inode" == "$(stat -c %i "$(tenant_env_dir test)/.relay-start.lock")" ]] \
    || { echo 'FAIL: rendering replaced lock inode' >&2; exit 1; }
  [[ -d "$(tenant_env_dir test)/.relay-rotation-blocked" ]] \
    || { echo 'FAIL: rendering cleared recovery marker' >&2; exit 1; }
  [[ "$(stat -c '%u:%g:%a' "$RELAY_ENV_EXEC")" == 0:0:755 ]] \
    || { echo 'FAIL: launcher install permissions' >&2; exit 1; }
  rmdir "$(tenant_env_dir test)/.relay-rotation-blocked"
fi
cat >"$(tenant_weechat_home test)/relay.conf" <<'EOF'
[network]
password = "${env:RELAY_PASSWORD}"
bind_address = "127.0.0.1"
allow_empty_password = off
ipv6 = off
[api]
api = 18005
EOF

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
check() {
  local value
  value="$(_read_env_value "$lw" RELAY_PASSWORD)"
  [[ -n "$value" && "$value" == "$(_read_env_value "$wc" RELAY_PASSWORD)" ]] || fail 'split credentials'
  [[ "$(stat -c %a "$lw")" == 600 && "$(stat -c %a "$wc")" == 600 ]] || fail 'permissions'
  [[ "$(<"$lw")" == *OTHER_KEY=preserve* ]] || fail 'other env fields lost'
  # shellcheck disable=SC2016  # require WeeChat's literal expression
  grep -qF '${env:RELAY_PASSWORD}' "$(tenant_weechat_home test)/relay.conf" || fail 'config expression lost'
}
# An attacker-controlled final path must not redirect either tenant-side writer
# to a root-owned file. This also checks the former root chmod/chown path.
sentinel="$fixture/root-sentinel"
printf 'untouched\n' >"$sentinel"
rm "$wc"
ln -s "$sentinel" "$wc"
if _write_weechat_env test blocked 2>/dev/null; then fail 'accepted weechat.env symlink'; fi
[[ "$(<"$sentinel")" == untouched ]] || fail 'weechat writer changed symlink target'
rm "$wc"
printf 'RELAY_PASSWORD=old-password\n' >"$wc"
chmod 600 "$wc"
rm "$lw"
ln -s "$sentinel" "$lw"
if _relay_env_set_password test "$lw" blocked 2>/dev/null; then fail 'accepted lunarwing.env symlink'; fi
[[ "$(<"$sentinel")" == untouched ]] || fail 'relay writer changed symlink target'
rm "$lw"
printf 'OTHER_KEY=preserve\nRELAY_PASSWORD=old-password\n' >"$lw"
chmod 600 "$lw"
rotate_tenant_weechat_relay_password test
check
first="$(_read_env_value "$lw" RELAY_PASSWORD)"
[[ "$first" != old-password ]] || fail 'no rotation'
rotate_tenant_weechat_relay_password test
check
second="$(_read_env_value "$lw" RELAY_PASSWORD)"
[[ "$first" != "$second" ]] || fail 'rerun reused password'

# Simulate the second file's write failing. The first must be rolled back,
# and the services must never restart with mismatched credentials.
original_writer="$(declare -f _write_weechat_env)"
self_heal_restart() (
  local service="$1"
  set --
  # Same source boundary as the infrastructure-health test harness. Keep its
  # real restart dispatch, replacing only the init/privilege transport.
  # shellcheck disable=SC1090  # deliberately exclude the self-heal main entry point
  source <(sed '/^# HARNESS_ENTRY_POINT/,$d' "$LUNARWING_ROOT/ic-infrastructure-health-check/lunarwing-self-heal.sh")
  SERVICE_MANAGER="$INIT_SYSTEM"
  _resolve_systemd_tenant_user() { id -un; }
  _sh_timeout() { "$@"; }
  sudo() {
    [[ "$1" == -n && "$2" == -u && "$4" == env ]] || return 1
    shift 5
    "$@"
  }
  systemctl() {
    [[ "$1" == --user ]] || fail 'self-heal used system bus'
    shift
    _systemctl_user test "$@"
  }
  restart_service "$service"
)
assert_starts_blocked() {
  local svc attempts
  for svc in lunarwing-test lunarwing-weechat-test lunarwing-weechat-adapter-test; do
    attempts="$(wc -l <"$fixture/start-attempts")"
    if _relay_service_action test start "$svc" 2>/dev/null; then
      fail "external start bypassed rotation barrier: $svc"
    fi
    if self_heal_restart "$svc" 2>/dev/null; then
      fail "self-heal bypassed rotation barrier: $svc"
    fi
    [[ "$(wc -l <"$fixture/start-attempts")" == "$((attempts + 2))" ]] \
      || fail 'external/self-heal attempt did not reach the real launcher'
  done
  [[ -z "$(ls -A "$state")" ]] || fail 'consumer started during split write'
}
_write_weechat_env() {
  [[ -z "$(ls -A "$state")" ]] || fail 'wrote credential with a consumer still active'
  assert_starts_blocked
  if [[ ! -f "$fixture/failed" ]]; then
    touch "$fixture/failed"
    return 1
  fi
  original_write_weechat_env "$@"
}
eval "${original_writer/_write_weechat_env ()/original_write_weechat_env ()}"
if rotate_tenant_weechat_relay_password test; then fail 'accepted failed write'; fi
check
[[ "$second" == "$(_read_env_value "$lw" RELAY_PASSWORD)" ]] || fail 'rollback lost old password'
for svc in lunarwing-test lunarwing-weechat-test lunarwing-weechat-adapter-test; do
  [[ -f "$state/$svc" ]] || fail "failed to restore $svc"
done

# The same lifecycle is used on OpenRC; only the service action adapter differs.
rc-service() {
  local svc="$1" action="$2"
  case "$action" in
    status) [[ -f "$state/$svc" ]] ;;
    stop) rm -f "$state/$svc" ;;
    start) start_consumer "$svc" ;;
    restart) rm -f "$state/$svc"; start_consumer "$svc" ;;
    *) return 1 ;;
  esac
}
INIT_SYSTEM=openrc
rotate_tenant_weechat_relay_password test
check
[[ "$second" != "$(_read_env_value "$lw" RELAY_PASSWORD)" ]] || fail 'OpenRC rotation reused password'

# Do not bring an intentionally stopped tenant up just to rotate its files.
rm -f "$state"/*
rotate_tenant_weechat_relay_password test
check
[[ -z "$(ls -A "$state")" ]] || fail 'started an inactive service'

# Refuse a damaged configuration before taking any services down.
sed -i 's/127\.0\.0\.1/0.0.0.0/' "$(tenant_weechat_home test)/relay.conf"
before="$(_read_env_value "$lw" RELAY_PASSWORD)"
if (rotate_tenant_weechat_relay_password test); then fail 'accepted unsafe relay.conf'; fi
[[ "$before" == "$(_read_env_value "$lw" RELAY_PASSWORD)" ]] || fail 'changed credential on invalid config'
sed -i 's/0\.0\.0\.0/127.0.0.1/' "$(tenant_weechat_home test)/relay.conf"

# A killed writer must leave a durable block even after its flock disappears.
_write_weechat_env() { assert_starts_blocked; kill -KILL "$BASHPID"; }
{ if rotate_tenant_weechat_relay_password test; then fail 'killed rotation succeeded'; fi; } 2>"$fixture/killed.log"
[[ -d "$(tenant_env_dir test)/.relay-rotation-blocked" ]] || fail 'interruption lost recovery marker'
assert_starts_blocked
if rotate_tenant_weechat_relay_password test; then fail 'accepted interrupted split credentials'; fi
# Operator reconciles the pair, then retries rotation; no manual marker deletion.
_relay_env_set_password test "$lw" "$(_read_env_value "$wc" RELAY_PASSWORD)"
eval "$original_writer"
rotate_tenant_weechat_relay_password test
check
[[ ! -e "$(tenant_env_dir test)/.relay-rotation-blocked" ]] || fail 'recovery left starts blocked'

# Failed rollback must also retain the barrier for subsequent self-heal starts.
_write_weechat_env() {
  if [[ ! -f "$fixture/rollback-failed" ]]; then
    original_write_weechat_env "$@"
    touch "$fixture/rollback-failed"
  fi
  return 1
}
if rotate_tenant_weechat_relay_password test; then fail 'accepted failed rollback'; fi
assert_starts_blocked
[[ -d "$(tenant_env_dir test)/.relay-rotation-blocked" ]] || fail 'rollback failure lost barrier'
_relay_env_set_password test "$lw" "$(_read_env_value "$wc" RELAY_PASSWORD)"
eval "$original_writer"
rotate_tenant_weechat_relay_password test
check

# Old deployed units cannot provide exclusion; refuse before stopping/writing.
rm "$OPENRC_INITD_DIR/lunarwing-test"
before="$(_read_env_value "$lw" RELAY_PASSWORD)"
if rotate_tenant_weechat_relay_password test; then fail 'accepted unguarded unit'; fi
[[ "$before" == "$(_read_env_value "$lw" RELAY_PASSWORD)" ]] || fail 'old unit preflight mutated password'
printf 'PASS: systemd/OpenRC rotation, concurrent starts, rollback, interruption and recovery\n'
python3 "$SCRIPT_DIR/tests/test-relay-start-barrier.py"
