#!/usr/bin/env bash
#
# test-weechat-preflight.sh — TDD harness for WeeChat preflight generated-relay
# and minimal-env checks (Todo 4 of weechat-relay-auto-bootstrap plan).
#
# Uses LUNARWING_PORTS_REGISTRY and LUNARWING_TENANT_HOME_BASE overrides to run
# the preflight against fixture directories.  No sudo, no services, no root.
# The fixture secret must NEVER appear in any captured preflight output.
#
# Scenarios covered:
#   1. valid setup          — exit 0, all OK
#   2. absent relay config  — exit 1, FAIL
#   3. wrong API port       — exit 1, FAIL
#   4. non-loopback bind    — exit 1, FAIL
#   5. literal-password leak — exit 1, FAIL, no secret in output
#   6. missing minimal env  — exit 1, FAIL
#   7. adapter-down         — INFO (not FAIL), exit 0
#   8. setup-error          — exit 2
#   9. empty-lw-relay-pw    — empty lunarwing.env RELAY_PASSWORD with valid
#                             expression-based relay.conf; must NOT falsely
#                             report a plaintext password leak
#  10. mismatch-pw          — non-empty weechat.env RELAY_PASSWORD that differs
#                             from lunarwing.env; must FAIL and exit 1
#  11. ipv6-on              — IPv4 loopback with IPv6 enabled; must FAIL
#  12. missing-ipv6-option  — missing explicit IPv6-off invariant; must FAIL
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFLIGHT="$SCRIPT_DIR/../lunarwing-weechat-preflight.sh"

# Fixture secret — must NEVER appear in preflight output.
FIXTURE_SECRET='FIXTURE_RELAY_SECRET_aaa111'
# Second distinct non-empty secret for mismatch tests — also must NEVER appear.
MISMATCH_SECRET='FIXTURE_RELAY_SECRET_bbb222'

failures=0
total=0

assert_exit() {  # <label> <expected_exit> <actual_exit>
  total=$((total + 1))
  if [[ "$2" == "$3" ]]; then
    echo "  PASS: $1 (exit $3)"
  else
    echo "  FAIL: $1"
    echo "        expected exit $2, got $3"
    failures=$((failures + 1))
  fi
}

assert_in() {  # <label> <haystack> <needle>
  total=$((total + 1))
  if [[ "$2" == *"$3"* ]]; then
    echo "  PASS: $1"
  else
    echo "  FAIL: $1"
    echo "        expected output to contain: $3"
    failures=$((failures + 1))
  fi
}

assert_not_in() {  # <label> <haystack> <needle>
  total=$((total + 1))
  if [[ "$2" != *"$3"* ]]; then
    echo "  PASS: $1"
  else
    echo "  FAIL: $1"
    echo "        output must NOT contain: $3"
    failures=$((failures + 1))
  fi
}

# ---- Fixture state ----

FIXTURE_ROOT=""
REGISTRY=""
TENANT_HOME=""
TENANT_NAME="testtenant"
WEECHAT_PORT=9005
ADAPTER_PORT=9009

create_fixture() {  # <scenario>
  local scenario="${1:-valid}"

  FIXTURE_ROOT="$(mktemp -d)"
  trap 'cleanup_fixture' EXIT
  REGISTRY="$FIXTURE_ROOT/ports.json"
  TENANT_HOME="$FIXTURE_ROOT/home"

  local tdir="$TENANT_HOME/$TENANT_NAME"

  # Directory structure.
  mkdir -p "$tdir/lunarwing/env" \
           "$tdir/lunarwing/state/channels" \
           "$tdir/.config/weechat"

  # Port registry.
  cat > "$REGISTRY" <<JSON
{"tenants":{"$TENANT_NAME":{"base_port":9000,"ports":{"weechat":$WEECHAT_PORT,"weechat_adapter":$ADAPTER_PORT}}}}
JSON

  # lunarwing.env — satisfies existing env-var checks.
  case "$scenario" in
    empty-lw-relay-pw)
      cat > "$tdir/lunarwing/env/lunarwing.env" <<E
RELAY_URL=http://127.0.0.1:$WEECHAT_PORT
WS_ADAPTER_URL=http://127.0.0.1:$ADAPTER_PORT
ADAPTER_PORT=$ADAPTER_PORT
WEECHAT_ADAPTER_PORT=$ADAPTER_PORT
RELAY_PASSWORD=
E
      ;;
    *)
      cat > "$tdir/lunarwing/env/lunarwing.env" <<E
