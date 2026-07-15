#!/usr/bin/env bash
# Standalone test harness for WeeChat relay auto-bootstrap (Todos 1 + 3).
# Sources lunarwing-mt-admin.sh (the script's BASH_SOURCE guard skips main()
# when sourced, so no dispatch / require_root runs) and invokes the pure
# helpers + bootstrap functions against fixture directories.
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

assert_fail() {  # <label> <cmd...> — runs cmd, expects nonzero exit
  local label="$1"; shift
  if "$@" >/dev/null 2>&1; then
    echo "  FAIL: $label (expected nonzero exit)"
    failures=$((failures + 1))
  else
    echo "  PASS: $label"
  fi
}

assert_ok() {  # <label> <cmd...> — runs cmd, expects zero exit
  local label="$1"; shift
  if "$@" >/dev/null 2>&1; then
    echo "  PASS: $label"
  else
    echo "  FAIL: $label (expected zero exit)"
    failures=$((failures + 1))
  fi
}

# ── Fixture directory ──────────────────────────────────────────────────────
MT_FIXTURE="$(mktemp -d)"
trap 'rm -rf "$MT_FIXTURE"' EXIT

# Stub tenant_* path helpers to point at the fixture dir.
tenant_home()    { echo "$MT_FIXTURE/$1"; }
tenant_env_dir() { echo "$MT_FIXTURE/$1/lunarwing/env"; }
tenant_weechat_home() { printf '%s/.config/weechat' "$(tenant_home "$1")"; }

# Stub ports_get to return deterministic ports.
ports_get() {  # <name> <service>
  case "$2" in
    weechat) echo 18000 ;;
    *)       echo 0 ;;
  esac
}

# Stub chown and sudo so tests run as non-root.
chown() { :; }

