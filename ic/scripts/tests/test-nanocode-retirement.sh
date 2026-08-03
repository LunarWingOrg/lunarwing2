#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

failures=0
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }

assert_eq() {
  local label="$1" actual="$2" expected="$3"
  if [[ "$actual" == "$expected" ]]; then pass "$label"; else
    fail "$label (expected '$expected', got '$actual')"
  fi
}

tenant_state_dir() { printf '%s/state\n' "$fixture"; }
tenant_env_dir() { printf '%s/env\n' "$fixture"; }
say() { :; }
chown() { :; }
mkdir -p "$fixture/state" "$fixture/env"

config="$fixture/state/config.toml"
cat >"$config" <<'TOML'
[sandbox]
enabled = true

# External worker: nanocode - legacy route
[[sandbox.external_workers]]
name = "nanocode"
url = "ws://127.0.0.1:10007/ws/agent"
[sandbox.external_workers.metadata]
kind = "legacy"

[[sandbox.external_workers]]
name = "opencode"
url = "ws://127.0.0.1:20007/ws/agent"

[unrelated]
preserved = true
TOML

if remove_external_worker_config fixture nanocode; then
  if python3 - "$config" <<'PY'
import sys
import tomllib

with open(sys.argv[1], "rb") as handle:
    config = tomllib.load(handle)
workers = config["sandbox"]["external_workers"]
assert [worker["name"] for worker in workers] == ["opencode"]
assert config["unrelated"]["preserved"] is True
PY
  then
    pass "Nanocode TOML block and nested tables removed"
  else
    fail "retired config is valid and preserves unrelated data"
  fi
else
  fail "valid Nanocode TOML block can be retired"
fi

backup="$config.pre-nanocode-retirement"
if [[ -f "$backup" ]] && grep -q 'name = "nanocode"' "$backup"; then
  pass "original config retained as rollback copy"
else
  fail "original config rollback copy missing"
fi
assert_eq "config rollback copy is private" "$(stat -c '%a' "$backup")" "600"

malformed="$fixture/state/bad/config.toml"
mkdir -p "$(dirname "$malformed")"
printf '[[sandbox.external_workers]\nname = "nanocode"\n' >"$malformed"
tenant_state_dir() { printf '%s/state/bad\n' "$fixture"; }
before="$(<"$malformed")"
if remove_external_worker_config fixture nanocode >/dev/null 2>&1; then
  fail "malformed TOML was accepted"
else
  pass "malformed TOML blocks retirement"
fi
assert_eq "malformed config remains unchanged" "$(<"$malformed")" "$before"
[[ ! -e "$malformed.pre-nanocode-retirement" ]] \
  && pass "malformed config creates no rollback copy" \
  || fail "malformed config created misleading rollback copy"

unsafe_state="$fixture/state/unsafe"
mkdir -p "$unsafe_state"
cat >"$unsafe_state/config.toml" <<'TOML'
[[sandbox.external_workers]]
name = "nanocode"
url = "ws://127.0.0.1:10007/ws/agent"
TOML
printf 'do-not-touch\n' >"$fixture/config-victim"
ln -s "$fixture/config-victim" "$unsafe_state/config.toml.pre-nanocode-retirement"
tenant_state_dir() { printf '%s/state/unsafe\n' "$fixture"; }
if remove_external_worker_config fixture nanocode >/dev/null 2>&1; then
  fail "symlinked rollback path was accepted"
else
  pass "symlinked rollback path blocks config retirement"
fi
assert_eq "symlink target remains unchanged" "$(<"$fixture/config-victim")" "do-not-touch"
grep -q 'name = "nanocode"' "$unsafe_state/config.toml" \
  && pass "unsafe backup leaves live config unchanged" \
  || fail "unsafe backup mutated live config"

tenant_state_dir() { printf '%s/state\n' "$fixture"; }

mkdir -p "$fixture/env-unsafe"
printf 'NANOCODE_MODEL=do-not-touch\n' >"$fixture/env-victim"
ln -s "$fixture/env-victim" "$fixture/env-unsafe/lunarwing.env"
tenant_env_dir() { printf '%s/env-unsafe\n' "$fixture"; }
if _archive_nanocode_env fixture >/dev/null 2>&1; then
  fail "symlinked tenant env was accepted"