RELAY_URL=http://127.0.0.1:$WEECHAT_PORT
WS_ADAPTER_URL=http://127.0.0.1:$ADAPTER_PORT
ADAPTER_PORT=$ADAPTER_PORT
WEECHAT_ADAPTER_PORT=$ADAPTER_PORT
RELAY_PASSWORD=$FIXTURE_SECRET
E
      ;;
  esac

  # capabilities.json — satisfies existing capabilities check.
  cat > "$tdir/lunarwing/state/channels/weechat.capabilities.json" <<'JSON'
{"setup":{"required_fields":[{"name":"RELAY_PASSWORD","env":true}]}}
JSON

  # weechat.env — minimal credential env.
  case "$scenario" in
    missing-env)
      # Intentionally do not create weechat.env.
      ;;
    empty-env|empty-lw-relay-pw)
      printf 'RELAY_PASSWORD=\n' \
        > "$tdir/lunarwing/env/weechat.env"
      ;;
    mismatch-pw)
      printf 'RELAY_PASSWORD=%s\n' "$MISMATCH_SECRET" \
        > "$tdir/lunarwing/env/weechat.env"
      ;;
    *)
      printf 'RELAY_PASSWORD=%s\n' "$FIXTURE_SECRET" \
        > "$tdir/lunarwing/env/weechat.env"
      ;;
  esac

  # relay.conf — generated relay configuration.
  local rc="$tdir/.config/weechat/relay.conf"
  case "$scenario" in
    absent-config)
      # Intentionally do not create relay.conf.
      ;;
    wrong-port)
      printf '%s\n' \
        'password = "${env:RELAY_PASSWORD}"' \
        'bind_address = "127.0.0.1"' \
        'ipv6 = off' \
        '' \
        '[api]' \
        'api = 8888' \
        > "$rc"
      ;;
    non-loopback)
      printf '%s\n' \
        'password = "${env:RELAY_PASSWORD}"' \
        'bind_address = "0.0.0.0"' \
        'ipv6 = off' \
        '' \
        '[api]' \
        "api = $WEECHAT_PORT" \
        > "$rc"
      ;;
    password-leak)
      printf '%s\n' \
        'password = "${env:RELAY_PASSWORD}"' \
        "leaked_value = \"$FIXTURE_SECRET\"" \
        'bind_address = "127.0.0.1"' \
        'ipv6 = off' \
        '' \
        '[api]' \
        "api = $WEECHAT_PORT" \
        > "$rc"
      ;;
    ipv6-on)
      printf '%s\n' \
        'password = "${env:RELAY_PASSWORD}"' \
        'bind_address = "127.0.0.1"' \
        'ipv6 = on' \
        '' \
        '[api]' \
        "api = $WEECHAT_PORT" \
        > "$rc"
      ;;
    missing-ipv6-option)
      printf '%s\n' \
        'password = "${env:RELAY_PASSWORD}"' \
        'bind_address = "127.0.0.1"' \
        '' \
        '[api]' \
        "api = $WEECHAT_PORT" \
        > "$rc"
      ;;
    *)
      # valid (default for all other scenarios).
      printf '%s\n' \
        'password = "${env:RELAY_PASSWORD}"' \
        'bind_address = "127.0.0.1"' \
        'ipv6 = off' \
        '' \
        '[api]' \
        "api = $WEECHAT_PORT" \
        > "$rc"
      ;;
  esac
}

cleanup_fixture() {
  if [[ -n "${FIXTURE_ROOT:-}" && -d "${FIXTURE_ROOT:-}" ]]; then
    rm -rf "$FIXTURE_ROOT"
  fi
}

# Run preflight against current fixture; sets PREFLIGHT_OUT / PREFLIGHT_EXIT.
run_preflight() {
  PREFLIGHT_OUT=""
  PREFLIGHT_EXIT=0
  set +e
  PREFLIGHT_OUT="$(LUNARWING_PORTS_REGISTRY="$REGISTRY" \
    LUNARWING_TENANT_HOME_BASE="$TENANT_HOME" \
    bash "$PREFLIGHT" "$TENANT_NAME" 2>&1)"
  PREFLIGHT_EXIT=$?
  set -e
}

# ============================================================
# 1. Valid setup
# ============================================================
echo "=== 1. valid setup ==="
create_fixture valid
run_preflight
assert_exit "valid → exit 0" 0 "$PREFLIGHT_EXIT"
assert_in  "relay.conf OK label" "$PREFLIGHT_OUT" '[OK  ] relay.conf'
assert_in  "weechat.env OK label" "$PREFLIGHT_OUT" '[OK  ] weechat.env'
assert_in  "literal expression shown" "$PREFLIGHT_OUT" '${env:RELAY_PASSWORD}'
assert_in  "api port shown" "$PREFLIGHT_OUT" "api = $WEECHAT_PORT"
assert_not_in "no secret in output" "$PREFLIGHT_OUT" "$FIXTURE_SECRET"
# Adapter-down is expected in test env (no real adapter listening).
assert_in  "adapter health is INFO" "$PREFLIGHT_OUT" '[INFO] adapter health'
cleanup_fixture
trap - EXIT