# Capture WeeChat invocations for inspection.
weechat_bin_path="$MT_FIXTURE/fake-weechat"
cat >"$weechat_bin_path" <<'WEECHAT_STUB'
#!/usr/bin/env bash
set -euo pipefail
echo "$@" >> "${WEECHAT_CALL_DIR:-/dev/null}/weechat_calls.log"
# Parse --dir <path> and find port from /relay add api <port> in run-command strings
weechat_home=""
relay_port=""
i=0
args=("$@")
while [[ $i -lt ${#args[@]} ]]; do
  if [[ "${args[$i]}" == "--dir" ]]; then
    weechat_home="${args[$((i+1))]}"
  fi
  # --run-command '/relay add api 18000' contains the port
  if [[ "${args[$i]}" == *"/relay add api "* ]]; then
    relay_port="${args[$i]##*/relay add api }"
    relay_port="${relay_port%%\'*}"
  fi
  i=$((i+1))
done
mkdir -p "$weechat_home"
cat >"$weechat_home/relay.conf" <<REOF
[api]
api = $relay_port

[network]
password = "\${env:RELAY_PASSWORD}"
allow_empty_password = off
bind_address = "127.0.0.1"
REOF
touch "$weechat_home/weechat.conf"
touch "$weechat_home/sec.conf"
WEECHAT_STUB
chmod +x "$weechat_bin_path"

# Stub command -v to return our fake weechat
weechat() { "$weechat_bin_path" "$@"; }

# Fixture password
FIXTURE_PASSWORD="test-relay-secret-12345"

write_fixture_env() {  # <tenant>
  local env_dir
  env_dir="$(tenant_env_dir "$1")"
  mkdir -p "$env_dir"
  cat >"$env_dir/lunarwing.env" <<ENV
RELAY_PASSWORD=$FIXTURE_PASSWORD
ENV
}

# ── Tests: pure helpers ──────────────────────────────────────────────────────
echo "=== tenant_weechat_home tests ==="

out="$(tenant_weechat_home fixture-tenant)"
assert_eq "returns .config/weechat under tenant home" \
  "$out" "$MT_FIXTURE/fixture-tenant/.config/weechat"

echo "=== _read_env_value tests ==="

env_test="$MT_FIXTURE/test-env"
mkdir -p "$env_test"
cat >"$env_test/test.env" <<ENV
FIRST=value1
RELAY_PASSWORD=secret-pass
LAST_VALUE=final
RELAY_PASSWORD=overridden
ENV

out="$(_read_env_value "$env_test/test.env" "RELAY_PASSWORD")"
assert_eq "reads last value for duplicate keys" "$out" "overridden"

out="$(_read_env_value "$env_test/test.env" "FIRST")"
assert_eq "reads first key" "$out" "value1"

out="$(_read_env_value "$env_test/test.env" "NONEXISTENT")"
assert_eq "missing key returns empty" "$out" ""

out="$(_read_env_value "$env_test/nonexistent.env" "ANY")"
assert_eq "missing file returns empty" "$out" ""

echo "=== _weechat_config_dir_has_entries tests ==="

empty_dir="$MT_FIXTURE/empty-config"
mkdir -p "$empty_dir"
assert_fail "empty dir has no entries" _weechat_config_dir_has_entries "$empty_dir"

# A directory with just a dotfile
dotfile_dir="$MT_FIXTURE/dotfile-config"
mkdir -p "$dotfile_dir"
touch "$dotfile_dir/.weechat.conf"
assert_ok "dir with dotfile has entries" _weechat_config_dir_has_entries "$dotfile_dir"

# A directory with a normal file
file_dir="$MT_FIXTURE/file-config"
mkdir -p "$file_dir"
touch "$file_dir/relay.conf"
assert_ok "dir with normal file has entries" _weechat_config_dir_has_entries "$file_dir"

echo "=== _weechat_validate_relay_config tests ==="

valid_config_dir="$MT_FIXTURE/valid-config"
mkdir -p "$valid_config_dir"
cat >"$valid_config_dir/relay.conf" <<CONF
[api]
api = 18000

[network]
password = "\${env:RELAY_PASSWORD}"
allow_empty_password = off
bind_address = "127.0.0.1"
CONF

assert_ok "valid config accepted" \
  _weechat_validate_relay_config "$valid_config_dir" 18000 "$FIXTURE_PASSWORD"

# Wrong port
wrong_port_dir="$MT_FIXTURE/wrong-port"
mkdir -p "$wrong_port_dir"
cat >"$wrong_port_dir/relay.conf" <<CONF
[api]
api = 19999

[network]
password = "\${env:RELAY_PASSWORD}"
allow_empty_password = off
bind_address = "127.0.0.1"
CONF
assert_fail "wrong port rejected" \
  _weechat_validate_relay_config "$wrong_port_dir" 18000 "$FIXTURE_PASSWORD"

# Plaintext password leak (contains the actual password)
leak_dir="$MT_FIXTURE/leak-config"
mkdir -p "$leak_dir"
cat >"$leak_dir/relay.conf" <<CONF
[api]
api = 18000

[network]
password = "$FIXTURE_PASSWORD"
allow_empty_password = off
bind_address = "127.0.0.1"
CONF
assert_fail "plaintext password leak rejected" \
  _weechat_validate_relay_config "$leak_dir" 18000 "$FIXTURE_PASSWORD"

# Non-loopback bind
nonloop_dir="$MT_FIXTURE/nonloop-config"
mkdir -p "$nonloop_dir"
cat >"$nonloop_dir/relay.conf" <<CONF
[api]
api = 18000

[network]
password = "\${env:RELAY_PASSWORD}"
allow_empty_password = off
bind_address = "0.0.0.0"
CONF
assert_fail "non-loopback bind rejected" \
  _weechat_validate_relay_config "$nonloop_dir" 18000 "$FIXTURE_PASSWORD"

# Missing relay.conf
no_relay_dir="$MT_FIXTURE/no-relay"
mkdir -p "$no_relay_dir"
assert_fail "missing relay.conf rejected" \
  _weechat_validate_relay_config "$no_relay_dir" 18000 "$FIXTURE_PASSWORD"

# Missing [api] section
no_api_dir="$MT_FIXTURE/no-api"
mkdir -p "$no_api_dir"
cat >"$no_api_dir/relay.conf" <<CONF
[network]
password = "\${env:RELAY_PASSWORD}"
allow_empty_password = off
bind_address = "127.0.0.1"
CONF
assert_fail "missing [api] rejected" \
  _weechat_validate_relay_config "$no_api_dir" 18000 "$FIXTURE_PASSWORD"

# Missing env expression
no_env_dir="$MT_FIXTURE/no-env"
mkdir -p "$no_env_dir"
cat >"$no_env_dir/relay.conf" <<CONF
[api]
api = 18000

[network]
password = "some-other-value"
allow_empty_password = off
bind_address = "127.0.0.1"
CONF
assert_fail "missing env expression rejected" \
  _weechat_validate_relay_config "$no_env_dir" 18000 "$FIXTURE_PASSWORD"

# Missing allow_empty_password=off
no_empty_off_dir="$MT_FIXTURE/no-empty-off"
mkdir -p "$no_empty_off_dir"
cat >"$no_empty_off_dir/relay.conf" <<CONF
[api]
api = 18000

[network]
password = "\${env:RELAY_PASSWORD}"
bind_address = "127.0.0.1"
CONF
assert_fail "missing allow_empty_password=off rejected" \
  _weechat_validate_relay_config "$no_empty_off_dir" 18000 "$FIXTURE_PASSWORD"

echo "=== _weechat_generate_relay_config tests ==="

# Set up call logging
export WEECHAT_CALL_DIR="$MT_FIXTURE/weechat-calls"
mkdir -p "$WEECHAT_CALL_DIR"

gen_temp="$MT_FIXTURE/gen-temp"
mkdir -p "$gen_temp"
write_fixture_env "gen-test"

# Stub sudo to call our fake weechat directly, preserving RELAY_PASSWORD env
sudo() {
  while [[ "$1" == --preserve-env=* ]]; do
    shift
  done
  if [[ "$1" == "-u" ]]; then
    shift 2  # skip -u <user>
  fi
  "$@"
}

_weechat_generate_relay_config "gen-test" "$gen_temp" 18000 "$(tenant_env_dir gen-test)/lunarwing.env"

# Verify WeeChat was called with the exact required arguments
call_log="$WEECHAT_CALL_DIR/weechat_calls.log"
assert_ok "call log exists" test -f "$call_log"

# Check that the escaped env expression is in the call args
if grep -q '${env:RELAY_PASSWORD}' "$call_log" && ! grep -q "${FIXTURE_PASSWORD}" "$call_log"; then
  echo "  PASS: call args contain escaped \${env:RELAY_PASSWORD}, not plaintext"
else
  echo "  FAIL: call args leak password or miss escaped expression"
  failures=$((failures + 1))
fi

# Check /relay add api (not addreplace, not weechat protocol)
if grep -q '/relay add api ' "$call_log" && ! grep -q '/relay addreplace' "$call_log"; then
  echo "  PASS: uses /relay add api, not addreplace"
else
  echo "  FAIL: wrong relay command"
  failures=$((failures + 1))
fi

# Check bind_address = 127.0.0.1
if grep -q '127.0.0.1' "$call_log"; then
  echo "  PASS: bind_address 127.0.0.1 in call args"
else
  echo "  FAIL: missing bind_address"
  failures=$((failures + 1))
fi

# Check allow_empty_password off
if grep -q 'allow_empty_password off' "$call_log"; then
  echo "  PASS: allow_empty_password off in call args"
else
  echo "  FAIL: missing allow_empty_password off"
  failures=$((failures + 1))
fi

# Check /save and /quit
if grep -q '/save' "$call_log" && grep -q '/quit' "$call_log"; then
  echo "  PASS: /save and /quit present"
else
  echo "  FAIL: missing /save or /quit"
  failures=$((failures + 1))
fi

# Generated config is valid
assert_ok "generated config is valid" \
  _weechat_validate_relay_config "$gen_temp" 18000 "$FIXTURE_PASSWORD"

missing_bin_marker="$MT_FIXTURE/missing-weechat-returned"
(
  command() {
    if [[ "${1:-}" == "-v" && "${2:-}" == "weechat" ]]; then
      return 1
    fi
    builtin command "$@"
  }
  set +e
  _weechat_generate_relay_config "gen-test" "$MT_FIXTURE/no-weechat-output" 18000 "$(tenant_env_dir gen-test)/lunarwing.env" >/dev/null 2>&1
  printf '%s\n' "$?" >"$missing_bin_marker"
  exit 0
) || true
assert_ok "missing WeeChat returns instead of exiting the caller" test -f "$missing_bin_marker"
assert_eq "missing WeeChat return code" "$(<"$missing_bin_marker")" "1"

echo "=== configure_weechat_relay tests ==="

# Happy path: empty/absent target succeeds
happy_tenant="cw-happy"
mkdir -p "$(tenant_home "$happy_tenant")/.config"
write_fixture_env "$happy_tenant"

# Reset call log
rm -rf "$WEECHAT_CALL_DIR"
mkdir -p "$WEECHAT_CALL_DIR"

configure_weechat_relay "$happy_tenant" >/dev/null

target_dir="$(tenant_weechat_home "$happy_tenant")"
assert_ok "target dir created" test -d "$target_dir"
assert_ok "relay.conf exists" test -f "$target_dir/relay.conf"
assert_ok "promoted config is valid" \
  _weechat_validate_relay_config "$target_dir" 18000 "$FIXTURE_PASSWORD"

# Explicit recovery must also prepare the least-privilege runtime credential.
happy_weechat_env="$(tenant_env_dir "$happy_tenant")/weechat.env"
assert_ok "explicit recovery writes weechat.env" test -f "$happy_weechat_env"
assert_eq "explicit recovery writes only RELAY_PASSWORD" \
  "$(<"$happy_weechat_env")" "RELAY_PASSWORD=$FIXTURE_PASSWORD"

# Plaintext password must not be in the config file
if ! grep -qr "$FIXTURE_PASSWORD" "$target_dir"; then
  echo "  PASS: no plaintext password in promoted dir"
else
  echo "  FAIL: plaintext password found in promoted dir"
  failures=$((failures + 1))
fi

# Temp dir cleaned up (no .weechat-tmp or similar under .config)
temp_dirs="$(find "$(tenant_home "$happy_tenant")/.config" -maxdepth 1 -name '*tmp*' -o -name '*temp*' 2>/dev/null | wc -l)"
assert_eq "temp dirs cleaned" "$temp_dirs" "0"

# Failure path: non-empty target is rejected, unchanged
conflict_tenant="cw-conflict"
mkdir -p "$(tenant_weechat_home "$conflict_tenant")"
echo "existing-config-content" > "$(tenant_weechat_home "$conflict_tenant")/relay.conf"
# Snapshot the dir for byte-for-byte comparison
conflict_snapshot="$MT_FIXTURE/conflict-snapshot"
cp -a "$(tenant_weechat_home "$conflict_tenant")" "$conflict_snapshot"

write_fixture_env "$conflict_tenant"
printf 'RELAY_PASSWORD=preserve-existing-value\n' \
  >"$(tenant_env_dir "$conflict_tenant")/weechat.env"
conflict_env_before="$(<"$(tenant_env_dir "$conflict_tenant")/weechat.env")"

rm -rf "$WEECHAT_CALL_DIR"
mkdir -p "$WEECHAT_CALL_DIR"

# Run in subshell so die()'s exit 1 doesn't kill the test script
if ( configure_weechat_relay "$conflict_tenant" ) >/dev/null 2>&1; then
  echo "  FAIL: non-empty target should fail"
  failures=$((failures + 1))
else
  echo "  PASS: non-empty target fails"
fi

# Verify target unchanged (byte-for-byte)
if diff -rq "$conflict_snapshot" "$(tenant_weechat_home "$conflict_tenant")" >/dev/null 2>&1; then
  echo "  PASS: conflict target unchanged"
else
  echo "  FAIL: conflict target was modified"
  failures=$((failures + 1))
fi

assert_eq "conflict preserves existing weechat.env" \
  "$(<"$(tenant_env_dir "$conflict_tenant")/weechat.env")" "$conflict_env_before"

# WeeChat was NOT called for the conflict case
if [[ ! -s "$WEECHAT_CALL_DIR/weechat_calls.log" ]]; then
  echo "  PASS: WeeChat not invoked on conflict"
else
  echo "  FAIL: WeeChat invoked on conflict"
  failures=$((failures + 1))
fi

# Regression: preserved non-empty legacy config without weechat.env
# must still create weechat.env — systemd/OpenRC rendered units require it.
legacy_tenant="cw-legacy-no-env"
legacy_weechat_home="$(tenant_weechat_home "$legacy_tenant")"
mkdir -p "$legacy_weechat_home"
# Sentinel content that must survive byte-for-byte.
printf 'legacy-sentinel-config\n' >"$legacy_weechat_home/weechat.conf"
printf 'legacy-relay-sentinel\n' >"$legacy_weechat_home/relay.conf"
# Snapshot for byte-for-byte comparison.
legacy_snapshot="$MT_FIXTURE/legacy-snapshot"
cp -a "$legacy_weechat_home" "$legacy_snapshot"

write_fixture_env "$legacy_tenant"
# Explicitly ensure no weechat.env exists before the call.
legacy_weechat_env="$(tenant_env_dir "$legacy_tenant")/weechat.env"
rm -f "$legacy_weechat_env"

rm -rf "$WEECHAT_CALL_DIR"
mkdir -p "$WEECHAT_CALL_DIR"

# Run in subshell so die()'s exit 1 doesn't kill the test script.
if ( configure_weechat_relay "$legacy_tenant" ) >/dev/null 2>&1; then
  echo "  FAIL: legacy config without weechat.env should fail"
  failures=$((failures + 1))
else
  echo "  PASS: legacy config without weechat.env fails"
fi

# Verify target config unchanged (byte-for-byte).
if diff -rq "$legacy_snapshot" "$(tenant_weechat_home "$legacy_tenant")" >/dev/null 2>&1; then
  echo "  PASS: legacy config preserved unchanged"
else
  echo "  FAIL: legacy config was modified"
  failures=$((failures + 1))
fi

# The critical regression: weechat.env must be created even though
# configure returned nonzero, because systemd/OpenRC rendered units
# require it at service startup.
assert_ok "legacy path creates weechat.env" test -f "$legacy_weechat_env"
legacy_env_content=""
if [[ -f "$legacy_weechat_env" ]]; then
  legacy_env_content="$(<"$legacy_weechat_env")"
fi
assert_eq "legacy path weechat.env has RELAY_PASSWORD" \
  "$legacy_env_content" "RELAY_PASSWORD=$FIXTURE_PASSWORD"

# Failure path: missing RELAY_PASSWORD in env
no_pass_tenant="cw-nopass"
mkdir -p "$(tenant_home "$no_pass_tenant")/.config"
mkdir -p "$(tenant_env_dir "$no_pass_tenant")"
echo "SOME_OTHER_VAR=value" > "$(tenant_env_dir "$no_pass_tenant")/lunarwing.env"

if ( configure_weechat_relay "$no_pass_tenant" ) >/dev/null 2>&1; then
  echo "  FAIL: missing password should fail"
  failures=$((failures + 1))
else
  echo "  PASS: missing password fails"
fi

# Target should NOT exist (generation never started)
assert_fail "target not created on missing password" \
  test -d "$(tenant_weechat_home "$no_pass_tenant")"

return_marker="$MT_FIXTURE/configure-returned"
(
  set +e
  configure_weechat_relay "$no_pass_tenant" >/dev/null 2>&1
  printf '%s\n' "$?" >"$return_marker"
  exit 0
) || true
assert_ok "configuration errors return instead of exiting the caller" test -f "$return_marker"
assert_eq "configuration error return code" "$(<"$return_marker")" "1"

# Failure path: WeeChat command failure (fake-weechat returns error)
fail_bin="$MT_FIXTURE/fail-weechat"
cat >"$fail_bin" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF
chmod +x "$fail_bin"

# Override weechat stub to use the failing binary for this test
weechat_bin_path_backup="$weechat_bin_path"
weechat_bin_path="$fail_bin"

fail_tenant="cw-genfail"
mkdir -p "$(tenant_home "$fail_tenant")/.config"
write_fixture_env "$fail_tenant"

if ( configure_weechat_relay "$fail_tenant" ) >/dev/null 2>&1; then
  echo "  FAIL: generation failure should cause configure failure"
  failures=$((failures + 1))
else
  echo "  PASS: generation failure causes configure failure"
fi

# Target should NOT exist
assert_fail "target not created on generation failure" \
  test -d "$(tenant_weechat_home "$fail_tenant")"

# No temp dirs left behind
leftover_temps="$(find "$(tenant_home "$fail_tenant")/.config" -maxdepth 1 -name '*tmp*' -o -name '*temp*' 2>/dev/null | wc -l)"
assert_eq "no temp dirs on gen failure" "$leftover_temps" "0"

# Restore working stub
weechat_bin_path="$weechat_bin_path_backup"

# Failure path: validation failure (weechat writes invalid config)
badweechat_bin="$MT_FIXTURE/bad-weechat"
cat >"$badweechat_bin" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
# Write a config missing the [api] section
for arg in "$@"; do :; done
prev=""
weechat_home=""
for arg in "$@"; do
  if [[ "$prev" == "--dir" ]]; then
    weechat_home="$arg"
  fi
  prev="$arg"
done
mkdir -p "$weechat_home"
cat >"$weechat_home/relay.conf" <<REOF
[network]
password = "\${env:RELAY_PASSWORD}"
REOF
EOF
chmod +x "$badweechat_bin"
weechat_bin_path_backup2="$weechat_bin_path"
weechat_bin_path="$badweechat_bin"

badval_tenant="cw-badval"
mkdir -p "$(tenant_home "$badval_tenant")/.config"
write_fixture_env "$badval_tenant"

if ( configure_weechat_relay "$badval_tenant" ) >/dev/null 2>&1; then
  echo "  FAIL: invalid generated config should fail"
  failures=$((failures + 1))
else
  echo "  PASS: invalid generated config fails"
fi

# Target should NOT exist
assert_fail "target not created on validation failure" \
  test -d "$(tenant_weechat_home "$badval_tenant")"

# Restore working stub
weechat_bin_path="$weechat_bin_path_backup2"

# Empty target dir (exists but empty) succeeds
empty_tenant="cw-emptytarget"
mkdir -p "$(tenant_weechat_home "$empty_tenant")"
# Ensure it's truly empty
rm -rf "$(tenant_weechat_home "$empty_tenant")"
mkdir -p "$(tenant_weechat_home "$empty_tenant")"
write_fixture_env "$empty_tenant"

rm -rf "$WEECHAT_CALL_DIR"
mkdir -p "$WEECHAT_CALL_DIR"

if ( configure_weechat_relay "$empty_tenant" ) >/dev/null 2>&1; then
  echo "  PASS: empty existing target succeeds"
else
  echo "  FAIL: empty existing target should succeed"
  failures=$((failures + 1))
fi

assert_ok "promoted config valid for empty target" \
  _weechat_validate_relay_config "$(tenant_weechat_home "$empty_tenant")" 18000 "$FIXTURE_PASSWORD"

unique_tenant="cw-unique-temp"
unique_parent="$(tenant_home "$unique_tenant")/.config"
mkdir -p "$unique_parent/.weechat-bootstrap-tmp.$$"
printf 'preserve-sentinel\n' >"$unique_parent/.weechat-bootstrap-tmp.$$/sentinel"
write_fixture_env "$unique_tenant"
configure_weechat_relay "$unique_tenant" >/dev/null
assert_eq "bootstrap uses a unique temp directory" \
  "$(<"$unique_parent/.weechat-bootstrap-tmp.$$/sentinel")" "preserve-sentinel"

race_tenant="cw-promotion-race"
race_target="$(tenant_weechat_home "$race_tenant")"
mkdir -p "$(dirname "$race_target")"
write_fixture_env "$race_tenant"
mv() {
  local arg
  for arg in "$@"; do
    case "$arg" in
      *weechat-bootstrap*)
        mkdir -p "$race_target"
        printf 'preserve-race-winner\n' >"$race_target/sentinel"
        break
        ;;
    esac
  done
  command mv "$@"
}
if configure_weechat_relay "$race_tenant" >/dev/null 2>&1; then
  echo "  FAIL: promotion race should fail without nesting generated config"
  failures=$((failures + 1))
else
  echo "  PASS: promotion race fails safely"
fi
assert_eq "promotion race preserves competing target" \
  "$(<"$race_target/sentinel")" "preserve-race-winner"
unset -f mv

# ── Todo 3: CLI dispatch and add_tenant wiring ───────────────────────────────
echo "=== CLI dispatch smoke check ==="

# Verify --help mentions configure-weechat-relay
help_output="$(bash "$ADMIN_SCRIPT" --help 2>&1)"
if echo "$help_output" | grep -q 'configure-weechat-relay'; then
  echo "  PASS: --help lists configure-weechat-relay"
else
  echo "  FAIL: --help missing configure-weechat-relay"
  failures=$((failures + 1))
fi

# Missing argument should produce a usage error (before require_root)
err="$(bash "$ADMIN_SCRIPT" configure-weechat-relay 2>&1 || true)"
if echo "$err" | grep -qi 'usage'; then
  echo "  PASS: missing argument produces usage error"
else
  echo "  FAIL: missing argument did not produce usage error"
  echo "        $err"
  failures=$((failures + 1))
fi

# Extra argument should produce an error
err2="$(bash "$ADMIN_SCRIPT" configure-weechat-relay tenant1 extra 2>&1 || true)"
if echo "$err2" | grep -qi 'usage\|unexpected\|error'; then
  echo "  PASS: extra argument produces error"
else
  echo "  FAIL: extra argument accepted"
  echo "        $err2"
  failures=$((failures + 1))
fi

# Unknown tenant: requires root, so we test the not-found path differently.
# With PORTS_REGISTRY set to a fixture, the script should reach registry check
# after passing require_root (which fails as non-root). We verify the dispatch
# recognizes the command (not "unknown command").
err3="$(PORTS_REGISTRY="$MT_FIXTURE/ports-fixture.json" bash "$ADMIN_SCRIPT" configure-weechat-relay ghost-tenant 2>&1 || true)"
if echo "$err3" | grep -qi 'not found\|must run as root'; then
  echo "  PASS: unknown tenant handled (reached dispatch, not unknown command)"
else
  echo "  FAIL: unknown tenant did not produce expected error"
  echo "        $err3"
  failures=$((failures + 1))
fi

echo "=== add_tenant non-fatal auto-bootstrap ==="

# Stub the heavy add_tenant operations to test ordering and non-fatal behavior.
# We test that configure_weechat_relay is called after env creation and before
# postgres startup, and that its failure is non-fatal.

call_order_log="$MT_FIXTURE/call_order.log"
: >"$call_order_log"

# Override stubs to track execution order
start_tenant_postgres() { echo "postgres" >>"$call_order_log"; }
clone_tenant_repo()    { :; }
create_tenant_user()   { :; }
warn_if_adapter_deps_missing() { :; }
write_tenant_vision_env() { :; }
write_tenant_lunarwing_env() { echo "env-created" >>"$call_order_log"; }
write_tenant_bridge_env() { :; }
write_tenant_gotify_config() { :; }
ensure_external_worker_config() { :; }
ensure_container_runtime() { :; }
build_vision_sidecar_image() { :; }
ensure_init_system() { :; }
ensure_health_pipeline() { :; }
_read_env_value() { echo "fixture-pass"; }
_write_weechat_env() { echo "weechat-env" >>"$call_order_log"; }

# Stub render functions
render_tenant_systemd_units() { :; }
render_tenant_openrc_units() { :; }

# Override ports functions to avoid touching real registry
ports_registry_init() { :; }
ports_allocate() { echo "10000"; }

# Track configure_weechat_relay calls
_weechat_bootstrap_called=false
original_configure_weechat_relay() { configure_weechat_relay "$@"; }

# Stub: fail on first call to test non-fatal behavior
bootstrap_fail_mode="${BOOTSTRAP_FAIL_MODE:-}"
configure_weechat_relay() {
  echo "weechat-bootstrap" >>"$call_order_log"
  _weechat_bootstrap_called=true
  if [[ "$bootstrap_fail_mode" == "fail" ]]; then
    return 1
  fi
  # Simulate success (don't actually run the real function)
  return 0
}

# Also stub sanitize_name to just pass through for this test
sanitize_name() { echo "$1"; }

# Stub SSH provisioning
ensure_ssh_config() { :; }
provision_tenant_ssh_key() { :; }
warn_if_sshd_unreachable() { :; }

# Run add_tenant with success-mode bootstrap
: >"$call_order_log"
DEFAULT_SSH_ENABLED="false" SSH_OPT_OUT="true" CONTAINER_RT="podman" \
  VISION_SIDECAR_IMAGE="dummy" INIT_SYSTEM="systemd" \
  DEFAULT_HEALTH_ENABLED="false" HEALTH_OPT_OUT="true" \
  add_tenant "auto-boot-tenant" "" "" "" "" "" "" "" "" "" "" "" "" "" "" "" "" "" "" "" \
  >/dev/null 2>&1 || true

# Verify ordering: env-created before weechat-bootstrap before postgres
order_result="$(cat "$call_order_log")"
env_line=$(echo "$order_result" | grep -n 'env-created' | cut -d: -f1 || echo 0)
weechat_line=$(echo "$order_result" | grep -n 'weechat-bootstrap' | cut -d: -f1 || echo 0)
pg_line=$(echo "$order_result" | grep -n 'postgres' | cut -d: -f1 || echo 0)

if [[ "$env_line" -gt 0 && "$weechat_line" -gt "$env_line" ]]; then
  echo "  PASS: weechat bootstrap called after env creation"
else
  echo "  FAIL: weechat bootstrap not called after env creation"
  echo "        order: $order_result"
  failures=$((failures + 1))
fi

if [[ "$pg_line" -gt 0 && "$weechat_line" -gt 0 && "$pg_line" -gt "$weechat_line" ]]; then
  echo "  PASS: weechat bootstrap called before postgres"
else
  echo "  FAIL: postgres not called after weechat bootstrap"
  echo "        order: $order_result"
  failures=$((failures + 1))
fi

# Test non-fatal failure: bootstrap fails but add_tenant continues
: >"$call_order_log"
bootstrap_fail_mode="fail"
DEFAULT_SSH_ENABLED="false" SSH_OPT_OUT="true" CONTAINER_RT="podman" \
  VISION_SIDECAR_IMAGE="dummy" INIT_SYSTEM="systemd" \
  DEFAULT_HEALTH_ENABLED="false" HEALTH_OPT_OUT="true" \
  add_tenant "auto-boot-fail" "" "" "" "" "" "" "" "" "" "" "" "" "" "" "" "" "" "" "" \
  >/dev/null 2>&1 || true

# postgres should still be called (base provisioning continues)
if grep -q 'postgres' "$call_order_log"; then
  echo "  PASS: base provisioning continues after bootstrap failure"
else
  echo "  FAIL: base provisioning aborted on bootstrap failure"
  echo "        order: $(cat "$call_order_log")"
  failures=$((failures + 1))
fi

echo "=== opt-out (--no-weechat-bootstrap) behavior ==="

# Reset call log, set the opt-out global, and verify:
#   - configure_weechat_relay is NOT invoked
#   - _write_weechat_env IS still invoked (minimal env)
#   - base provisioning continues (postgres called)
#   - summary output shows explicit disabled message
: >"$call_order_log"
bootstrap_fail_mode=""
WEECHAT_BOOTSTRAP_OPT_OUT=true
DEFAULT_SSH_ENABLED="false" SSH_OPT_OUT="true" CONTAINER_RT="podman" \
  VISION_SIDECAR_IMAGE="dummy" INIT_SYSTEM="systemd" \
  DEFAULT_HEALTH_ENABLED="false" HEALTH_OPT_OUT="true" \
  add_tenant "auto-boot-disabled" "" "" "" "" "" "" "" "" "" "" "" "" "" "" "" "" "" "" "" \
  >"$MT_FIXTURE/disabled-output" 2>&1 || true

assert_fail "opt-out does not invoke WeeChat bootstrap" \
  grep -q '^weechat-bootstrap$' "$call_order_log"
assert_ok "opt-out still writes minimal WeeChat env" \
  grep -q '^weechat-env$' "$call_order_log"
assert_ok "opt-out continues base provisioning" \
  grep -q '^postgres$' "$call_order_log"
assert_ok "opt-out summary is explicit" \
  grep -qF 'weechat relay:    disabled (--no-weechat-bootstrap)' "$MT_FIXTURE/disabled-output"
# shellcheck disable=SC2034 # reset global for subsequent tests
WEECHAT_BOOTSTRAP_OPT_OUT=false

echo "=== opt-in live WeeChat smoke ==="
if [[ "${RUN_LIVE_WEECHAT:-0}" == "1" ]]; then
  live_weechat_bin="$(type -P weechat || true)"
  if [[ -x "$live_weechat_bin" ]]; then
    mkdir -p /tmp/opencode
    smoke_dir="$(mktemp -d -p /tmp/opencode weechat-relay-smoke.XXXXXX)"
    smoke_output="$MT_FIXTURE/live-weechat.out"
    smoke_password="live-smoke-secret-not-a-tenant-credential"
    smoke_port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
    if (
      trap 'rm -rf "$smoke_dir"' EXIT
      RELAY_PASSWORD="$smoke_password" TERM=xterm "$live_weechat_bin" --dir "$smoke_dir" \
        --run-command '/set relay.network.password "\${env:RELAY_PASSWORD}"' \
        --run-command '/set relay.network.allow_empty_password off' \
        --run-command '/set relay.network.bind_address "127.0.0.1"' \
        --run-command "/relay add api ${smoke_port}" \
        --run-command '/save' \
        --run-command '/quit' >"$smoke_output" 2>&1
      _weechat_validate_relay_config "$smoke_dir" "$smoke_port" "$smoke_password"
    ); then
      echo "  PASS: live WeeChat generated a valid relay config"
    else
      echo "  FAIL: live WeeChat smoke failed"
      failures=$((failures + 1))
    fi
    assert_fail "live smoke temp directory cleaned" test -e "$smoke_dir"
    if [[ -f "$smoke_output" ]] && grep -qF "$smoke_password" "$smoke_output"; then
      echo "  FAIL: live WeeChat output exposed the fixture password"
      failures=$((failures + 1))
    else
      echo "  PASS: live WeeChat output did not expose the fixture password"
    fi
  else
    echo "  SKIP: weechat is not installed"
  fi
else
  echo "  SKIP: set RUN_LIVE_WEECHAT=1 to enable"
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