else
  pass "symlinked tenant env blocks retirement"
fi
assert_eq "tenant env symlink target remains unchanged" \
  "$(<"$fixture/env-victim")" "NANOCODE_MODEL=do-not-touch"

tenant_env_dir() { printf '%s/env\n' "$fixture"; }
cat >"$fixture/env/lunarwing.env" <<'ENV'
OPENCODE_MODEL=preserved
NANOCODE_MODEL=retired
NANOCODE_BASE_URL=http://legacy.invalid/v1
ENV
printf 'NANOCODE_MODE=websocket\n' >"$fixture/env/nanocode.env"
chmod 0644 "$fixture/env/lunarwing.env" "$fixture/env/nanocode.env"
if _archive_nanocode_env "$(id -un)"; then
  pass "Nanocode environment retired"
else
  fail "Nanocode environment retirement failed"
fi
if grep -q '^NANOCODE_' "$fixture/env/lunarwing.env"; then
  fail "live environment retains Nanocode keys"
else
  pass "live environment drops Nanocode keys"
fi
grep -q '^OPENCODE_MODEL=preserved$' "$fixture/env/lunarwing.env" \
  && pass "live environment preserves supported worker keys" \
  || fail "live environment lost supported worker keys"
[[ -f "$fixture/env/nanocode.env.retired" && ! -e "$fixture/env/nanocode.env" ]] \
  && pass "legacy worker env archived" \
  || fail "legacy worker env was not archived"
assert_eq "live environment hardened" "$(stat -c '%a' "$fixture/env/lunarwing.env")" "600"
assert_eq "environment rollback copy private" \
  "$(stat -c '%a' "$fixture/env/lunarwing.env.pre-nanocode-retirement")" "600"

PORTS_REGISTRY="$fixture/ports.json"
cat >"$PORTS_REGISTRY" <<'JSON'
{"tenants":{"fixture":{"workers":{"nanocode":true,"opencode":true},"ports":{"nanocode_wss":10007},"extended_ports":{"nanocode_health":20003}}}}
JSON
acquire_ports_lock() { :; }
release_ports_lock() { :; }
# Function comes from sourced mt-admin later in ShellCheck's composed parse.
# shellcheck disable=SC2218
ports_retire_nanocode fixture
assert_eq "registry marks Nanocode retired" \
  "$(jq -r '.tenants.fixture.workers.nanocode' "$PORTS_REGISTRY")" "false"
assert_eq "registry preserves Nanocode WSS slot" \
  "$(jq -r '.tenants.fixture.ports.nanocode_wss' "$PORTS_REGISTRY")" "10007"
assert_eq "registry preserves Nanocode health slot" \
  "$(jq -r '.tenants.fixture.extended_ports.nanocode_health' "$PORTS_REGISTRY")" "20003"
assert_eq "registry preserves supported workers" \
  "$(jq -r '.tenants.fixture.workers.opencode' "$PORTS_REGISTRY")" "true"

CALLS=()
_stop_nanocode_supervision() { CALLS+=(stop-supervision); }
_stop_nanocode_containers() { CALLS+=(stop-containers); }
_remove_nanocode_supervision() { CALLS+=(remove-supervision); }
remove_external_worker_config() { CALLS+=(remove-config); }
_archive_nanocode_env() { CALLS+=(archive-env); }
ports_retire_nanocode() { CALLS+=(retire-registry); }
_retire_tenant_nanocode fixture
assert_eq "retirement order stops before mutation" "${CALLS[*]}" \
  "stop-supervision stop-containers remove-supervision remove-config archive-env retire-registry"

CALLS=()
_stop_nanocode_containers() { CALLS+=(stop-containers); return 1; }
if _retire_tenant_nanocode fixture >/dev/null 2>&1; then
  fail "container stop failure allowed retirement"
else
  pass "container stop failure blocks retirement"
fi
assert_eq "stop failure prevents config and registry mutation" "${CALLS[*]}" \
  "stop-supervision stop-containers"

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all Nanocode retirement tests passed\n'
