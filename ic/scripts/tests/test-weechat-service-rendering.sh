#!/usr/bin/env bash
# Standalone test harness for WeeChat service rendering (Todo 2).
# Verifies: dedicated weechat.env (RELAY_PASSWORD only, mode 0600),
# systemd EnvironmentFile= path, OpenRC post-drop env loading, tmux command
# preservation, and renderer helper extraction.
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

assert_fail() {  # <label> <cmd...>
  if "$@" >/dev/null 2>&1; then
    echo "  FAIL: $1 (expected nonzero exit)"
    failures=$((failures + 1))
  else
    echo "  PASS: $1"
  fi
}

assert_ok() {  # <label> <cmd...>
  if "$@" >/dev/null 2>&1; then
    echo "  PASS: $1"
  else
    echo "  FAIL: $1 (expected zero exit)"
    failures=$((failures + 1))
  fi
}

# ── Fixture directory ──────────────────────────────────────────────────────
MT_FIXTURE="$(mktemp -d)"
trap 'rm -rf "$MT_FIXTURE"' EXIT

# Stub tenant_* path helpers to point at the fixture dir.
tenant_home()         { echo "$MT_FIXTURE/$1"; }
tenant_env_dir()      { echo "$MT_FIXTURE/$1/lunarwing/env"; }
tenant_state_dir()    { echo "$MT_FIXTURE/$1/lunarwing/state"; }
tenant_run_dir()      { echo "$MT_FIXTURE/$1/lunarwing/run"; }
tenant_log_dir()      { echo "$MT_FIXTURE/$1/lunarwing/logs"; }
tenant_weechat_home() { printf '%s/.config/weechat' "$(tenant_home "$1")"; }

# Stub ports_get to return deterministic ports.
ports_get() {  # <name> <service>
  case "$2" in
    weechat) echo 18000 ;;
    *)       echo 0 ;;
  esac
}

chown() { :; }
chmod() { :; }

FIXTURE_PASSWORD="render-secret-abc"

write_fixture_env() {  # <tenant>
  local env_dir
  env_dir="$(tenant_env_dir "$1")"
  mkdir -p "$env_dir"
  cat >"$env_dir/lunarwing.env" <<ENV
GATEWAY_AUTH_TOKEN=gatewaytok
XMPP_PASSWORD=xmppsecret
LLM_API_KEY=llmsecret
DATABASE_URL=postgres://user:dbpass@127.0.0.1:5432/db
RELAY_PASSWORD=$FIXTURE_PASSWORD
ENV
}

echo "=== _write_weechat_env tests ==="

write_fixture_env "render-test"

# Run in subshell to capture exit without killing the test under set -e
( _write_weechat_env "render-test" "$FIXTURE_PASSWORD" ) || true

env_file="$(tenant_env_dir "render-test")/weechat.env"
if [[ -f "$env_file" ]]; then
  echo "  PASS: weechat.env exists"
else
  echo "  FAIL: weechat.env exists"
  failures=$((failures + 1))
fi

# Exactly one assignment: RELAY_PASSWORD=<password>
line_count="$(wc -l <"$env_file" | tr -d ' ')"
assert_eq "exactly one line" "$line_count" "1"

# Contains RELAY_PASSWORD assignment
assert_eq "contains RELAY_PASSWORD assignment" \
  "$(cat "$env_file")" "RELAY_PASSWORD=$FIXTURE_PASSWORD"

# Does NOT contain any unrelated secret
if grep -q 'DATABASE_URL\|LLM_API_KEY\|XMPP_PASSWORD\|GATEWAY_AUTH_TOKEN\|dbpass\|xmpsecret\|llmsecret\|gatewaytok' "$env_file"; then
  echo "  FAIL: weechat.env leaks unrelated secrets"
  failures=$((failures + 1))
else
  echo "  PASS: weechat.env contains only RELAY_PASSWORD"
fi

# Mode is 0600 (stat strips leading zero, so compare both)
perms="$(stat -c '%a' "$env_file" 2>/dev/null || stat -f '%Lp' "$env_file" 2>/dev/null)"
if [[ "$perms" == "600" || "$perms" == "0600" ]]; then
  echo "  PASS: mode is 0600"
else
  echo "  FAIL: mode is 0600"
  echo "        expected: [0600]"
  echo "        actual:   [$perms]"
  failures=$((failures + 1))
fi

legacy_temp="$(tenant_env_dir "render-test")/.weechat.env.tmp.$$"
printf 'preserve-sentinel\n' >"$legacy_temp"
_write_weechat_env "render-test" "$FIXTURE_PASSWORD"
assert_eq "weechat.env writer uses a unique temp file" \
  "$(<"$legacy_temp")" "preserve-sentinel"

env_conflict_tenant="render-env-directory-conflict"
mkdir -p "$(tenant_env_dir "$env_conflict_tenant")/weechat.env"
if _write_weechat_env "$env_conflict_tenant" "$FIXTURE_PASSWORD" >/dev/null 2>&1; then
  echo "  FAIL: weechat.env directory conflict should fail"
  failures=$((failures + 1))
else
  echo "  PASS: weechat.env directory conflict fails safely"
fi
env_conflict_entries="$(ls -A "$(tenant_env_dir "$env_conflict_tenant")/weechat.env" | wc -l)"
assert_eq "weechat.env conflict leaves directory empty" "$env_conflict_entries" "0"

echo "=== systemd renderer helper ==="

systemd_out="$MT_FIXTURE/systemd-output"
mkdir -p "$systemd_out"
( _write_weechat_env "render-test" "$FIXTURE_PASSWORD" ) || true
( _render_weechat_systemd_unit "render-test" "$systemd_out" ) || true

