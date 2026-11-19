#!/usr/bin/env bash
# Standalone test harness for the value-flag guards (WI#637).
#
# Before WI#637 a value flag without a value aborted with `$2: unbound
# variable` under `set -u`, and `--flag --next-flag` silently consumed the
# next flag as the value and exited 0 — e.g. `add-tenant probe --llm-api-key
# --with-kimi` wrote LLM_API_KEY=--with-kimi and left the worker disabled.
# `need_value "$@"` (mt-admin-lib/00-common.sh, plus the standalone copy in
# import-tenant.sh) closes both shapes.
#
# Sources lunarwing-mt-admin.sh (its BASH_SOURCE guard skips main()) and
# drives the real dispatcher with the root/lock/runtime edges stubbed, so the
# assertions cover the actual `case` arms rather than a copy of them.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
IMPORT_SCRIPT="$SCRIPT_DIR/../import-tenant.sh"

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
assert_contains() {  # <label> <haystack> <needle>
  if [[ "$2" == *"$3"* ]]; then
    echo "  PASS: $1"
  else
    echo "  FAIL: $1"
    echo "        expected to contain: [$3]"
    echo "        actual:              [$2]"
    failures=$((failures + 1))
  fi
}

# The dispatcher consults these before the parse loop; none may run here.
unset LUNARWING_SERVICE_MANAGER LUNARWING_CONTAINER_RUNTIME
# shellcheck disable=SC2317  # invoked indirectly by main()
require_root() { :; }
# shellcheck disable=SC2317
lw_lock_tree_init() { :; }
# shellcheck disable=SC2317
openrc_dispatch_preflight() { return 0; }

echo "=== need_value helper ==="

rc=0; out="$( need_value --some-flag 2>&1 )" || rc=$?
assert_eq "missing value rc" "$rc" "1"
assert_contains "missing value message" "$out" "--some-flag requires a value"

rc=0; out="$( need_value --some-flag --with-kimi 2>&1 )" || rc=$?
assert_eq "flag-as-value rc" "$rc" "1"
assert_contains "flag-as-value message" "$out" "requires a value"
assert_contains "flag-as-value names the swallowed flag" "$out" "--with-kimi"

rc=0; need_value --some-flag "value" || rc=$?
assert_eq "plain value accepted" "$rc" "0"

rc=0; need_value --some-flag "" || rc=$?
assert_eq "empty value still accepted" "$rc" "0"

rc=0; need_value --expires-in-days "-1" || rc=$?
assert_eq "single-dash value still accepted" "$rc" "0"

echo "=== dispatcher: missing trailing value dies with a usage-style error ==="
run_denied() {  # <label> <flag> <command...>
  local label="$1" flag="$2" out rc=0
  shift 2
  out="$( main "$@" "$flag" 2>&1 )" || rc=$?
  assert_eq "$label rc" "$rc" "1"
  assert_contains "$label message" "$out" "$flag requires a value"
}

run_denied "add-tenant --llm-api-key"        --llm-api-key        add-tenant probeA
run_denied "add-tenant --gateway-host"       --gateway-host       add-tenant probeA
run_denied "add-tenant --xmpp-password"      --xmpp-password      add-tenant probeA
run_denied "add-tenant --lunardepth-api-key-file" --lunardepth-api-key-file add-tenant probeA
run_denied "add-tenants --llm-api-key"       --llm-api-key        add-tenants probeA
run_denied "configure-lunardepth --api-key-file" --api-key-file   configure-lunardepth probeA
run_denied "configure-kimi --api-key-file"   --api-key-file       configure-kimi probeA
run_denied "configure-opencode --model"      --model              configure-opencode probeA
run_denied "configure-voice --stt-engine"    --stt-engine         configure-voice probeA

echo "=== dispatcher: a neighbouring flag is not swallowed as the value ==="
run_swallow_denied() {  # <label> <flag> <next-flag> <command...>
  local label="$1" flag="$2" next="$3" out rc=0
  shift 3
  out="$( main "$@" "$flag" "$next" 2>&1 )" || rc=$?
  assert_eq "$label rc" "$rc" "1"
  assert_contains "$label message" "$out" "$flag requires a value"
}

run_swallow_denied "add-tenant --llm-api-key --with-kimi" --llm-api-key --with-kimi add-tenant probeA
run_swallow_denied "add-tenant --gateway-host --with-kimi" --gateway-host --with-kimi add-tenant probeA
run_swallow_denied "add-tenant --xmpp-password --with-kimi" --xmpp-password --with-kimi add-tenant probeA
run_swallow_denied "add-tenant --lunardepth-model --with-lunardepth" --lunardepth-model --with-lunardepth add-tenant probeA
run_swallow_denied "configure-voice --stt-engine --disable-vad" --stt-engine --disable-vad configure-voice probeA
run_swallow_denied "build-worker-candidates --target --selected-commit" --target --selected-commit build-worker-candidates probeA

echo "=== dispatcher: valid values keep their arm (no regression) ==="
# Shellcheck cannot follow the dispatcher's positional contract; the indices
# are add_tenant's, and WI#637's census used the same ones (arg 8, arg 19).
# shellcheck disable=SC2317
add_tenant() { printf 'llm_api_key=%s\nwith_kimi=%s\n' "$8" "${19}"; }
# shellcheck disable=SC2317
require_tenant_llm_endpoint() { :; }
# shellcheck disable=SC2317
with_tenant_lifecycle_lock() { local _tenant="$1"; shift; "$@"; }
sanitize_name() { printf '%s' "$1"; }

out="$( main add-tenant probeA --llm-api-key sekrit --with-kimi 2>&1 )"
assert_contains "llm-api-key value reaches add_tenant" "$out" "llm_api_key=sekrit"
assert_contains "follow-on --with-kimi still applies" "$out" "with_kimi=true"

echo "=== structural: every flag arm with 'shift 2' carries need_value ==="
# Fails if a new value flag is added without the guard. The --in arm of
# darkirc-contact validate-migration is the one intentional exception: it
# rejects anything but a literal '-' itself.
unguarded_arms() {  # <file>
  awk '
    /^[[:space:]]*(--[a-z0-9-]+|-\\*|\\*)\\)/ { need = 0 }
    /need_value/                            { need = 1 }
    /shift 2/ {
      if (!need && $0 !~ /--in\\)/) printf "%s:%d: %s\n", FILENAME, FNR, $0
    }
  ' "$1"
}

for script in "$ADMIN_SCRIPT" "$IMPORT_SCRIPT"; do
  if bad="$(unguarded_arms "$script")" && [[ -z "$bad" ]]; then
    echo "  PASS: $(basename "$script") has no unguarded value arm"
  else
    echo "  FAIL: $(basename "$script") has unguarded value arms:"
    printf '%s\n' "$bad"
    failures=$((failures + 1))
  fi
done

echo ""
if [[ "$failures" -eq 0 ]]; then
  echo "ALL TESTS PASSED"
  exit 0
else
  echo "$failures TEST(S) FAILED"
  exit 1
fi
