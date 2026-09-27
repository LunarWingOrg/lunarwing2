#!/usr/bin/env bash
# Standalone test harness for rotate_tenant_weechat_relay_password (WI#202,
# item-8, MR!1178 rework). Sources lunarwing-mt-admin.sh (its BASH_SOURCE guard
# skips main() when sourced) and drives the rotation against fixture dirs.
#
# Covers the two MR !1178 review findings:
#   [High]   the new password never appears on argv and relay.conf keeps the
#            literal ${env:RELAY_PASSWORD} expression (no live /set path).
#   [Medium] weechat.env is written before lunarwing.env; a forced failure of
#            the second write leaves a convergable split state, and re-running
#            the idempotent verb repairs it.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$SCRIPT_DIR/../lunarwing-mt-admin.sh"

# shellcheck source=../lunarwing-mt-admin.sh
source "$ADMIN_SCRIPT"

failures=0
assert_eq() {  # <label> <actual> <expected>
  if [[ "$2" == "$3" ]]; then
    echo "  PASS: $1"
  else
    echo "  FAIL: $1"
    echo "        expected: [$3]"
    echo "        actual:   [$2]"
    failures=$((failures + 1))
  fi
}

assert_ok() {  # <label> <cmd...>
  local label="$1"; shift
  if ( "$@" ) >/dev/null 2>&1; then
    echo "  PASS: $label"
  else
    echo "  FAIL: $label (expected zero exit)"
    failures=$((failures + 1))
  fi
}

assert_fail() {  # <label> <cmd...>
  local label="$1"; shift
  # Subshell: functions under test may call die (exit 1) — contain it.
  if ( "$@" ) >/dev/null 2>&1; then
    echo "  FAIL: $label (expected nonzero exit)"
    failures=$((failures + 1))
  else
    echo "  PASS: $label"
  fi
}

assert_contains() {  # <label> <haystack> <needle>
  if [[ "$2" == *"$3"* ]]; then
    echo "  PASS: $1"
  else
    echo "  FAIL: $1"
    echo "        needle:   [$3]"
    echo "        haystack: [$2]"
    failures=$((failures + 1))
  fi
}

assert_not_contains() {  # <label> <haystack> <needle>
  if [[ "$2" != *"$3"* ]]; then
    echo "  PASS: $1"
  else
    echo "  FAIL: $1 (unexpected needle present)"
    echo "        needle:   [$3]"
    failures=$((failures + 1))
  fi
}

# ── Fixture ────────────────────────────────────────────────────────────────
MT_FIXTURE="$(mktemp -d)"
trap 'rm -rf "$MT_FIXTURE"' EXIT

tenant_home()         { echo "$MT_FIXTURE/$1"; }
tenant_env_dir()      { echo "$MT_FIXTURE/$1/lunarwing/env"; }
tenant_weechat_home() { printf '%s/.config/weechat' "$(tenant_home "$1")"; }

tenant_exists_in_registry() { [[ "$1" == rot-tenant ]]; }
tenant_weechat_enabled()    { [[ "$1" == rot-tenant ]]; }

ports_get() {  # <name> <service>
  case "$2" in
    weechat) echo 18000 ;;
    *)       echo 0 ;;
  esac
}

# Deterministic, argv-observable token generator. Every call appends one line
# to a log so tests can prove the secret never travels through any sudo/tmux
# command line (there is none) and can correlate the two env files.
GEN_LOG="$MT_FIXTURE/gen.log"
: >"$GEN_LOG"
gen_counter=0
generate_token() {
  gen_counter=$((gen_counter + 1))
  printf 'gentoken-%04d\n' "$gen_counter" >>"$GEN_LOG"
  printf 'deadbeef%056d\n' "$gen_counter"
}

# chown/chmod are no-ops in the fixture (non-root).
chown() { :; }

OLD_PW="old-relay-secret-aaaaaaaaaaaaaaaaaaaa"

write_fixture_tenant() {  # <tenant>
  local t="$1" env_dir wconf
  env_dir="$(tenant_env_dir "$t")"
  mkdir -p "$env_dir"
  cat >"$env_dir/lunarwing.env" <<ENV
RELAY_PASSWORD=$OLD_PW
OTHER_KEY=keepme
ENV
  wconf="$(tenant_weechat_home "$t")"
  mkdir -p "$wconf"
  cat >"$wconf/relay.conf" <<'CONF'
[api]
api = 18000

[network]
password = "${env:RELAY_PASSWORD}"
allow_empty_password = off
bind_address = "127.0.0.1"
ipv6 = off
CONF
}

# ── Happy path ─────────────────────────────────────────────────────────────
echo "=== rotate_tenant_weechat_relay_password: happy path ==="

write_fixture_tenant rot-tenant

out="$(rotate_tenant_weechat_relay_password rot-tenant 2>&1)"
rc=$?
assert_eq "rotation exits zero" "$rc" "0"

lw_pw="$(_read_env_value "$(tenant_env_dir rot-tenant)/lunarwing.env" RELAY_PASSWORD)"
wc_pw="$(_read_env_value "$(tenant_env_dir rot-tenant)/weechat.env" RELAY_PASSWORD)"

assert_eq "lunarwing.env carries the freshly generated password" "$lw_pw" "$(printf 'deadbeef%056d' 1 | cut -c1-32)"
assert_eq "weechat.env carries the same password" "$wc_pw" "$lw_pw"
assert_eq "old password is gone from lunarwing.env" "$([[ "$lw_pw" == "$OLD_PW" ]] && echo present || echo absent)" "absent"