unit_file="$systemd_out/lunarwing-weechat-render-test.service"
if [[ -f "$unit_file" ]]; then
  echo "  PASS: systemd unit file exists"
else
  echo "  FAIL: systemd unit file exists"
  failures=$((failures + 1))
fi
assert_eq "unit name is lunarwing-weechat-<tenant>" \
  "$(basename "$unit_file")" "lunarwing-weechat-render-test.service"

# Contains EnvironmentFile pointing at weechat.env
expected_env_line="EnvironmentFile=$(tenant_env_dir "render-test")/weechat.env"
if grep -qF "$expected_env_line" "$unit_file"; then
  echo "  PASS: systemd unit has EnvironmentFile=.../weechat.env"
else
  echo "  FAIL: systemd unit missing EnvironmentFile"
  echo "        expected: $expected_env_line"
  failures=$((failures + 1))
fi

# Does NOT point at lunarwing.env
if grep -q 'EnvironmentFile=.*lunarwing\.env' "$unit_file"; then
  echo "  FAIL: systemd unit points at lunarwing.env (wrong)"
  failures=$((failures + 1))
else
  echo "  PASS: systemd does not point at lunarwing.env"
fi

# Preserves tmux start command with weechat --dir
if grep -q 'tmux.*new-session.*weechat.*--dir' "$unit_file"; then
  echo "  PASS: systemd preserves tmux + weechat --dir command"
else
  echo "  FAIL: systemd missing tmux weechat command"
  failures=$((failures + 1))
fi

# Preserves weechat_home dir path
if grep -q "$(tenant_weechat_home "render-test")" "$unit_file"; then
  echo "  PASS: systemd contains weechat_home path"
else
  echo "  FAIL: systemd missing weechat_home path"
  failures=$((failures + 1))
fi

echo "=== OpenRC renderer helper ==="

openrc_out="$MT_FIXTURE/openrc-output"
mkdir -p "$openrc_out"
( _write_weechat_env "render-test" "$FIXTURE_PASSWORD" ) || true
( _render_weechat_openrc_unit "render-test" "$openrc_out/lunarwing-weechat-render-test" ) || true

rc_file="$openrc_out/lunarwing-weechat-render-test"
if [[ -f "$rc_file" ]]; then
  echo "  PASS: OpenRC script exists"
else
  echo "  FAIL: OpenRC script exists"
  failures=$((failures + 1))
fi
assert_eq "script name is lunarwing-weechat-<tenant>" \
  "$(basename "$rc_file")" "lunarwing-weechat-render-test"

# Has weechat_env_file assignment pointing at weechat.env (in := default pattern)
expected_env_file_path="$(tenant_env_dir "render-test")/weechat.env"
if grep -qF "$expected_env_file_path" "$rc_file" && grep -q 'weechat_env_file' "$rc_file"; then
  echo "  PASS: OpenRC sets weechat_env_file=.../weechat.env"
else
  echo "  FAIL: OpenRC missing weechat_env_file assignment"
  echo "        expected path: $expected_env_file_path"
  failures=$((failures + 1))
fi

# Does NOT point at lunarwing.env
if grep -q 'weechat_env_file=.*lunarwing\.env' "$rc_file"; then
  echo "  FAIL: OpenRC weechat_env_file points at lunarwing.env (wrong)"
  failures=$((failures + 1))
else
  echo "  PASS: OpenRC weechat_env_file does not point at lunarwing.env"
fi

# Uses the root-owned launcher as the executable that start-stop-daemon drops to
# the tenant before it can open weechat.env.
if grep -q 'weechat_openrc_env_exec' "$rc_file"; then
  echo "  PASS: OpenRC sets the post-drop env launcher"
else
  echo "  FAIL: OpenRC missing post-drop env launcher"
  failures=$((failures + 1))
fi

if grep -qF -- '--exec "${weechat_openrc_env_exec}"' "$rc_file" \
  && grep -qF -- '--env-file "${weechat_env_file}" --' "$rc_file"; then
  echo "  PASS: OpenRC starts WeeChat through the post-drop env launcher"
else
  echo "  FAIL: OpenRC does not start WeeChat through the post-drop env launcher"
  failures=$((failures + 1))
fi

if grep -q 'load_env()' "$rc_file" \
  || grep -Eq '(sed|cat|head|tail).*weechat_env_file' "$rc_file" \
  || grep -Eq '(^|[[:space:]])(\.|source)[[:space:]].*weechat_env_file' "$rc_file"; then
  echo "  FAIL: OpenRC reads tenant-controlled WeeChat env content as root"
  failures=$((failures + 1))
else
  echo "  PASS: OpenRC does not read tenant-controlled WeeChat env content as root"
fi

if grep -qF 'command_user="${weechat_user}:${weechat_group}"' "$rc_file" \
  && grep -qF -- '--user "${weechat_user}"' "$rc_file"; then
  echo "  PASS: OpenRC retains tenant privilege-drop configuration"
else
  echo "  FAIL: OpenRC missing tenant privilege-drop configuration"
  failures=$((failures + 1))
fi

# Preserves tmux command for starting weechat (same tmux invocation as systemd)
if grep -q 'weechat_command:=.*tmux' "$rc_file" \
  && grep -q 'new-session.*weechat.*--dir' "$rc_file"; then
  echo "  PASS: OpenRC preserves tmux + weechat --dir start"
else
  echo "  FAIL: OpenRC missing tmux weechat start"
  failures=$((failures + 1))
fi

# ── Summary ─────────────────────────────────────────────────────────────────
echo ""
if [[ "$failures" -eq 0 ]]; then
  echo "ALL TESTS PASSED"
  exit 0
else
  echo "$failures TEST(S) FAILED"
  exit 1
fi