# ============================================================
# 2. Absent relay config
# ============================================================
echo "=== 2. absent relay config ==="
create_fixture absent-config
run_preflight
assert_exit "absent config → exit 1" 1 "$PREFLIGHT_EXIT"
assert_in  "relay.conf FAIL" "$PREFLIGHT_OUT" '[FAIL] relay.conf'
assert_in  "recovery command" "$PREFLIGHT_OUT" 'configure-weechat-relay'
assert_not_in "no secret in output" "$PREFLIGHT_OUT" "$FIXTURE_SECRET"
cleanup_fixture
trap - EXIT

# ============================================================
# 3. Wrong API port
# ============================================================
echo "=== 3. wrong API port ==="
create_fixture wrong-port
run_preflight
assert_exit "wrong port → exit 1" 1 "$PREFLIGHT_EXIT"
assert_in  "relay.conf FAIL" "$PREFLIGHT_OUT" '[FAIL] relay.conf'
assert_in  "expected port mentioned" "$PREFLIGHT_OUT" "expected $WEECHAT_PORT"
assert_in  "recovery command" "$PREFLIGHT_OUT" 'configure-weechat-relay'
assert_not_in "no secret in output" "$PREFLIGHT_OUT" "$FIXTURE_SECRET"
cleanup_fixture
trap - EXIT

# ============================================================
# 4. Non-loopback bind
# ============================================================
echo "=== 4. non-loopback bind ==="
create_fixture non-loopback
run_preflight
assert_exit "non-loopback → exit 1" 1 "$PREFLIGHT_EXIT"
assert_in  "relay.conf FAIL" "$PREFLIGHT_OUT" '[FAIL] relay.conf'
assert_in  "loopback mentioned" "$PREFLIGHT_OUT" '127.0.0.1'
assert_not_in "no secret in output" "$PREFLIGHT_OUT" "$FIXTURE_SECRET"
cleanup_fixture
trap - EXIT

# ============================================================
# 5. Literal-password leak
# ============================================================
echo "=== 5. literal-password leak ==="
create_fixture password-leak
run_preflight
assert_exit "password leak → exit 1" 1 "$PREFLIGHT_EXIT"
assert_in  "relay.conf FAIL" "$PREFLIGHT_OUT" '[FAIL] relay.conf'
assert_in  "leak/security risk detail" "$PREFLIGHT_OUT" 'plaintext password'
assert_not_in "no secret in output" "$PREFLIGHT_OUT" "$FIXTURE_SECRET"
cleanup_fixture
trap - EXIT

# ============================================================
# 6. Missing minimal env
# ============================================================
echo "=== 6. missing minimal env ==="
create_fixture missing-env
run_preflight
assert_exit "missing env → exit 1" 1 "$PREFLIGHT_EXIT"
assert_in  "weechat.env FAIL" "$PREFLIGHT_OUT" '[FAIL] weechat.env'
assert_in  "recovery command" "$PREFLIGHT_OUT" 'configure-weechat-relay'
assert_not_in "no secret in output" "$PREFLIGHT_OUT" "$FIXTURE_SECRET"
cleanup_fixture
trap - EXIT

# ============================================================
# 7. Adapter-down informational behaviour
# ============================================================
# The valid fixture has no adapter listening — adapter health must be INFO,
# not FAIL, and the overall exit must be 0.
echo "=== 7. adapter-down informational ==="
create_fixture valid
run_preflight
assert_exit "adapter-down → exit 0" 0 "$PREFLIGHT_EXIT"
assert_in  "adapter health is INFO not FAIL" "$PREFLIGHT_OUT" '[INFO] adapter health'
assert_not_in "no adapter FAIL" "$PREFLIGHT_OUT" '[FAIL] adapter health'
assert_not_in "no secret in output" "$PREFLIGHT_OUT" "$FIXTURE_SECRET"
cleanup_fixture
trap - EXIT

# ============================================================
# 8. Setup-error exit 2
# ============================================================
echo "=== 8. setup-error exit 2 ==="
# Point at a non-existent registry → die → exit 2.
PREFLIGHT_EXIT=0
set +e
PREFLIGHT_OUT="$(LUNARWING_PORTS_REGISTRY="/nonexistent/ports.json" \
  LUNARWING_TENANT_HOME_BASE="/nonexistent/home" \
  bash "$PREFLIGHT" "$TENANT_NAME" 2>&1)"