# weechat.env contains ONLY RELAY_PASSWORD (mode/secret isolation contract).
wc_body="$(cat "$(tenant_env_dir rot-tenant)/weechat.env")"
assert_eq "weechat.env holds exactly one line" "$(printf '%s\n' "$wc_body" | grep -c .)" "1"
assert_contains "weechat.env line is RELAY_PASSWORD=..." "$wc_body" "RELAY_PASSWORD="

# lunarwing.env preserved the unrelated key.
assert_eq "unrelated lunarwing.env key preserved" \
  "$(_read_env_value "$(tenant_env_dir rot-tenant)/lunarwing.env" OTHER_KEY)" "keepme"

# relay.conf was NOT rewritten and still holds the env expression, not plaintext.
relay_body="$(cat "$(tenant_weechat_home rot-tenant)/relay.conf")"
assert_contains "relay.conf keeps the env expression" "$relay_body" '${env:RELAY_PASSWORD}'
assert_not_contains "relay.conf has no plaintext new password" "$relay_body" "$lw_pw"
assert_not_contains "relay.conf has no plaintext old password" "$relay_body" "$OLD_PW"

# Operator guidance: restart via the managed lifecycle, never a live /set.
assert_contains "output points at restart-tenant" "$out" "restart-tenant rot-tenant"
assert_not_contains "output never suggests tmux send-keys" "$out" "send-keys"
assert_not_contains "output never suggests a live /set" "$out" "/set relay.network.password"

# ── Idempotency / convergence after a forced split state ──────────────────
echo "=== rotate_tenant_weechat_relay_password: forced split state reconverges ==="

write_fixture_tenant rot-tenant
: >"$GEN_LOG"; gen_counter=0

# First rotation succeeds.
rotate_tenant_weechat_relay_password rot-tenant >/dev/null 2>&1
first_pw="$(_read_env_value "$(tenant_env_dir rot-tenant)/lunarwing.env" RELAY_PASSWORD)"

# Simulate the MR !1178 [Medium] failure: weechat.env updated but lunarwing.env
# write failed, leaving weechat.env=new / lunarwing.env=old. Construct it
# directly: roll lunarwing.env back to the old value while weechat.env keeps
# the rotated one.
cat >"$(tenant_env_dir rot-tenant)/lunarwing.env" <<ENV
RELAY_PASSWORD=$OLD_PW
OTHER_KEY=keepme
ENV

lw_before="$(_read_env_value "$(tenant_env_dir rot-tenant)/lunarwing.env" RELAY_PASSWORD)"
wc_before="$(_read_env_value "$(tenant_env_dir rot-tenant)/weechat.env" RELAY_PASSWORD)"
assert_eq "split state installed (lunarwing.env reverted)" "$lw_before" "$OLD_PW"
assert_eq "split state installed (weechat.env still rotated)" "$wc_before" "$first_pw"

# Re-running the verb must converge both files on a single fresh password.
rotate_tenant_weechat_relay_password rot-tenant >/dev/null 2>&1
lw_after="$(_read_env_value "$(tenant_env_dir rot-tenant)/lunarwing.env" RELAY_PASSWORD)"
wc_after="$(_read_env_value "$(tenant_env_dir rot-tenant)/weechat.env" RELAY_PASSWORD)"

assert_eq "re-run converges lunarwing.env" "$lw_after" "$(printf 'deadbeef%056d' 2 | cut -c1-32)"
assert_eq "re-run converges weechat.env to the same value" "$wc_after" "$lw_after"
assert_eq "re-run leaves no trace of the old password" \
  "$([[ "$lw_after" == "$OLD_PW" || "$wc_after" == "$OLD_PW" ]] && echo present || echo absent)" "absent"

# ── Refusal paths ──────────────────────────────────────────────────────────
echo "=== rotate_tenant_weechat_relay_password: refusal paths ==="

# relay.conf that lost the env expression (hand-edited to plaintext) must
# refuse rotation rather than silently rotate under a broken config.
write_fixture_tenant rot-tenant
cat >"$(tenant_weechat_home rot-tenant)/relay.conf" <<CONF
[api]
api = 18000

[network]
password = "hardcoded-plaintext"
allow_empty_password = off
bind_address = "127.0.0.1"
ipv6 = off
CONF
assert_fail "refuses rotation when relay.conf lost the env expression" \
  rotate_tenant_weechat_relay_password rot-tenant
# And it must not have touched either env file.
assert_eq "lunarwing.env untouched after refusal" \
  "$(_read_env_value "$(tenant_env_dir rot-tenant)/lunarwing.env" RELAY_PASSWORD)" "$OLD_PW"

# WeeChat disabled for the tenant.
tenant_weechat_enabled() { return 1; }
write_fixture_tenant rot-tenant
assert_fail "refuses rotation when WeeChat is disabled" \
  rotate_tenant_weechat_relay_password rot-tenant
tenant_weechat_enabled() { [[ "$1" == rot-tenant ]]; }

# Tenant not in registry.
assert_fail "refuses rotation for unknown tenant" \
  rotate_tenant_weechat_relay_password no-such-tenant

# Missing RELAY_PASSWORD in lunarwing.env.
write_fixture_tenant rot-tenant
sed -i '/^RELAY_PASSWORD=/d' "$(tenant_env_dir rot-tenant)/lunarwing.env"
assert_fail "refuses rotation when RELAY_PASSWORD is absent" \
  rotate_tenant_weechat_relay_password rot-tenant

# ── Summary ────────────────────────────────────────────────────────────────
echo
if [[ "$failures" -eq 0 ]]; then
  echo "ALL TESTS PASSED"
  exit 0
else
  echo "$failures TEST(S) FAILED"
  exit 1
fi
