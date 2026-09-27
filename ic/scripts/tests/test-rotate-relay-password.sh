#!/usr/bin/env bash
# Fixture-only rotation test: no live services or secrets are touched.
# shellcheck disable=SC1091,SC2034  # dynamic source and fixture globals consumed by mt-admin
set -euo pipefail
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d)"
trap 'rm -rf -- "$fixture"' EXIT
tenant_home() { printf '%s/%s\n' "$fixture" "$1"; }
tenant_env_dir() { printf '%s/%s/env\n' "$fixture" "$1"; }
tenant_weechat_home() { printf '%s/%s/weechat\n' "$fixture" "$1"; }
tenant_exists_in_registry() { [[ "$1" == test ]]; }
tenant_weechat_enabled() { return 0; }
ports_get() { printf '18005\n'; }
ensure_init_system() { :; }
chown() { :; }
INIT_SYSTEM=systemd # consumed by the sourced mt-admin library
state="$fixture/services"
mkdir -p "$state" "$(tenant_env_dir test)" "$(tenant_weechat_home test)"
for svc in lunarwing-test lunarwing-weechat-test lunarwing-weechat-adapter-test; do
  touch "$state/$svc"
done
_systemctl_user() {
  local action="$2" svc="${!#}"
  svc="${svc%.service}"
  case "$action" in
    show-environment) : ;;
    is-active) [[ -f "$state/$svc" ]] ;;
    stop) rm -f "$state/$svc" ;;
    start) [[ "$(<"$(tenant_env_dir test)/lunarwing.env")" == *"RELAY_PASSWORD=$(_read_env_value "$(tenant_env_dir test)/weechat.env" RELAY_PASSWORD)"* ]] || return 1
           touch "$state/$svc" ;;
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
_write_weechat_env() {
  [[ -z "$(ls -A "$state")" ]] || fail 'wrote credential with a consumer still active'
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
    start) [[ "$(_read_env_value "$lw" RELAY_PASSWORD)" == "$(_read_env_value "$wc" RELAY_PASSWORD)" ]] || return 1
           touch "$state/$svc" ;;
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
printf 'PASS: systemd/OpenRC rotation, rerun uniqueness, rollback and service restore\n'