PREFLIGHT_EXIT=$?
set -e
assert_exit "missing registry → exit 2" 2 "$PREFLIGHT_EXIT"
assert_in  "error message" "$PREFLIGHT_OUT" 'error:'
assert_not_in "no secret in output" "$PREFLIGHT_OUT" "$FIXTURE_SECRET"

# ============================================================
# 9. Empty lunarwing.env RELAY_PASSWORD with valid expression-based relay.conf
#    Must NOT falsely report a plaintext password leak.
#    relay.conf should be OK (valid expression); weechat.env FAILs (empty pw).
# ============================================================
echo "=== 9. empty-lw-relay-pw (no false plaintext-leak) ==="
create_fixture empty-lw-relay-pw
run_preflight
assert_exit "empty-lw-pw → exit 1" 1 "$PREFLIGHT_EXIT"
assert_not_in "no false plaintext leak" "$PREFLIGHT_OUT" 'plaintext password'
assert_in  "relay.conf still OK" "$PREFLIGHT_OUT" '[OK  ] relay.conf'
assert_in  "weechat.env FAIL (empty pw)" "$PREFLIGHT_OUT" '[FAIL] weechat.env'
assert_not_in "no secret in output" "$PREFLIGHT_OUT" "$FIXTURE_SECRET"

# ============================================================
# 10. Mismatched non-empty RELAY_PASSWORD between weechat.env and lunarwing.env
#     The regression emitted WARN and exited 0; this locks FAIL and exit 1.
# ============================================================
echo "=== 10. mismatch-pw (non-empty mismatch must FAIL) ==="
create_fixture mismatch-pw
run_preflight
assert_exit "mismatch-pw → exit 1" 1 "$PREFLIGHT_EXIT"
assert_in  "weechat.env FAIL" "$PREFLIGHT_OUT" '[FAIL] weechat.env'
assert_in  "mismatch detail" "$PREFLIGHT_OUT" 'does not match'
assert_not_in "no WARN for weechat.env" "$PREFLIGHT_OUT" '[WARN] weechat.env'
assert_not_in "no fixture secret in output" "$PREFLIGHT_OUT" "$FIXTURE_SECRET"
assert_not_in "no mismatch secret in output" "$PREFLIGHT_OUT" "$MISMATCH_SECRET"

# ============================================================
# 11. IPv6 enabled with IPv4 loopback bind
#     WeeChat 4.7.x refuses to bind: "invalid bind address '127.0.0.1' for IPv6".
#     A valid IPv4-only config MUST disable IPv6.
# ============================================================
echo "=== 11. ipv6-on (IPv4 loopback with ipv6 = on) ==="
create_fixture ipv6-on
run_preflight
assert_exit "ipv6-on → exit 1" 1 "$PREFLIGHT_EXIT"
assert_in  "relay.conf FAIL" "$PREFLIGHT_OUT" '[FAIL] relay.conf'
assert_in  "ipv6 detail" "$PREFLIGHT_OUT" 'ipv6'
assert_in  "ipv6 recovery" "$PREFLIGHT_OUT" '/set relay.network.ipv6 off'
assert_not_in "no secret in output" "$PREFLIGHT_OUT" "$FIXTURE_SECRET"
cleanup_fixture
trap - EXIT

# ============================================================
# 12. Missing ipv6 option
#     The generator must always emit relay.network.ipv6; a missing entry
#     means the config was not produced by the current bootstrap path.
# ============================================================
echo "=== 12. missing-ipv6-option ==="
create_fixture missing-ipv6-option
run_preflight
assert_exit "missing-ipv6 → exit 1" 1 "$PREFLIGHT_EXIT"
assert_in  "relay.conf FAIL" "$PREFLIGHT_OUT" '[FAIL] relay.conf'
assert_in  "ipv6 detail" "$PREFLIGHT_OUT" 'ipv6'
assert_in  "ipv6 recovery" "$PREFLIGHT_OUT" '/set relay.network.ipv6 off'
assert_not_in "no secret in output" "$PREFLIGHT_OUT" "$FIXTURE_SECRET"
cleanup_fixture
trap - EXIT

# ============================================================
# Summary
# ============================================================
echo ""
echo "----------------------------------------------------------------"
echo "Tests run: $total  Failures: $failures"
if [[ "$failures" -eq 0 ]]; then
  echo "ALL TESTS PASSED"
  exit 0
else
  echo "$failures TEST(S) FAILED"
  exit 1
fi
